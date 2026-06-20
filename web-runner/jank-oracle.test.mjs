// Validates the jank detector on synthetic trajectories (no browser needed):
// a smooth constant-velocity motion must score ~0; a stall-then-jump motion
// must score high. Run with `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { trajectoryJank } from './jank-oracle.mjs';

test('smooth constant-velocity motion scores near zero', () => {
  const smooth = Array.from({ length: 20 }, (_, i) => i * 5); // 0,5,10,...
  const j = trajectoryJank(smooth);
  assert.ok(j.score < 0.1, `expected smooth score < 0.1, got ${j.score}`);
  assert.strictEqual(j.stalls, 0);
  assert.strictEqual(j.jumps, 0);
});

test('stall-then-jump motion scores high', () => {
  // stall for two frames, then jump 30 — classic dropped-frame catch-up.
  const janky = [];
  for (let k = 0; k < 7; k++) {
    janky.push(k * 30, k * 30, k * 30);
  }
  const j = trajectoryJank(janky);
  assert.ok(j.score > 0.5, `expected janky score > 0.5, got ${j.score}`);
  assert.ok(j.stalls > 0 && j.jumps > 0);
});

test('non-moving element is not flagged', () => {
  const still = Array(20).fill(100);
  assert.strictEqual(trajectoryJank(still).score, 0);
});

test('detector is deterministic', () => {
  const xs = [0, 0, 12, 12, 24, 24, 36];
  assert.deepStrictEqual(trajectoryJank(xs), trajectoryJank(xs));
});
