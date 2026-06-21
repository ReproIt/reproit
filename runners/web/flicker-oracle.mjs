// Tier-2 flicker detector (pure). The runner records the frames presented
// during a transition (CDP screencast) and computes, per frame, how different
// it is from the FINAL settled frame. This module scores that sequence for a
// TRANSIENT DIVERGENCE: an intermediate frame that is MORE different from the
// settled result than the STARTING frame was, i.e. the screen briefly showed
// something further from BOTH endpoints (a blank/garbled flash) before settling.
//
// Why "more different than the start": frame 0 is the pre-action screen, so its
// distance from the final screen is the legitimate magnitude of the change the
// transition makes. A clean transition only ever gets CLOSER to the final frame
// (distance decreases monotonically to ~0). A flicker overshoots: some middle
// frame is further from the final than frame 0 was, then it recovers. That
// overshoot is the flash.
//
// Pixel + frame timing, so this is timing-sensitive: it lives behind
// REPROIT_FLICKER_PIXELS and is only a finding when it reproduces across
// `check` repeats. This file is the host-pure core (no browser), unit-tested
// with `node --test`, mirroring jank-oracle.mjs.

/// diffs[i] = fraction in [0,1] of pixels by which frame i differs from the
/// FINAL frame (diffs[last] is ~0 by construction). Returns a finding object
/// `{ peak, at, frames }` when a transient divergence is detected, else null.
///
/// floor:  ignore sub-noise peaks (compression/AA jitter) below this fraction.
/// factor: a middle frame must exceed the starting distance by this margin to
///         count as an overshoot, so a transition that merely changes a lot
///         (large but monotonic) is NOT flagged.
export function transientDivergence(diffs, { floor = 0.04, factor = 1.35 } = {}) {
  const n = diffs.length;
  if (n < 3) return null; // need start + at least one middle + settled end
  const start = diffs[0];
  let peak = 0;
  let at = -1;
  for (let i = 1; i < n - 1; i++) {
    if (diffs[i] > peak) {
      peak = diffs[i];
      at = i;
    }
  }
  if (peak > floor && peak > Math.max(start, floor) * factor) {
    return { peak: Math.round(peak * 1000) / 1000, at, frames: n };
  }
  return null;
}
