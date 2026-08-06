/**
 * Context API parity test. Mirrors the Flutter SDK's `test/context_test.dart`:
 * verifies the tier-1 auto dimensions, the hashed (never-raw) `uid` from
 * identify(), and that setContext/setContexts merge, the inputs the cloud's
 * ingest endpoint (`POST /v1/events`) folds into each event's context to compute
 * a cohort discriminator.
 */

// RN is a peer dependency and not installed in the node test env, so stub the
// `Platform` module the context collector reads. (The signature parity test
// deliberately touches only pure modules and needs no RN.)
jest.mock('react-native', () => ({ Platform: { OS: 'ios', Version: '17.4' }, View: 'View' }), {
  virtual: true,
});
// `react` is likewise a peer dep, not installed in this pure-JS test env. The
// provider only references React at call-time, so an import-time stub suffices.
jest.mock(
  'react',
  () => ({ useEffect: () => {}, useCallback: (f: unknown) => f, createElement: () => null }),
  { virtual: true },
);

import { ReproIt } from '../src/index';
import { autoContext, hashUid, sha256Hex } from '../src/context';

afterEach(() => ReproIt.dispose());

describe('autoContext, tier-1 auto dimensions (zero-PII)', () => {
  test('collects platform, locale, tz, release', () => {
    const ctx = autoContext();
    expect(ctx.platform).toBe('ios');
    expect(ctx.osVersion).toBe('17.4');
    // locale + tz come from the JS Intl API (present in Node / RN-with-intl).
    expect(typeof ctx.locale).toBe('string');
    expect(typeof ctx.tz).toBe('string');
    // release flag: boolean derived from !__DEV__ (true when __DEV__ undefined).
    expect(typeof ctx.release).toBe('boolean');
  });
});

describe('sha256 / hashUid', () => {
  test('sha256 matches the known RFC vector', () => {
    expect(sha256Hex('abc')).toBe(
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    );
    expect(sha256Hex('')).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
  });

  test('hashUid is 16 lowercase hex chars and stable', () => {
    const a = hashUid('user@example.com');
    const b = hashUid('user@example.com');
    expect(a).toMatch(/^[0-9a-f]{16}$/);
    expect(a).toBe(b);
    // Byte-identical to Flutter: first 16 chars of the full SHA-256.
    expect(a).toBe(sha256Hex('user@example.com').slice(0, 16));
  });
});

describe('ReproIt context API (mirrors Flutter)', () => {
  test('init seeds auto dimensions into the batch context', () => {
    ReproIt.init({ appId: 'ctx', onEvent: () => {} });
    const ctx = ReproIt.context();
    expect(ctx).toHaveProperty('platform');
    expect(ctx).toHaveProperty('locale');
    expect(ctx).toHaveProperty('tz');
    expect(typeof ctx.release).toBe('boolean');
  });

  test('identify hashes the raw id and merges context', () => {
    ReproIt.init({ appId: 'ctx', onEvent: () => {} });
    ReproIt.identify('user@example.com', { role: 'admin' });
    const ctx = ReproIt.context();
    const uid = ctx.uid as string;
    expect(uid).toMatch(/^[0-9a-f]{16}$/);
    expect(uid).not.toContain('user'); // never the raw value
    expect(uid).toBe(hashUid('user@example.com'));
    expect(ctx.role).toBe('admin');
  });

  test('setContext / setContexts merge further dimensions', () => {
    ReproIt.init({ appId: 'ctx', onEvent: () => {} });
    ReproIt.setContext('plan', 'free');
    ReproIt.setContexts({ region: 'eu', seats: 3 });
    const ctx = ReproIt.context();
    expect(ctx.plan).toBe('free');
    expect(ctx.region).toBe('eu');
    expect(ctx.seats).toBe(3);
  });

  test('failure capture envelopes carry context without duplicate finding telemetry', () => {
    const batches: unknown[] = [];
    const fetchMock = jest.fn(() => Promise.resolve({} as Response));
    (globalThis as { fetch?: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;

    ReproIt.init({ appId: 'ctx', endpoint: 'https://ingest.example' });
    ReproIt.identify('u1');
    // Manually contribute a snapshot so there's an event to flush.
    ReproIt.recordSnapshot(
      {
        role: 'screen',
        children: [
          { role: 'header', id: 'title' },
          { role: 'button', id: 'settings' },
        ],
      },
      'load',
    );
    ReproIt.captureBug();

    const calls = fetchMock.mock.calls as unknown as Array<[string, { body: string }]>;
    const captureCall = calls.find(([url]) =>
      String(url).endsWith('/v1/capture-batches'),
    );
    expect(captureCall).toBeDefined();
    const [, opts] = captureCall as unknown as [string, { body: string }];
    const body = JSON.parse(opts.body);
    batches.push(body);
    expect(body.version).toBe(1);
    const checkpoint = body.events.find(
      (captured: { event: { kind: string; name?: string } }) =>
        captured.event.kind === 'checkpoint' && captured.event.name === 'determinism-envelope',
    );
    expect(checkpoint.event.attributes.context.uid).toBe(hashUid('u1'));
    expect(checkpoint.event.attributes.context.platform).toBe('ios');
    for (const [url, request] of fetchMock.mock.calls as unknown as Array<
      [string, { body: string }]
    >) {
      if (!url.endsWith('/v1/events')) continue;
      const telemetry = JSON.parse(request.body);
      expect(telemetry.frames.some((frame: { event: { kind: string } }) =>
        frame.event.kind === 'finding')).toBe(false);
    }

    delete (globalThis as { fetch?: typeof fetch }).fetch;
  });
});
