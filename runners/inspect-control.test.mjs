import assert from 'node:assert/strict';
import test from 'node:test';

import { boundedWaitMs } from './inspect-control.mjs';

test('platform inspection wait is bounded', () => {
  assert.equal(boundedWaitMs('1'), 1_000);
  assert.equal(boundedWaitMs('5000'), 5_000);
  assert.equal(boundedWaitMs('9999999'), 900_000);
  assert.equal(boundedWaitMs('nope'), 240_000);
});
