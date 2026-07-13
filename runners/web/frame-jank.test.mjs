// Validates the cross-engine rAF frame-interval jank/hang classifier (the
// firefox/webkit path, where the chromium-only Long Tasks API is unavailable).
// The classifier is the deterministic core of the cross-engine watchdog; these
// tests pin its FALSE-POSITIVE-FREE behavior on synthetic interval lists (no
// browser needed). Run with `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { classifyFrameIntervals } from './runner.mjs';

// A clean 60fps stretch: every frame near the vsync cadence. No verdict.
test('smooth 60fps frames are clean (no false positive)', () => {
  const smooth = Array.from({ length: 60 }, () => 16 + (Math.random() < 0.5 ? 0 : 1));
  assert.strictEqual(classifyFrameIntervals(smooth), null);
});

// Headless / throttled cadence (~30fps, ~33ms) is still clean.
test('throttled 30fps frames are clean', () => {
  const throttled = Array.from({ length: 40 }, () => 33);
  assert.strictEqual(classifyFrameIntervals(throttled), null);
});

// A LONE mid-range late frame (a GC pause, a scheduling blip): one ~120-250ms
// frame surrounded by clean frames must NOT be flagged. This is the core
// false-positive guard for the noisier rAF signal.
test('a single GC-blip frame is not jank', () => {
  for (const blip of [120, 180, 240, 300, 349]) {
    const xs = [16, 16, 16, blip, 16, 16, 16];
    assert.strictEqual(
      classifyFrameIntervals(xs),
      null,
      `a lone ${blip}ms frame must be below the lone-jank floor`,
    );
  }
});

// The jank fixture: a 600ms synchronous stall shows up as one ~600ms frame,
// which clears the lone-jank floor (350ms) -> JANK at the JANK_FLOOR_MS bucket.
test('a lone 600ms stall is jank', () => {
  const xs = [16, 16, 600, 16, 16];
  const v = classifyFrameIntervals(xs);
  assert.deepStrictEqual(v, { kind: 'jank', bucket: 200, count: 1 });
});

// A SUSTAINED stutter: several consecutive moderately-long frames whose total
// blocked time crosses the jank floor is jank even though no single frame hits
// the lone floor (this is the dropped-frame-run case Long Tasks would also flag).
test('a sustained run of long frames is jank', () => {
  const xs = [16, 120, 130, 110, 16]; // 3 long frames, total 360ms
  const v = classifyFrameIntervals(xs);
  assert.deepStrictEqual(v, { kind: 'jank', bucket: 200, count: 1 });
});

// A short run that does not reach the jank floor and has no lone-floor frame is
// NOT jank (two ~110ms frames = 220ms total -> sustained-and-over-floor, so this
// IS jank; use a case under the floor to prove the guard).
test('a brief sub-floor run is not jank', () => {
  const xs = [16, 110, 16, 16]; // one 110ms frame, run length 1, under lone floor
  assert.strictEqual(classifyFrameIntervals(xs), null);
});

// The freeze fixture: a 3500ms stall is one >= 2000ms frame -> HANG.
test('a 3500ms freeze is a hang', () => {
  const xs = [16, 16, 3500, 16];
  const v = classifyFrameIntervals(xs);
  assert.deepStrictEqual(v, { kind: 'hang', bucket: 2000, count: 1 });
});

// Hang takes precedence over jank in the same window.
test('hang wins over jank in the same window', () => {
  const xs = [600, 16, 2500, 16];
  const v = classifyFrameIntervals(xs);
  assert.strictEqual(v.kind, 'hang');
  assert.strictEqual(v.bucket, 2000);
});

// count is the number of distinct stall RUNS, not raw frames: a single stall is
// always count 1 however rAF chopped it, keeping the marker reproducible.
test('count is the number of stall runs', () => {
  const oneStall = [16, 600, 16];
  assert.strictEqual(classifyFrameIntervals(oneStall).count, 1);
  const twoStalls = [16, 600, 16, 16, 700, 16];
  assert.strictEqual(classifyFrameIntervals(twoStalls).count, 2);
});

// Empty / undefined input is clean (a clean action records frames near cadence,
// or nothing if no frames were presented).
test('empty input is clean', () => {
  assert.strictEqual(classifyFrameIntervals([]), null);
  assert.strictEqual(classifyFrameIntervals(undefined), null);
});

// Deterministic: the same interval list always yields the same verdict.
test('classifier is deterministic', () => {
  const xs = [16, 16, 600, 16, 120, 130, 16];
  assert.deepStrictEqual(classifyFrameIntervals(xs), classifyFrameIntervals(xs));
});
