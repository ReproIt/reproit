jest.mock('react-native', () => ({ NativeModules: { ReproItRuntime: {
  capsuleJson: JSON.stringify({ exchanges: [{ id: 'native' }] }),
} } }));

import { installCausalFetch, nativeCausalCapsule, redactCausal, type CausalExchange } from '../src/causal';

describe('causal fetch transport', () => {
  const original = globalThis.fetch;
  const originalXhr = globalThis.XMLHttpRequest;
  afterEach(() => { globalThis.fetch = original; globalThis.XMLHttpRequest = originalXhr; });

  test('redacts before emitting the universal exchange marker', async () => {
    const lines: string[] = [];
    globalThis.fetch = jest.fn(async () => new Response(
      JSON.stringify({ profile: { email: 'a@example.com' }, author: null }),
      { status: 200, headers: { 'content-type': 'application/json' } },
    )) as typeof fetch;
    const uninstall = installCausalFetch({ actionIndex: () => 1, emit: (line) => lines.push(line) });
    await globalThis.fetch!('https://app.test/feed', {
      method: 'POST', headers: { authorization: 'Bearer raw', 'content-type': 'application/json' },
      body: JSON.stringify({ token: 'raw', query: 'ok' }),
    });
    uninstall();
    const marker = lines.find((line) => line.startsWith('REPROIT:EXCHANGE '));
    expect(marker).toBeDefined();
    const exchange = JSON.parse(marker!.slice('REPROIT:EXCHANGE '.length));
    expect(exchange.requestHeaders.authorization).toBe('<reproit:secret>');
    expect(exchange.requestBody.token).toBe('<reproit:string:length=3>');
    expect(exchange.responseBody.profile.email).toBe('<reproit:string:length=13>');
  });

  test('replay is exact and fail closed', async () => {
    const exchange: CausalExchange = {
      id: 'a-0-0', actor: 'a', actionIndex: 0, ordinal: 0, protocol: 'https',
      method: 'GET', url: 'https://app.test/config?a=1&b=2', requestHeaders: {}, status: 200,
      responseHeaders: { 'content-type': 'application/json' }, responseBody: { enabled: true }, required: true,
    };
    globalThis.fetch = jest.fn(async () => { throw new Error('live network must not run'); }) as typeof fetch;
    installCausalFetch({ actionIndex: () => 0, capsule: { exchanges: [exchange] }, emit: () => {} });
    expect(await (await globalThis.fetch!('https://app.test/config?b=2&a=1')).json()).toEqual({ enabled: true });
    await expect(globalThis.fetch!('https://app.test/other')).rejects.toThrow('CAPSULE:MISS');
  });

  test('redaction is structural and deterministic', () => {
    const redacted = redactCausal({ phone: '123', apiKey: 'raw-api', 'publishable-key': 'raw-pub', private_key: 'raw-private',
      'access.key': 'raw-access', 'signing key': 'raw-signing', keyboardLayout: 'dvorak', key: 'ordinary', nested: { ok: 1 } });
    expect(redacted).toEqual({
      'access.key': '<reproit:string:length=10>', apiKey: '<reproit:string:length=7>', key: 'ordinary', keyboardLayout: 'dvorak', nested: { ok: 1 },
      phone: '<reproit:string:length=3>', private_key: '<reproit:string:length=11>', 'publishable-key': '<reproit:string:length=7>', 'signing key': '<reproit:string:length=11>',
    });
    expect(JSON.stringify(redacted)).not.toMatch(/raw-(api|pub|private|access|signing)/);
  });

  test('loads the Appium-injected capsule through the autolinked native module', () => {
    expect(nativeCausalCapsule()?.exchanges?.[0].id).toBe('native');
  });

  test('direct XMLHttpRequest is routed through fail-closed replay', async () => {
    globalThis.XMLHttpRequest = class {} as typeof XMLHttpRequest;
    globalThis.fetch = jest.fn(async () => { throw new Error('live network must not run'); }) as typeof fetch;
    const exchange: CausalExchange = { id: 'a-0-0', actor: 'a', actionIndex: 0, ordinal: 0, protocol: 'https',
      method: 'GET', url: 'https://app.test/x?a=1&b=2', requestHeaders: {}, status: 200,
      responseHeaders: { 'content-type': 'application/json' }, responseBody: { ok: true }, required: true };
    installCausalFetch({ actionIndex: () => 0, capsule: { exchanges: [exchange] }, emit: () => {} });
    const xhr = new XMLHttpRequest(); xhr.responseType = 'json';
    const done = new Promise<void>((resolve, reject) => { xhr.onload = () => resolve(); xhr.onerror = () => reject(new Error('XHR replay failed')); });
    xhr.open('GET', 'https://app.test/x?b=2&a=1'); xhr.send(); await done;
    expect(xhr.status).toBe(200); expect(xhr.response).toEqual({ ok: true });
  });
});
