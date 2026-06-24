// Validates the DOM/layout OVERFLOW oracle (detectOverflow) end to end: it FIRES
// on a layout that clips/overflows (a fixed-width button whose long label is
// truncated by text-overflow; a child wider than its non-scrolling parent) and
// stays SILENT on a clean element that fits. This is the i18n / long-string / RTL
// failure class (a German or RTL string overflowing a fixed button).
//
// Uses a REAL Chromium via Playwright: the predicate is DOM-bound (scrollWidth/
// clientWidth, getBoundingClientRect child-vs-parent, offsetWidth<scrollWidth),
// so jsdom cannot stand in. The fixture is a static, deterministic HTML file
// (overflow-fixture.html), so the measurement is byte-reproducible. Run with
// `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { chromium } from 'playwright';
import { detectOverflow, OVERFLOW_TOL } from './runner.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_URL = pathToFileURL(join(HERE, 'overflow-fixture.html')).href;

// Browser-backed: skip cleanly where Chromium isn't installed (e.g. the CI
// web-runner job, which runs `node --test` without `npx playwright install`), so
// this never red-flags a browserless environment. It runs fully anywhere a
// browser is present (local dev, or a CI job that installs one). Same guard as
// groundtruth-taps.test.mjs.
let browserUnavailable = false;
try {
  const probe = await chromium.launch();
  await probe.close();
} catch (e) {
  browserUnavailable = `chromium not launchable (${e && e.message ? e.message.split('\n')[0] : e}); skipping`;
}

test('detectOverflow fires on clipped/overflowing nodes and is silent on the clean control', { skip: browserUnavailable }, async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.goto(FIXTURE_URL);
    const items = await page.evaluate(detectOverflow, OVERFLOW_TOL);

    // FIRES: the clipped button is reported with kind `clip` (its single-line
    // label is wider than the fixed-width box, truncated by text-overflow).
    assert.ok(
      items.some((i) => i.key === 'id:clip-btn' && i.kind === 'clip'),
      `expected a clip finding for #clip-btn, got ${JSON.stringify(items)}`,
    );
    // FIRES: the wide child escapes its non-scrolling fixed-width parent. The
    // overflow surfaces as a `spill` (child border box past parent content box)
    // and/or `scroll` (parent content wider than its client box); require at least
    // one of those for the wide child.
    assert.ok(
      items.some((i) => i.key === 'id:wide-child' && (i.kind === 'spill' || i.kind === 'scroll')),
      `expected a spill/scroll finding for #wide-child, got ${JSON.stringify(items)}`,
    );

    // SILENT: the clean control fits its container, so it must NOT appear in ANY
    // signal.
    assert.ok(
      !items.some((i) => i.key === 'id:clean-btn'),
      `clean #clean-btn must not be flagged, got ${JSON.stringify(items)}`,
    );

    // Every reported overflow exceeds the documented tolerance (deterministic
    // threshold, not a ratio).
    assert.ok(items.every((i) => i.by > OVERFLOW_TOL), `all overflows exceed the tolerance, got ${JSON.stringify(items)}`);
  } finally {
    await browser.close();
  }
});

test('detectOverflow is deterministic across repeated captures', { skip: browserUnavailable }, async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.goto(FIXTURE_URL);
    const a = await page.evaluate(detectOverflow, OVERFLOW_TOL);
    const b = await page.evaluate(detectOverflow, OVERFLOW_TOL);
    // Same DOM -> byte-identical finding (already sorted by key+kind in-page).
    assert.deepStrictEqual(a, b, 'same layout -> same overflow findings');
  } finally {
    await browser.close();
  }
});

test('detectOverflow does NOT flag a child of a ZERO-SIZE parent (the SPA-wrapper false positive)', { skip: browserUnavailable }, async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    // A normal on-screen button whose immediate parent is a collapsed 0x0 wrapper
    // (a positioning context / fragment, common in SPA layouts). The parent's
    // content edge sits at the origin, so the old spill math reported a giant
    // phantom overflow (button.right - 0 = ~1151px) for a button that renders fine.
    await page.setContent('<!doctype html><html><body style="margin:0;font:16px monospace">' +
      '<div style="width:0;height:0;position:relative">' +
      '<button style="position:absolute;left:1032px;top:117px;width:119px">Copy page</button>' +
      '</div></body></html>');
    const items = await page.evaluate(detectOverflow, OVERFLOW_TOL);
    const phantom = items.find((i) => i.kind === 'spill' && i.by > 200);
    assert.equal(phantom, undefined, `a zero-size parent must not manufacture a spill, got ${JSON.stringify(items)}`);
  } finally {
    await browser.close();
  }
});

test('detectOverflow stays silent on a layout with no overflow', { skip: browserUnavailable }, async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    // A clean page: a short label in a box wide enough to hold it, nothing clips.
    await page.setContent('<!doctype html><html><body style="margin:0;font:16px monospace">' +
      '<button style="width:200px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;padding:4px">Save</button>' +
      '</body></html>');
    const items = await page.evaluate(detectOverflow, OVERFLOW_TOL);
    assert.deepStrictEqual(items, [], `clean layout must report nothing, got ${JSON.stringify(items)}`);
  } finally {
    await browser.close();
  }
});
