import {
  ActionEffectOracle,
  StatePreservationOracle,
  contractMarker,
} from '../src/structural-contracts';

test('proves state loss only across an explicit authoritative boundary', () => {
  let state = 'draft:present';
  const oracle = new StatePreservationOracle();
  oracle.register('checkout-draft', {
    boundaries: ['rotation'],
    sample: () => ({
      key: 'checkout',
      state,
      authoritative: true,
      settled: true,
    }),
  });
  expect(oracle.boundary('rotation', 'before')[0].status).toBe('VALID');
  state = 'draft:empty';
  const results = oracle.boundary('rotation', 'after');
  expect(results[0]).toMatchObject({
    status: 'PROVEN',
    id: 'state-preservation:rotation:checkout-draft',
  });
  expect(contractMarker(results)).toContain('state-preservation:rotation:checkout-draft');
});

test('state preservation abstains without authoritative settled samples', () => {
  const oracle = new StatePreservationOracle();
  oracle.register('x', {
    boundaries: ['background-foreground'],
    sample: () => ({
      key: 'x',
      state: 'a',
      authoritative: false,
      settled: true,
    }),
  });
  expect(oracle.boundary('background-foreground', 'before')[0].status).toBe('UNKNOWN');
  expect(oracle.boundary('background-foreground', 'after')[0].status).toBe('UNKNOWN');
});

test('process recreation requires explicit persistent baseline callbacks', () => {
  let state = 'draft:present';
  let persisted: { key: string; state: string; authoritative: boolean; settled: boolean } | null =
    null;
  const oracle = new StatePreservationOracle();
  oracle.register('draft', {
    boundaries: ['process-recreation'],
    sample: () => ({
      key: 'checkout',
      state,
      authoritative: true,
      settled: true,
    }),
    saveBaseline: (_kind, value) => {
      persisted = value;
      return true;
    },
    loadBaseline: () => persisted,
  });
  expect(oracle.boundary('process-recreation', 'before')[0].status).toBe('VALID');
  state = 'draft:empty';
  expect(oracle.boundary('process-recreation', 'after')[0].status).toBe('PROVEN');
});

test('action effects prove exact declared effects without labels', () => {
  let observation = { route: 'cart', state: 'idle', authoritative: true, settled: true };
  const oracle = new ActionEffectOracle();
  oracle.register('checkout', {
    sample: () => observation,
    route: { target: 'receipt' },
    state: { target: 'complete' },
  });
  oracle.begin('checkout');
  observation = { ...observation, route: 'cart', state: 'complete' };
  const results = oracle.end('checkout');
  expect(results.filter((r) => r.status === 'PROVEN').map((r) => r.id)).toEqual([
    'action-effect:checkout:route',
  ]);
});

test('action effects abstain when the platform observation is not ' + 'authoritative', () => {
  const oracle = new ActionEffectOracle();
  oracle.register('x', {
    sample: () => ({ authoritative: false, settled: true }),
    route: { target: 'x' },
  });
  expect(oracle.begin('x')[0].status).toBe('UNKNOWN');
  expect(oracle.end('x')[0].status).toBe('UNKNOWN');
});
