// Validates the universal framebuffer-probe floor (PIECE 2) WITHOUT a browser:
// the grid math, the pixel-diff, and the region classification are host-pure, so
// we feed synthetic RGBA buffers + synthetic probe points and assert the gaps.
// Run with `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import {
  gridPoints,
  changedFraction,
  classifyPoint,
  probeRegionsToGroundtruth,
  DEFAULT_CHANGE_FRACTION,
  PIXEL_DELTA,
} from './probe.mjs';

test('gridPoints is bounded, deterministic, and inset from edges', () => {
  const pts = gridPoints(1000, 800, { cols: 4, rows: 3, inset: 0.1 });
  assert.strictEqual(pts.length, 12, '4x3 grid = 12 points');
  // Inset 0.1 -> first point at (100, 80), never at the very edge.
  assert.deepStrictEqual(pts[0], { x: 100, y: 80 });
  assert.ok(pts.every((p) => p.x >= 100 && p.x <= 900 && p.y >= 80 && p.y <= 720));
  // Deterministic: same input -> same output.
  assert.deepStrictEqual(pts, gridPoints(1000, 800, { cols: 4, rows: 3, inset: 0.1 }));
  // Degenerate sizes don't throw.
  assert.deepStrictEqual(gridPoints(0, 0), []);
});

// Build a flat RGBA buffer of `n` pixels, all the given [r,g,b].
function rgba(n, r, g, b) {
  const buf = new Uint8ClampedArray(n * 4);
  for (let i = 0; i < n; i++) {
    buf[i * 4] = r;
    buf[i * 4 + 1] = g;
    buf[i * 4 + 2] = b;
    buf[i * 4 + 3] = 255;
  }
  return buf;
}

test('changedFraction is ~0 for identical frames and high for a repaint', () => {
  const a = rgba(1000, 10, 20, 30);
  assert.strictEqual(changedFraction(a, a), 0, 'identical frames -> no change');
  // Flip every pixel well past PIXEL_DELTA.
  const b = rgba(1000, 200, 200, 200);
  assert.strictEqual(changedFraction(a, b), 1, 'full repaint -> all changed');
  // Sub-threshold noise (delta below PIXEL_DELTA) is ignored.
  const noisy = rgba(1000, 10 + PIXEL_DELTA - 1, 20, 30);
  assert.strictEqual(changedFraction(a, noisy), 0, 'sub-threshold noise is not a change');
});

test('classifyPoint flags only operable + a11y-absent points', () => {
  const big = DEFAULT_CHANGE_FRACTION * 2;
  const tiny = DEFAULT_CHANGE_FRACTION / 2;
  // Pixels changed AND no a11y node -> the gap.
  assert.strictEqual(classifyPoint(big, false), 'gap');
  // Pixels changed but an a11y node covers it -> already in graph 2.
  assert.strictEqual(classifyPoint(big, true), 'covered');
  // No pixel change -> not operable.
  assert.strictEqual(classifyPoint(tiny, false), 'inert');
  assert.strictEqual(classifyPoint(tiny, true), 'inert');
});

test('the canvas hit-area case: an operable region with no control is a gap', () => {
  // A WebGL/canvas button: clicking (300,200) repaints heavily, but there is no
  // DOM/a11y node there -> the floor reports it where the a11y walk is blind.
  const points = [
    { x: 300, y: 200, changed: 0.5, a11yCovered: false }, // the canvas button -> gap
    { x: 50, y: 50, changed: 0.6, a11yCovered: true }, // a real <button> -> covered, not a gap
    { x: 700, y: 400, changed: 0.0001, a11yCovered: false }, // dead background -> inert
  ];
  const els = probeRegionsToGroundtruth(points);
  assert.strictEqual(els.length, 1, 'only the uncovered operable region is a gap');
  const el = els[0];
  assert.strictEqual(el.id, 'probe:@300,200', 'addressed by spatial selector');
  assert.strictEqual(el.operable, true);
  assert.strictEqual(el.a11y.rolePresent, false, 'the floor signal: pixels react, AT sees nothing');
  assert.strictEqual(el.a11y.namePresent, false);
  assert.strictEqual(el.gestureKind, 'probe');
});

test('probe output is deterministic and sorted by selector', () => {
  const points = [
    { x: 900, y: 100, changed: 0.5, a11yCovered: false },
    { x: 100, y: 100, changed: 0.5, a11yCovered: false },
  ];
  const a = probeRegionsToGroundtruth(points);
  const b = probeRegionsToGroundtruth(points);
  assert.deepStrictEqual(a, b);
  assert.deepStrictEqual(
    a.map((e) => e.id),
    ['probe:@100,100', 'probe:@900,100'],
  );
});
