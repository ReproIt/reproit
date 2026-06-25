/**
 * Developer-provided BUILD identity test.
 *
 * RN can't auto-detect the app build version / git commit without a native
 * module, so the developer supplies them via `init({ build })`. They ride every
 * event's context as `context.build = { version, commit }` (only the provided
 * fields). The cloud's bucketing reads
 * `context.build.version`/`.commit` to segment bugs by build ("regressed in
 * 1.4.2 / no hits since 1.4.5"). Back-compat: no `build` -> no `build` key.
 */

// RN is a peer dependency and not installed in the node test env, so stub the
// `Platform` module the context collector reads (same stub as context.test.ts).
jest.mock(
  'react-native',
  () => ({ Platform: { OS: 'ios', Version: '17.4' }, View: 'View' }),
  { virtual: true }
);
jest.mock(
  'react',
  () => ({ useEffect: () => {}, useCallback: (f: unknown) => f, createElement: () => null }),
  { virtual: true }
);

import { ReproIt } from '../src/index';

afterEach(() => ReproIt.dispose());

/** Post one snapshot, flush, and return the JSON batch body the SDK POSTed. */
function flushOneBatch(opts: Parameters<typeof ReproIt.init>[0]): {
  ctx?: Record<string, unknown>;
} {
  const fetchMock = jest.fn(() => Promise.resolve({} as Response));
  (globalThis as { fetch?: typeof fetch }).fetch = fetchMock as unknown as typeof fetch;
  ReproIt.init({ ...opts, endpoint: 'https://ingest.example' });
  ReproIt.recordSnapshot(
    { role: 'screen', children: [{ role: 'header', id: 'title' }] },
    'load'
  );
  ReproIt.flush();
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

  test('the batch posted to /v1/events carries ctx.build = { version, commit }', () => {
    const body = flushOneBatch({ appId: 'b', build: { version: '1.4.2', commit: 'abc123' } });
    expect(body.ctx).toBeDefined();
    expect(body.ctx!.build).toEqual({ version: '1.4.2', commit: 'abc123' });
    // The auto dimensions still ride alongside it.
    expect(body.ctx!.platform).toBe('ios');
  });

  test('init WITHOUT build -> no build key (back-compat, today\'s behavior)', () => {
    const body = flushOneBatch({ appId: 'b' });
    expect(body.ctx).toBeDefined(); // auto dimensions still present
    expect('build' in (body.ctx as object)).toBe(false);
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
