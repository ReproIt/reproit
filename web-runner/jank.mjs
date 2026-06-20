// Per-engine animation-jank probe. Instead of diffing recordings (whose own
// framerate masks the truth), we ask the browser itself: record the interval
// between every animation frame for a few seconds, then summarize smoothness.
// A clean 60fps animation => intervals clustered at ~16.7ms, few drops. Jank =>
// long intervals (the compositor/main thread stalled) and a high drop count.
// Run the SAME page in each engine and compare: smooth in Blink, janky in
// Gecko/WebKit is exactly the bug class the user hit.
import { chromium, firefox, webkit } from 'playwright';
const URL = process.env.REPROIT_URL || 'https://example.com/';
const ENGINES = (process.env.REPROIT_ENGINES || 'chromium,firefox,webkit').split(',').map((s) => s.trim());
const SECONDS = parseFloat(process.env.REPROIT_JANK_SECONDS || '4');
const HEADLESS = process.env.REPROIT_HEADLESS === '1';
const BY = { chromium, firefox, webkit };

const probe = (seconds) =>
  new Promise((resolve) => {
    const ts = [];
    const t0 = performance.now();
    function tick(now) {
      ts.push(now);
      if (now - t0 < seconds * 1000) requestAnimationFrame(tick);
      else {
        const iv = [];
        for (let i = 1; i < ts.length; i++) iv.push(ts[i] - ts[i - 1]);
        resolve(iv);
      }
    }
    requestAnimationFrame(tick);
  });

function summarize(iv) {
  if (!iv.length) return null;
  const s = [...iv].sort((a, b) => a - b);
  const med = s[Math.floor(s.length / 2)];
  const p95 = s[Math.floor(s.length * 0.95)];
  const max = s[s.length - 1];
  const fps = 1000 / (iv.reduce((a, b) => a + b, 0) / iv.length);
  // a "dropped" frame = an interval longer than ~1.5 vsyncs past the median.
  const budget = med * 1.5;
  const dropped = iv.filter((x) => x > budget).length;
  // jerkiness = how variable the cadence is (0 = perfectly even).
  const mean = iv.reduce((a, b) => a + b, 0) / iv.length;
  const sd = Math.sqrt(iv.reduce((a, b) => a + (b - mean) ** 2, 0) / iv.length);
  return { frames: iv.length, fps, medMs: med, p95Ms: p95, maxMs: max, dropped, dropPct: (100 * dropped) / iv.length, jerk: sd / mean };
}

console.log(`JOURNEY[a] step: jank probe — ${ENGINES.join(', ')} @ ${URL}`);
const rows = [];
for (const e of ENGINES) {
  if (!BY[e]) { console.log(`  skip ${e}`); continue; }
  const b = await BY[e].launch({ headless: HEADLESS });
  const p = await (await b.newContext({ viewport: { width: 1280, height: 720 } })).newPage();
  await p.goto(URL, { waitUntil: 'domcontentloaded', timeout: 30000 }).catch(() => {});
  await p.waitForTimeout(400); // let the animation get going
  const iv = await p.evaluate(probe, SECONDS).catch(() => []);
  await b.close();
  const s = summarize(iv);
  rows.push({ e, s });
  if (s)
    console.log(
      `  ${e.padEnd(9)} fps=${s.fps.toFixed(1).padStart(5)}  median=${s.medMs.toFixed(1)}ms  p95=${s.p95Ms.toFixed(1)}ms  max=${s.maxMs.toFixed(0)}ms  dropped=${s.dropped} (${s.dropPct.toFixed(1)}%)  jerkiness=${s.jerk.toFixed(2)}`,
    );
}
// verdict: flag engines whose smoothness is materially worse than the best.
const ok = rows.filter((r) => r.s);
if (ok.length >= 2) {
  const best = ok.reduce((a, b) => (a.s.dropPct <= b.s.dropPct ? a : b));
  console.log(`\n  smoothest: ${best.e} (${best.s.dropPct.toFixed(1)}% dropped)`);
  let flagged = false;
  for (const r of ok) {
    if (r.e === best.e) continue;
    if (r.s.dropPct > best.s.dropPct + 5 || r.s.p95Ms > best.s.p95Ms * 2) {
      console.log(`  ⚠ ${r.e}: ${r.s.dropPct.toFixed(1)}% dropped vs ${best.s.dropPct.toFixed(1)}% — janky relative to ${best.e}`);
      flagged = true;
    }
  }
  console.log(flagged ? 'Some tests failed' : 'All tests passed');
} else console.log('All tests passed');
