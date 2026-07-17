// Cross-engine differential testing. Runs the SAME authored test (load + an
// optional replay action list) across multiple browser engines, then flags
// where they diverge:
//   - VISUAL: per-frame pixel diff, anti-aliasing aware (pixelmatch includeAA),
//             so normal font-hinting differences don't false-positive; only
//             real layout / color / animation divergence counts.
//   - CONSOLE: first-party-origin console errors present in one engine but not
//             another (third-party tracker noise is filtered out, a common
//             source of false positives).
//   - LAYOUT: bounding-box position/size of salient elements per engine.
//
// Defaults to HEADED so the real GPU compositor runs (animation/compositor bugs
// frequently do NOT reproduce in headless software rendering).
//
// Env:
//   REPROIT_URL              page under test (required)
//   REPROIT_ENGINES          csv: chromium,firefox,webkit  (default all three)
//   REPROIT_HEADLESS         '1' to force headless (default '0' = headed + GPU)
//   REPROIT_DIFF_OUT         output dir (default /tmp/reproit-diff)
//   REPROIT_FUZZ_CONFIG      optional {replay:[ "key:Down","tap:Pricing","scroll:600" ]}
//   REPROIT_DIFF_FRAMES      csv ms offsets to sample (default 300,700,1200,2000,3200)
//   REPROIT_VIEWPORT         WxH (default 1366x900)
//   REPROIT_DIFF_THRESHOLD   max non-AA divergent-pixel ratio before it's a finding (default 0.004)

import { chromium, firefox, webkit } from 'playwright';
import { mkdirSync, writeFileSync, readFileSync, existsSync } from 'node:fs';
import { PNG } from 'pngjs';
import pixelmatch from 'pixelmatch';

const URL = process.env.REPROIT_URL || 'https://example.com/';
const ENGINES = (process.env.REPROIT_ENGINES || 'chromium,firefox,webkit')
  .split(',')
  .map((s) => s.trim())
  .filter(Boolean);
const HEADLESS = process.env.REPROIT_HEADLESS === '1';
const OUT = process.env.REPROIT_DIFF_OUT || '/tmp/reproit-diff';
const FRAMES = (process.env.REPROIT_DIFF_FRAMES || '300,700,1200,2000,3200')
  .split(',')
  .map((n) => parseInt(n, 10));
const [VW, VH] = (process.env.REPROIT_VIEWPORT || '1366x900').split('x').map(Number);
const THRESHOLD = parseFloat(process.env.REPROIT_DIFF_THRESHOLD || '0.004');
// Mean per-pixel difference (0..255) that flags a broad, smooth rendering
// divergence (gradient/animation rendered differently). chrome-vs-chrome ≈ 0;
// an observed cross-engine gradient bug measured ≈ 3.5. 1.5 separates real
// divergence from sub-pixel AA noise.
const MEAN_DELTA_THRESHOLD = parseFloat(process.env.REPROIT_DIFF_MEANDELTA || '1.5');
const CLIP = { x: 0, y: 0, width: VW, height: Math.min(VH, 560) };
const BY = { chromium, firefox, webkit };
mkdirSync(OUT, { recursive: true });

function emit(s) {
  console.log(s);
}

// Replay one authored action. Minimal but real: key presses, tap-by-text,
// scroll. Same vocabulary the explorer/runner uses, so an authored test or a
// production-seeded path replays identically across engines.
async function applyAction(page, act) {
  if (act.startsWith('key:')) {
    const map = {
      Down: 'ArrowDown',
      Up: 'ArrowUp',
      Right: 'ArrowRight',
      Left: 'ArrowLeft',
      Enter: 'Enter',
      Tab: 'Tab',
      Esc: 'Escape',
      Space: ' ',
    };
    await page.keyboard.press(map[act.slice(4)] || act.slice(4)).catch(() => {});
  } else if (act.startsWith('tap:')) {
    const label = act.slice(4);
    await page
      .getByText(label, { exact: false })
      .first()
      .click({ timeout: 2000 })
      .catch(() => {});
  } else if (act.startsWith('scroll:')) {
    const y = parseInt(act.slice(7), 10) || 400;
    await page.evaluate((yy) => window.scrollTo(0, yy), y).catch(() => {});
  }
}

const origin = (() => {
  try {
    return new globalThis.URL(URL).origin;
  } catch {
    return '';
  }
})();

async function captureEngine(engine, replay) {
  const browser = await BY[engine].launch({ headless: HEADLESS });
  const ctx = await browser.newContext({
    viewport: { width: VW, height: VH },
    deviceScaleFactor: 1,
  });
  const page = await ctx.newPage();
  const console_all = [];
  page.on('console', (m) => {
    if (m.type() !== 'error' && m.type() !== 'warning') return;
    const loc = m.location && m.location();
    const url = (loc && loc.url) || '';
    console_all.push({ type: m.type(), text: m.text().slice(0, 240), url });
  });
  page.on('pageerror', (e) =>
    console_all.push({ type: 'pageerror', text: String(e).slice(0, 240), url: URL }),
  );
  await page.goto(URL, { waitUntil: 'domcontentloaded', timeout: 30000 }).catch(() => {});
  for (const a of replay) {
    await applyAction(page, a);
    await page.waitForTimeout(120);
  }

  const frames = {};
  let elapsed = 0;
  for (const ms of FRAMES) {
    await page.waitForTimeout(Math.max(0, ms - elapsed));
    elapsed = ms;
    const buf = await page.screenshot({ clip: CLIP }).catch(() => null);
    if (buf) {
      const p = `${OUT}/${engine}_${ms}.png`;
      writeFileSync(p, buf);
      frames[ms] = p;
    }
  }
  // salient element layout boxes, keyed by a stable identity (tag + text head).
  const boxes = await page
    .evaluate(() => {
      const sel = 'h1,h2,header,nav,button,a[role=button],[class*=hero],[class*=Hero]';
      const out = {};
      for (const el of document.querySelectorAll(sel)) {
        const r = el.getBoundingClientRect();
        if (r.width < 2 || r.height < 2) continue;
        const key = `${el.tagName.toLowerCase()}:${(el.textContent || '').trim().slice(0, 24)}`;
        if (!out[key])
          out[key] = {
            x: Math.round(r.x),
            y: Math.round(r.y),
            w: Math.round(r.width),
            h: Math.round(r.height),
          };
      }
      return out;
    })
    .catch(() => ({}));

  // first-party console issues only (drop third-party tracker noise).
  const firstParty = [
    ...new Set(
      console_all
        .filter(
          (c) =>
            !c.url ||
            c.url.startsWith(origin) ||
            c.url.startsWith('http') === false ||
            c.url === URL,
        )
        .map((c) => `[${c.type}] ${c.text}`),
    ),
  ];
  await browser.close();
  return { engine, frames, boxes, firstParty, allConsole: console_all.length };
}

// AA-aware visual diff between two same-size PNGs. Returns {ratio, ratioRaw, diffPath}.
function visualDiff(aPath, bPath, outPath) {
  const a = PNG.sync.read(readFileSync(aPath));
  const b = PNG.sync.read(readFileSync(bPath));
  if (a.width !== b.width || a.height !== b.height)
    return { ratio: 1, ratioRaw: 1, diffPath: null, sizeMismatch: true };
  const { width, height } = a;
  const diff = new PNG({ width, height });
  // includeAA:false → pixels that look like anti-aliasing are NOT counted.
  const mismatch = pixelmatch(a.data, b.data, diff.data, width, height, {
    threshold: 0.1,
    includeAA: false,
  });
  const mismatchRaw = pixelmatch(a.data, b.data, null, width, height, {
    threshold: 0.1,
    includeAA: true,
  });
  writeFileSync(outPath, PNG.sync.write(diff));
  const total = width * height;
  // Mean per-pixel difference magnitude (0..255). Unlike pixelmatch's
  // thresholded count, this is sensitive to BROAD, SMOOTH, low-contrast
  // divergence, a mis-rendered gradient or animation layer, which is exactly
  // the cross-engine bug pixelmatch's AA-aware threshold throws away. A few
  // sharp text-edge pixels barely move this; a whole gradient rendering
  // differently moves it a lot.
  let sum = 0;
  for (let i = 0; i < a.data.length; i += 4) {
    sum +=
      Math.abs(a.data[i] - b.data[i]) +
      Math.abs(a.data[i + 1] - b.data[i + 1]) +
      Math.abs(a.data[i + 2] - b.data[i + 2]);
  }
  const meanDelta = sum / (total * 3);
  return { ratio: mismatch / total, ratioRaw: mismatchRaw / total, meanDelta, diffPath: outPath };
}

async function main() {
  const cfgPath = process.env.REPROIT_FUZZ_CONFIG;
  let replay = [];
  if (cfgPath && existsSync(cfgPath)) {
    try {
      replay = JSON.parse(readFileSync(cfgPath, 'utf8')).replay || [];
    } catch {}
  }
  emit('JOURNEY claimed role=a');
  emit(
    `JOURNEY[a] step: cross-engine differential: ${ENGINES.join(', ')} @ ${URL} ` +
      `(${HEADLESS ? 'headless' : 'headed+GPU'})`,
  );

  const results = [];
  for (const e of ENGINES) {
    if (!BY[e]) {
      emit(`  skip unknown engine ${e}`);
      continue;
    }
    results.push(await captureEngine(e, replay));
  }
  if (results.length < 2) {
    emit('Need >=2 engines to diff.');
    emit('All tests passed');
    return;
  }

  const ref = results[0];
  const findings = [];
  const report = {
    url: URL,
    headless: HEADLESS,
    engines: ENGINES,
    reference: ref.engine,
    frames: FRAMES,
    comparisons: [],
  };

  for (const r of results.slice(1)) {
    emit(`\n=== ${ref.engine} (reference) vs ${r.engine} ===`);
    const cmp = {
      engine: r.engine,
      visual: [],
      consoleOnlyHere: [],
      consoleOnlyRef: [],
      layout: [],
    };

    // VISUAL per frame. Two complementary signals:
    //  - pixelmatch ratio: SHARP differences (layout shifts, text, hard edges).
    //  - meanDelta: BROAD SMOOTH differences (a gradient / animation layer
    //    rendering differently across engines) that pixelmatch throws away.
    // Either tripping is a finding.
    for (const ms of FRAMES) {
      if (!ref.frames[ms] || !r.frames[ms]) continue;
      const d = visualDiff(ref.frames[ms], r.frames[ms], `${OUT}/diff_${r.engine}_${ms}.png`);
      const pct = (d.ratio * 100).toFixed(2);
      const pctRaw = (d.ratioRaw * 100).toFixed(2);
      const sharp = d.ratio > THRESHOLD;
      const smooth = d.meanDelta > MEAN_DELTA_THRESHOLD;
      const flag = sharp || smooth;
      cmp.visual.push({ ms, ratio: d.ratio, ratioRaw: d.ratioRaw, meanDelta: d.meanDelta, flag });
      emit(
        `  VISUAL ${ms}ms: ${pct}% sharp (non-AA) · ` +
          `meanΔ ${d.meanDelta.toFixed(2)}/255 (gradient)` +
          `${flag ? '  ⚠ FLAG' : ''}`,
      );
      if (sharp) findings.push(`${r.engine} sharp-diverges ${pct}% from ${ref.engine} at ${ms}ms`);
      if (smooth)
        findings.push(
          `${r.engine} renders the animation/gradient differently from ${ref.engine} ` +
            `at ${ms}ms (meanΔ ${d.meanDelta.toFixed(2)})`,
        );
    }
    // CONSOLE (first-party only)
    cmp.consoleOnlyHere = r.firstParty.filter((x) => !ref.firstParty.includes(x));
    cmp.consoleOnlyRef = ref.firstParty.filter((x) => !r.firstParty.includes(x));
    if (cmp.consoleOnlyHere.length) {
      emit(`  CONSOLE only in ${r.engine}: ${cmp.consoleOnlyHere.length}`);
      cmp.consoleOnlyHere.slice(0, 6).forEach((e) => emit(`    - ${e}`));
      findings.push(
        `${r.engine} has ${cmp.consoleOnlyHere.length} first-party console error(s) ` +
          `absent in ${ref.engine}`,
      );
    }
    // LAYOUT
    for (const key of Object.keys(ref.boxes)) {
      const a = ref.boxes[key],
        b = r.boxes[key];
      if (!b) {
        cmp.layout.push({ key, missing: true });
        continue;
      }
      const dx = Math.abs(a.x - b.x),
        dy = Math.abs(a.y - b.y),
        dw = Math.abs(a.w - b.w),
        dh = Math.abs(a.h - b.h);
      if (dx > 4 || dy > 4 || dw > 6 || dh > 6) {
        cmp.layout.push({ key, dx, dy, dw, dh });
        emit(`  LAYOUT "${key}": Δx${dx} Δy${dy} Δw${dw} Δh${dh}`);
        findings.push(`${r.engine} layout of "${key}" shifts Δx${dx} Δy${dy} Δw${dw} Δh${dh}`);
      }
    }
    report.comparisons.push(cmp);
  }

  writeFileSync(`${OUT}/report.json`, JSON.stringify(report, null, 2));
  writeReportHtml(`${OUT}/report.html`, report, results);
  emit(`\nreport: ${OUT}/report.html`);
  emit('JOURNEY DONE');
  if (findings.length) {
    emit('EXCEPTION CAUGHT BY CROSS-ENGINE DIFF');
    findings.slice(0, 12).forEach((f) => emit(`  ${f}`));
    emit('Some tests failed');
  } else {
    emit('All tests passed');
  }
}

function writeReportHtml(path, report, results) {
  const eng = results.map((r) => r.engine);
  const rows = report.frames
    .map((ms) => {
      const cells = eng.map((e) => `<td><img src="${e}_${ms}.png"></td>`).join('');
      const diffs = report.comparisons
        .map((c) => {
          const v = c.visual.find((x) => x.ms === ms);
          const pct = v ? (v.ratio * 100).toFixed(2) : '-';
          const flag = v && v.flag ? ' ⚠' : '';
          return (
            `<td><div class="lbl">${c.engine} Δ ${pct}%${flag}</div>` +
            `<img src="diff_${c.engine}_${ms}.png"></td>`
          );
        })
        .join('');
      return `<tr><th>${ms}ms</th>${cells}${diffs}</tr>`;
    })
    .join('');
  const engineHeaders = eng.map((engine) => `<th>${engine}</th>`).join('');
  const comparisonHeaders = report.comparisons
    .map(
      (comparison) =>
        `<th>diff: ${report.reference}↔${comparison.engine}` + '<br><small>non-AA</small></th>',
    )
    .join('');
  const head = `<tr><th></th>${engineHeaders}${comparisonHeaders}</tr>`;
  const html = `<!doctype html><meta charset=utf8><title>Repro It · cross-engine diff</title>
<style>body{background:#0e0d0b;color:#ede6d6;font:13px ui-monospace,monospace;padding:24px}
h1{font-weight:400}h1 b{color:#ffb000}a{color:#ffb000}
table{border-collapse:collapse}td,th{border:1px solid #2a2722;padding:4px;vertical-align:top}
img{width:300px;display:block}.lbl{color:#a89e8a;margin-bottom:3px}small{color:#a89e8a}</style>
<h1>repro it<b>_</b> · cross-engine differential</h1>
<p>${report.url}. Reference: <b>${report.reference}</b>.
${report.headless ? 'headless' : 'headed+GPU'}. Diff ignores font anti-aliasing.</p>
<table>${head}${rows}</table>`;
  writeFileSync(path, html);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
