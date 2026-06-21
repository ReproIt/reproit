// Validates the Tier-2 flicker detector WITHOUT a browser: the transient-
// divergence scoring is host-pure (it operates on per-frame distance numbers),
// so we feed synthetic distance sequences and assert the verdict. Run with
// `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { transientDivergence } from './flicker-oracle.mjs';

test('a monotonic convergence is not a flicker', () => {
  // Old screen -> new screen: distance to the final frame only decreases.
  assert.strictEqual(transientDivergence([0.6, 0.4, 0.2, 0.05, 0]), null);
});

test('a large but monotonic change is not a flicker', () => {
  // The transition changes almost the whole screen, but never overshoots.
  assert.strictEqual(transientDivergence([0.9, 0.7, 0.4, 0.1, 0]), null);
});

test('an intermediate frame further from final than the start is a flicker', () => {
  // Frame 0 is 0.3 from final; a middle frame jumps to 0.8 (a blank/garbled
  // flash) before settling to 0. That overshoot is the flicker.
  const r = transientDivergence([0.3, 0.8, 0.2, 0]);
  assert.ok(r, 'expected a flicker finding');
  assert.strictEqual(r.at, 1);
  assert.strictEqual(r.peak, 0.8);
  assert.strictEqual(r.frames, 4);
});

test('a sub-floor blip is ignored as noise', () => {
  // A tiny overshoot (0.03) below the noise floor never trips.
  assert.strictEqual(transientDivergence([0.01, 0.03, 0.005, 0]), null);
});

test('the start frame is the baseline, not the floor', () => {
  // Starting already near-identical (0.02), a 0.06 middle frame overshoots it by
  // well over the factor AND clears the floor -> flicker.
  const r = transientDivergence([0.02, 0.06, 0.01, 0]);
  assert.ok(r);
  assert.strictEqual(r.peak, 0.06);
});

test('too few frames cannot be judged', () => {
  assert.strictEqual(transientDivergence([0.5, 0]), null);
  assert.strictEqual(transientDivergence([]), null);
});

test('thresholds are tunable', () => {
  // With a stricter factor the same sequence is no longer a flicker.
  assert.ok(transientDivergence([0.3, 0.5, 0.1, 0]));
  assert.strictEqual(transientDivergence([0.3, 0.5, 0.1, 0], { factor: 2.0 }), null);
});
