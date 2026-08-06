jest.mock('react-native', () => ({ NativeModules: {} }));

import { ReproIt } from '../src/index';

type Posted = { url: string; body: Record<string, unknown> };

function stubTransport(posted: Posted[]): void {
  globalThis.fetch = jest.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : String(input);
    if (url.includes('/v1/')) {
      posted.push({ url, body: JSON.parse(String(init?.body ?? '{}')) });
      return new Response('{}', { status: 200 });
    }
    // A dependency call the app makes; this is what capture must record.
    return new Response(JSON.stringify({ prices: null, token: 'raw-secret' }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  }) as typeof fetch;
}

function telemetryContainsFinding(posted: Posted[]): boolean {
  return posted
    .filter((entry) => entry.url.endsWith('/v1/events'))
    .some((entry) => {
      const frames = entry.body.frames;
      return Array.isArray(frames) && frames.some((frame) => {
        if (!frame || typeof frame !== 'object') return false;
        const event = (frame as { event?: { kind?: string } }).event;
        return event?.kind === 'finding';
      });
    });
}

describe('production capture gate', () => {
  const original = globalThis.fetch;
  afterEach(() => {
    ReproIt.dispose();
    globalThis.fetch = original;
  });

  test('a UI-only failure ships one capture batch by default', async () => {
    const posted: Posted[] = [];
    stubTransport(posted);
    ReproIt.init({ appId: 'app-demo', endpoint: 'https://ingest.test' });
    await globalThis.fetch!('https://api.test/prices');
    ReproIt.recordSnapshot({ role: 'screen', children: [{ role: 'button', id: 'quote' }] }, 'load');
    expect(ReproIt.captureBug()).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(posted.some((entry) => entry.url.endsWith('/v1/capture-batches'))).toBe(true);
    expect(telemetryContainsFinding(posted)).toBe(false);
  });

  test('opted in: a failure ships a capture batch carrying the recorded response', async () => {
    const posted: Posted[] = [];
    stubTransport(posted);
    ReproIt.init({
      appId: 'app-demo',
      endpoint: 'https://ingest.test',
      apiKey: 'sk_live_test',
      build: { version: '1.4.2', commit: 'abc123' },
      captureExchanges: true,
    });
    await globalThis.fetch!('https://api.test/prices?tier=gold');
    ReproIt.recordSnapshot({ role: 'screen', children: [{ role: 'button', id: 'quote' }] }, 'load');
    expect(ReproIt.captureBug()).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 10));

    const capture = posted.find((entry) => entry.url.endsWith('/v1/capture-batches'));
    expect(capture).toBeDefined();
    const batch = capture!.body as {
      version: number;
      projectId: string;
      deployment: Record<string, string>;
      capabilities: Array<{ capability: string }>;
      events: Array<{ event: Record<string, unknown> }>;
    };
    expect(batch.version).toBe(1);
    expect(batch.projectId).toBe('app-demo');
    expect(batch.deployment).toEqual({ version: '1.4.2', commit: 'abc123' });
    expect(batch.capabilities.map((entry) => entry.capability)).toContain('network');

    const dependency = batch.events.find((event) => event.event.kind === 'dependency');
    const value = dependency?.event.value as { value: { exchange: Record<string, never> } };
    const exchange = value.value.exchange as unknown as {
      request: { url: string };
      response: { status: number; body: Record<string, unknown> };
    };
    expect(exchange.request.url).toBe('https://api.test/prices?tier=gold');
    expect(exchange.response.status).toBe(200);
    expect(exchange.response.body.prices).toBeNull();
    // Secret-named response fields are placeholders before the batch leaves.
    expect(exchange.response.body.token).toEqual({
      $reproit: { redacted: true, type: 'string', length: 10 },
    });
    expect(JSON.stringify(batch)).not.toContain('raw-secret');
    expect(telemetryContainsFinding(posted)).toBe(false);
  });

  test('an operation with no dependency exchange still sends a capture batch', async () => {
    const posted: Posted[] = [];
    stubTransport(posted);
    ReproIt.init({
      appId: 'app-demo',
      endpoint: 'https://ingest.test',
      captureExchanges: true,
    });
    ReproIt.recordSnapshot({ role: 'screen', children: [{ role: 'button', id: 'quote' }] }, 'load');
    expect(ReproIt.captureBug()).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 10));
    const capture = posted.find((entry) => entry.url.endsWith('/v1/capture-batches'));
    expect(capture).toBeDefined();
    const batch = capture!.body as { capabilities: Array<{ capability: string }> };
    expect(batch.capabilities.map((entry) => entry.capability)).not.toContain('network');
    expect(telemetryContainsFinding(posted)).toBe(false);
  });
});
