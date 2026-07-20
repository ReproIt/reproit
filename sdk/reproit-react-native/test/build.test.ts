/**
 * Developer-provided BUILD identity test.
 *
 * RN can't auto-detect the app build version / git commit without a native
 * module, so the developer supplies them via `init({ build })`. They ride every
 * event's context as `context.build = { version, commit }` (only the provided
 * fields). The cloud's bucketing reads
 * `context.build.version`/`.commit` to segment bugs by build ("regressed in
 * 1.4.2 / no hits since 1.4.5"). No `build` config means no `build` key.
 */

// RN is a peer dependency and not installed in the node test env, so stub the
// `Platform` module the context collector reads (same stub as context.test.ts).
jest.mock('react-native', () => ({ Platform: { OS: 'ios', Version: '17.4' }, View: 'View' }), {
  virtual: true,
});
jest.mock(
  'react',
  () => ({ useEffect: () => {}, useCallback: (f: unknown) => f, createElement: () => null }),
  { virtual: true },
);

import { ReproIt } from '../src/index';

afterEach(() => ReproIt.dispose());

/** Post one snapshot, flush, and return the JSON batch body the SDK POSTed. */
function flushOneBatch(opts: Parameters<typeof ReproIt.init>[0]): {
  version: number;
  frames: Array<{ event: { kind: string; context?: Record<string, unknown> } }>;
} {
  const fetchMock = jest.fn(() => Promise.resolve({} as Response));
  (globalThis as { fetch?: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
  ReproIt.init({ ...opts, endpoint: 'https://ingest.example' });
  ReproIt.recordSnapshot({ role: 'screen', children: [{ role: 'header', id: 'title' }] }, 'load');
  ReproIt.captureBug();
  const [, fetchOpts] = fetchMock.mock.calls[0] as unknown as [string, { body: string }];
  delete (globalThis as { fetch?: typeof fetch }).fetch;
  return JSON.parse(fetchOpts.body);
}

describe('developer-provided build identity (context.build)', () => {
  test('init WITH build -> events carry context.build.{version,commit}', () => {
    ReproIt.init({ appId: 'b', onEvent: () => {}, build: { version: '1.4.2', commit: 'abc123' } });
    const ctx = ReproIt.context();
    expect(ctx.build).toEqual({ version: '1.4.2', commit: 'abc123' });
  });

  test('the finding frame carries context.build = { version, commit }', () => {
    const body = flushOneBatch({ appId: 'b', build: { version: '1.4.2', commit: 'abc123' } });
    expect(body.version).toBe(1);
    const finding = body.frames.find((frame) => frame.event.kind === 'finding');
    expect(finding?.event.context?.build).toEqual({ version: '1.4.2', commit: 'abc123' });
    // The auto dimensions still ride alongside it.
    expect(finding?.event.context?.platform).toBe('ios');
  });

  test('init WITHOUT build -> no build key', () => {
    const body = flushOneBatch({ appId: 'b' });
    const finding = body.frames.find((frame) => frame.event.kind === 'finding');
    expect(finding?.event.context).toBeDefined();
    expect('build' in (finding?.event.context ?? {})).toBe(false);
    expect(ReproIt.context().build).toBeUndefined();
  });

  test('only-version -> context.build has version, no commit', () => {
    ReproIt.init({ appId: 'b', onEvent: () => {}, build: { version: '9.9.9' } });
    expect(ReproIt.context().build).toEqual({ version: '9.9.9' });
  });

  test('only-commit -> context.build has commit, no version', () => {
    ReproIt.init({ appId: 'b', onEvent: () => {}, build: { commit: 'deadbeef' } });
    expect(ReproIt.context().build).toEqual({ commit: 'deadbeef' });
  });

  test('empty / blank build fields are dropped (no build key)', () => {
    ReproIt.init({ appId: 'b', onEvent: () => {}, build: { version: '', commit: '' } });
    expect(ReproIt.context().build).toBeUndefined();
  });
});

test('captureBug emits a structurally identified tester capture', () => {
  const events: Array<Record<string, unknown>> = [];
  ReproIt.init({
    appId: 'b',
    onEvent: (event) => events.push(event as unknown as Record<string, unknown>),
  });
  ReproIt.recordSnapshot(
    { role: 'screen', children: [{ role: 'button', id: 'checkout' }] },
    'load',
  );
  expect(ReproIt.captureBug()).toBe(true);
  const event = events.find((item) => item.oracle === 'tester-capture');
  expect(event).toBeDefined();
  expect((event!.findingIdentity as Record<string, unknown>).boundary).toBe(event!.sig);
  expect((event!.findingIdentity as Record<string, unknown>).invariant).toBe(
    'tester-observed-failure',
  );
});

test('production contract capture keeps its exact invariant identity', () => {
  const events: Array<Record<string, unknown>> = [];
  ReproIt.init({
    appId: 'b',
    onEvent: (event) => events.push(event as unknown as Record<string, unknown>),
  });
  ReproIt.recordSnapshot(
    { role: 'screen', children: [{ role: 'button', id: 'checkout' }] },
    'load',
  );
  let state = 'present';
  ReproIt.preserveState('draft', {
    boundaries: ['rotation'],
    sample: () => ({
      key: 'checkout',
      state,
      authoritative: true,
      settled: true,
    }),
  });
  ReproIt.stateBoundary('rotation', 'before');
  state = 'empty';
  ReproIt.stateBoundary('rotation', 'after');
  const event = events.find((item) => item.oracle === 'invariant');
  expect(event).toBeDefined();
  expect((event!.findingIdentity as Record<string, unknown>).invariant).toBe(
    'state-preservation:rotation:draft',
  );
  expect((event!.findingIdentity as Record<string, unknown>).boundary).toBe(event!.sig);
});
