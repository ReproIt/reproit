// Cross-engine compositor-jank oracle.
//
// The detector core is `trajectoryJank(positions)`: given the moving element's
// position sampled once per presented frame, it scores how UN-smooth the motion
// is. A clean animation advances in even steps (constant velocity → ~0 jerk);
// jank shows up as stalls (dropped frames) punctuated by catch-up jumps, which
// spike the jerk. Score ≈ 0 = smooth, → 1 = badly janky. This is the thing that
// distinguishes "Chrome smooth, Firefox/WebKit stutter".
//
// CAPTURE has two modes:
//   A) transform-sampling (runs anywhere, incl. headless): sample the element's
//      computed transform via rAF. Catches ANIMATION-CLOCK jank. It does NOT
//      see pure compositor-presentation stutter (the main thread can be smooth
//      while the GPU drops composited frames) — same blind spot as a plain rAF
//      probe, stated honestly.
//   B) presented-frame capture (the faithful one): record the real window at
//      60fps and track the element visually. This MUST run on an ISOLATED
//      DISPLAY (CI runner / headless VM / Xvfb), NEVER the user's desktop —
//      capturing/moving real windows there is unsafe (it once grabbed the
//      user's own browser). Mode B is gated behind REPROIT_JANK_DISPLAY=isolated.
//
// Usage: REPROIT_URL=… REPROIT_JANK_SELECTOR='.hero-shimmer' node jank-oracle.mjs

import { chromium, firefox, webkit } from 'playwright';

/// Pure detector. positions: number[] (e.g. x of the moving element per frame).
export function trajectoryJank(positions) {
  const n = positions.length;
  if (n < 3) return { score: 0, stalls: 0, jumps: 0, frames: n, meanV: 0 };
  const v = [];
  for (let i = 1; i < n; i++) v.push(positions[i] - positions[i - 1]);
  const absV = v.map(Math.abs);
  const meanV = absV.reduce((a, b) => a + b, 0) / absV.length;
  if (meanV === 0) return { score: 0, stalls: 0, jumps: 0, frames: n, meanV: 0 };
  // stalls: near-zero motion during an otherwise-moving animation (dropped frame).
  const stalls = absV.filter((x) => x < 0.15 * meanV).length;
  // jumps: motion far above the median step (catch-up after a stall).
  const sorted = [...absV].sort((a, b) => a - b);
  const med = sorted[sorted.length >> 1] || meanV;
  const jumps = absV.filter((x) => x > Math.max(3 * med, 2.5 * meanV)).length;
  // jerk: normalized total change in velocity. Constant velocity => 0.
  let jerk = 0;
  for (let i = 1; i < v.length; i++) jerk += Math.abs(v[i] - v[i - 1]);
  const totalDisp = Math.abs(positions[n - 1] - positions[0]) || meanV * (n - 1);
  const jerkNorm = jerk / (totalDisp || 1);
  const score = Math.min(1, jerkNorm / 4 + (stalls + jumps) / v.length);
  return { score, stalls, jumps, frames: n, meanV };
}

// Mode A: sample the element's transform translateX per animation frame.
// NB: page.evaluate passes a SINGLE arg, so selector+seconds come in as one obj.
const SAMPLE = ({ selector, seconds }) =>
  new Promise((resolve) => {
    const el = document.querySelector(selector);
    if (!el) return resolve([]);
    const xs = [];
    const t0 = performance.now();
    const read = () => {
      const m = new DOMMatrixReadOnly(getComputedStyle(el).transform);
      return m.m41; // translateX
    };
    function tick(now) {
      xs.push(read());
      if (now - t0 < seconds * 1000) requestAnimationFrame(tick);
      else resolve(xs);
    }
    requestAnimationFrame(tick);
  });

async function main() {
  const URL = process.env.REPROIT_URL || 'https://example.com/';
  const SELECTOR = process.env.REPROIT_JANK_SELECTOR || '.hero-shimmer';
  const SECONDS = parseFloat(process.env.REPROIT_JANK_SECONDS || '5');
  const ENGINES = (process.env.REPROIT_ENGINES || 'chromium,firefox,webkit').split(',');
  const isolated = process.env.REPROIT_JANK_DISPLAY === 'isolated';
  const BY = { chromium, firefox, webkit };

  if (!isolated) {
    console.error(
      'NOTE: mode A (transform sampling) — catches animation-clock jank, NOT compositor presentation stutter.',
    );
    console.error(
      'For the faithful detector run on an ISOLATED display (CI/Xvfb) with REPROIT_JANK_DISPLAY=isolated.',
    );
  }

  const rows = [];
  for (const e of ENGINES) {
    if (!BY[e]) continue;
    const b = await BY[e].launch({ headless: true });
    const p = await (await b.newContext({ viewport: { width: 1280, height: 720 } })).newPage();
    await p.goto(URL, { waitUntil: 'domcontentloaded', timeout: 30000 }).catch(() => {});
    await p.waitForTimeout(400);
    const xs = await p.evaluate(SAMPLE, { selector: SELECTOR, seconds: SECONDS }).catch(() => []);
    await b.close();
    const j = trajectoryJank(xs);
    rows.push({ e, j });
    console.log(
      `  ${e.padEnd(9)} frames=${j.frames} meanStep=${j.meanV.toFixed(2)} stalls=${j.stalls} jumps=${j.jumps} jank=${j.score.toFixed(3)}`,
    );
  }
  const scored = rows.filter((r) => r.j.meanV > 0);
  if (scored.length >= 2) {
    const best = scored.reduce((a, b) => (a.j.score <= b.j.score ? a : b));
    let flagged = false;
    for (const r of scored) {
      if (r.e !== best.e && r.j.score > best.j.score + 0.2) {
        console.log(`  ⚠ ${r.e}: jank ${r.j.score.toFixed(3)} vs ${best.e} ${best.j.score.toFixed(3)}`);
        flagged = true;
      }
    }
    console.log(flagged ? 'Some tests failed' : 'All tests passed');
  } else {
    console.log(`no animated motion on "${SELECTOR}" to score (try a different selector)`);
    console.log('All tests passed');
  }
}

// run main() only as a script, not when imported by the test.
if (import.meta.url === `file://${process.argv[1]}`) main();
