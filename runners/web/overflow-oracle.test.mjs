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

test('detectOverflow fires on clipped/overflowing nodes and is silent on the clean control', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.goto(FIXTURE_URL);
    const items = await page.evaluate(detectOverflow, OVERFLOW_TOL);

    // FIRES: the clipped button is reported with kind `clip` (its single-line
    // label is wider than the fixed-width box, truncated by text-overflow), and is
    // marked INTERACTIVE so the reporting layer keeps it (a hidden control label).
    assert.ok(
      items.some((i) => i.key === 'id:clip-btn' && i.kind === 'clip' && i.interactive === true),
      `expected an interactive clip finding for #clip-btn, got ${JSON.stringify(items)}`,
    );
    // EMITTED BUT NON-INTERACTIVE: the same ellipsis on a caption is intended
    // truncation. detectOverflow still reports the `clip`, but interactive:false
    // tells the reporting layer to drop it (not a hidden control label).
    assert.ok(
      items.some((i) => i.key === 'id:clip-caption' && i.kind === 'clip' && i.interactive === false),
      `expected a non-interactive clip for #clip-caption, got ${JSON.stringify(items)}`,
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

test('detectOverflow is deterministic across repeated captures', async () => {
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

test('detectOverflow does NOT flag a child of a ZERO-SIZE parent (the SPA-wrapper false positive)', async () => {
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

test('detectOverflow does NOT flag items of a horizontal RAIL (logo marquee / carousel)', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    // A "trusted by" logo marquee: a track many viewports wide (scrollWidth >>
    // clientWidth) under an overflow-hidden frame, holding logos each a few px
    // WIDER than their slot. Items meant to scroll past the frame -- on-screen
    // ones spill their slot, off-screen ones spill the (off-screen) track. None is
    // a visible layout bug. (Mirrors the real-site false positive.)
    let logos = '';
    for (let i = 0; i < 30; i++) {
      // slot 200px, logo 234px (spills its slot by ~34px on each side).
      logos += '<li class="slot" style="flex:0 0 200px;display:flex;justify-content:center">' +
        '<img class="logo" style="width:234px;height:40px;background:#ccc" alt="logo' + i + '"></li>';
    }
    await page.setContent('<!doctype html><html><body style="margin:0;font:16px monospace">' +
      '<div class="frame" style="overflow:hidden;width:1280px">' +
      '<ul class="track" style="display:flex;list-style:none;margin:0;padding:0;width:max-content;transform:translateX(-606px)">' +
      logos + '</ul></div></body></html>');
    const items = await page.evaluate(detectOverflow, OVERFLOW_TOL);
    const railSpill = items.find((i) => i.kind === 'spill' && i.key === 'tag:img');
    assert.equal(railSpill, undefined, `a marquee logo must not be flagged as a spill, got ${JSON.stringify(items)}`);
  } finally {
    await browser.close();
  }
});

test('detectOverflow does NOT flag a spill entirely off-screen horizontally', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    // A child wider than its parent, but the whole parent is parked off-screen to
    // the left (a slid-away panel / off-canvas menu). The spill is real in the box
    // model but the user never sees it, so it must not be reported.
    await page.setContent('<!doctype html><html><body style="margin:0;font:16px monospace">' +
      '<div style="position:absolute;left:-900px;top:100px;width:200px;height:60px">' +
      '<div style="width:340px;height:60px;background:#ccc">offscreen panel content</div>' +
      '</div></body></html>');
    const items = await page.evaluate(detectOverflow, OVERFLOW_TOL);
    assert.ok(
      !items.some((i) => i.kind === 'spill'),
      `an entirely off-screen spill must not be flagged, got ${JSON.stringify(items)}`,
    );
  } finally {
    await browser.close();
  }
});

test('detectOverflow stays silent on a layout with no overflow', async () => {
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
