/**
 * App-invariant oracle (self-triggered model). The native fuzzer drives the RN
 * app and cannot call the app's predicates, so the SDK evaluates its OWN
 * registered invariants on each settled state and, ONLY when it detects it is
 * under the fuzzer, logs a `REPROIT_INVARIANT` marker (which lands in
 * logcat/syslog) that runners/rn/runner.mjs scrapes into an EXPLORE:INVARIANT
 * line. These tests exercise the registry + evaluation + fuzz gate + marker
 * shape directly (no RN fiber needed), proving a VIOLATING invariant emits and a
 * CLEAN one is silent, both directions.
 */
// RN is a peer dependency and not installed in the node test env, so stub the
// modules the provider/context chain imports (same stubs as context.test.ts).
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

// Reach the internals under test without an RN runtime: force the engine active
// and invoke the same private hook `observe()` calls after each settle.
type Internal = {
  on: boolean;
  invariants: Array<{ id: string }>;
  checkInvariants: (sig: string) => void;
};
const impl = ReproIt as unknown as Internal;

function markers(spy: jest.SpyInstance): Array<{ sig: string; items: Array<{ id: string; message: string }> }> {
  return spy.mock.calls
    .map((c) => String(c[0] ?? ''))
    .filter((l) => l.startsWith('REPROIT_INVARIANT '))
    .map((l) => JSON.parse(l.slice('REPROIT_INVARIANT '.length)));
}

describe('ReproIt.invariant (RN self-triggered oracle)', () => {
  let logSpy: jest.SpyInstance;

  beforeEach(() => {
    impl.invariants = [];
    impl.on = true;
    (globalThis as { __reproit_fuzz?: unknown }).__reproit_fuzz = undefined;
    logSpy = jest.spyOn(console, 'log').mockImplementation(() => {});
  });

  afterEach(() => {
    logSpy.mockRestore();
    impl.invariants = [];
    impl.on = false;
    delete (globalThis as { __reproit_fuzz?: unknown }).__reproit_fuzz;
  });

  it('emits ONE marker listing only the violated invariants when under the fuzzer', () => {
    ReproIt.invariant('total-nonneg', () => true); // holds
    ReproIt.invariant('tab-selected', () => false); // violated, empty message
    ReproIt.invariant('cart-count', () => {
      throw new Error('cart went negative');
    }); // violated via throw
    ReproIt.invariant('balance', () => ({ ok: false, message: 'balance drifted' })); // {ok,message}

    (globalThis as { __reproit_fuzz?: unknown }).__reproit_fuzz = true;
    impl.checkInvariants('deadbeef');

    const found = markers(logSpy);
    expect(found).toHaveLength(1);
    expect(found[0].sig).toBe(''); // runner substitutes the current sig
    const byId = Object.fromEntries(found[0].items.map((i) => [i.id, i.message]));
    expect(Object.keys(byId).sort()).toEqual(['balance', 'cart-count', 'tab-selected']);
    expect(byId['tab-selected']).toBe('');
    expect(byId['cart-count']).toBe('cart went negative');
    expect(byId['balance']).toBe('balance drifted');
    expect(byId['total-nonneg']).toBeUndefined(); // the held one never appears
  });

  it('is SILENT when every invariant holds (clean state, no marker)', () => {
    ReproIt.invariant('a', () => true);
    ReproIt.invariant('b', () => ({ ok: true }));
    (globalThis as { __reproit_fuzz?: unknown }).__reproit_fuzz = true;
    impl.checkInvariants('cafebabe');
    expect(markers(logSpy)).toHaveLength(0);
  });

  it('is INERT in production: a violation is never evaluated without the fuzzer gate', () => {
    ReproIt.invariant('boom', () => false);
    // no __reproit_fuzz set
    impl.checkInvariants('cafed00d');
    expect(markers(logSpy)).toHaveLength(0);
  });

  it('honors process.env.REPROIT_FUZZ as an alternate gate', () => {
    ReproIt.invariant('boom', () => false);
    const prev = process.env.REPROIT_FUZZ;
    process.env.REPROIT_FUZZ = '1';
    try {
      impl.checkInvariants('feedface');
    } finally {
      if (prev === undefined) delete process.env.REPROIT_FUZZ;
      else process.env.REPROIT_FUZZ = prev;
    }
    expect(markers(logSpy)).toHaveLength(1);
  });

  it('registration is idempotent by id (re-register replaces, no duplicate)', () => {
    ReproIt.invariant('x', () => true);
    ReproIt.invariant('x', () => false); // replaces the holding predicate
    expect(impl.invariants).toHaveLength(1);
    (globalThis as { __reproit_fuzz?: unknown }).__reproit_fuzz = true;
    impl.checkInvariants('0badf00d');
    const found = markers(logSpy);
    expect(found).toHaveLength(1);
    expect(found[0].items.map((i) => i.id)).toEqual(['x']);
  });
});
