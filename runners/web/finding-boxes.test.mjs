// Validates the recorded-replay finding-highlight (drawFindingBoxes + tap's
// trigger tagging) end to end in a REAL Chromium: the "here's the bug" red box
// that ends a `check --record` clip. Covers every oracle that has an on-screen
// place - overflow + content-bug (re-detected from the settled DOM), crash/jank/
// hang (the tagged trigger element), and flicker (a churned-anchor key resolved
// back to its node). The fixture is static + deterministic so box counts/labels
// are byte-reproducible. Run with `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { chromium } from 'playwright';
import { drawFindingBoxes, tap } from './runner.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURE_URL = pathToFileURL(join(HERE, 'finding-boxes-fixture.html')).href;

// The label text of every drawn box, in DOM order.
async function boxLabels(page) {
  return await page.evaluate(() => {
    const layer = document.getElementById('__reproit_boxes');
    if (!layer) return [];
    return Array.from(layer.children).map((box) =>
      box.firstChild ? box.firstChild.textContent : '',
    );
  });
}

async function withPage(fn) {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    await page.goto(FIXTURE_URL, { waitUntil: 'networkidle' });
    await fn(page);
  } finally {
    await browser.close();
  }
}

test('boxes the content-bug from the settled DOM', async () => {
  await withPage(async (page) => {
    await drawFindingBoxes(page);
    const labels = await boxLabels(page);
    assert.ok(
      labels.some((l) => l.includes('[object Object]')),
      'expected a content box: ' + JSON.stringify(labels),
    );
  });
});

test('tap(mark) tags the clicked control, and the crash box points at it', async () => {
  await withPage(async (page) => {
    let crashes = 0;
    page.on('pageerror', () => {
      crashes++;
    });
    const tapped = await tap(page, 'key:id:boom', { mark: true });
    assert.equal(tapped, true);
    await page.waitForTimeout(200);
    assert.ok(crashes >= 1, 'the Submit button should have thrown');
    const taggedId = await page.evaluate(() => {
      const t = document.querySelector('[data-reproit-trigger]');
      return t ? t.id : null;
    });
    assert.equal(taggedId, 'boom', 'the crashing button must carry the trigger tag');
    await drawFindingBoxes(page, { triggerLabel: 'crash' });
    const labels = await boxLabels(page);
    assert.ok(labels.includes('crash'), 'expected a crash box: ' + JSON.stringify(labels));
  });
});

test('tap box mode draws a labeled highlight on the target WITHOUT clicking', async () => {
  await withPage(async (page) => {
    let crashes = 0;
    page.on('pageerror', () => {
      crashes++;
    });
    // `box` previews the target (pre-tap annotation) instead of clicking it. The
    // fixture's Submit throws on click, so a click would crash; box mode must not.
    const ok = await tap(page, 'key:id:boom', { box: 'tap  Submit', boxColor: '#e21f1f' });
    assert.equal(ok, true);
    await page.waitForTimeout(150);
    assert.equal(crashes, 0, 'box mode must NOT click the element');
    const caption = await page.evaluate(() => {
      const l = document.getElementById('__reproit_tapbox');
      return l && l.firstChild && l.firstChild.firstChild
        ? l.firstChild.firstChild.textContent
        : null;
    });
    assert.ok(caption && caption.includes('Submit'), 'expected a labeled tapbox: ' + caption);
  });
});

test('only the LAST tapped control keeps the trigger tag', async () => {
  await withPage(async (page) => {
    await tap(page, 'key:id:safe', { mark: true });
    await tap(page, 'key:id:boom', { mark: true }).catch(() => {}); // throws, but tags first
    await page.waitForTimeout(100);
    const ids = await page.evaluate(() =>
      Array.from(document.querySelectorAll('[data-reproit-trigger]')).map((e) => e.id),
    );
    assert.deepEqual(
      ids,
      ['boom'],
      'exactly the last clicked element is tagged: ' + JSON.stringify(ids),
    );
  });
});

test('tap WITHOUT mark never tags (a normal fuzz walk does not touch the DOM)' + '', async () => {
  await withPage(async (page) => {
    await tap(page, 'key:id:safe');
    const n = await page.evaluate(() => document.querySelectorAll('[data-reproit-trigger]').length);
    assert.equal(n, 0);
  });
});

test('resolves a churned-anchor key back to its node and boxes it (flicker)', async () => {
  await withPage(async (page) => {
    await drawFindingBoxes(page, { flickerKeys: ['id:site-header'] });
    const labels = await boxLabels(page);
    assert.ok(
      labels.some((l) => l.includes('flicker')),
      'expected a flicker box: ' + JSON.stringify(labels),
    );
  });
});

test('oracle=broken-render boxes ONLY the content bug, a single box', async () => {
  await withPage(async (page) => {
    await drawFindingBoxes(page, { oracle: 'broken-render' });
    const labels = await boxLabels(page);
    assert.equal(labels.length, 1, 'exactly one box: ' + JSON.stringify(labels));
    assert.ok(
      labels[0].includes('[object Object]'),
      'the box is the content bug: ' + JSON.stringify(labels),
    );
  });
});

test('no oracle hint boxes the visible content bug', async () => {
  await withPage(async (page) => {
    await drawFindingBoxes(page);
    const labels = await boxLabels(page);
    assert.ok(labels.some((l) => l.includes('[object Object]')));
  });
});

// Box geometry alongside labels, for the rect-tightening assertions.
async function boxGeom(page) {
  return await page.evaluate(() => {
    const layer = document.getElementById('__reproit_boxes');
    if (!layer) return [];
    return Array.from(layer.children).map((box) => ({
      label: box.firstChild ? box.firstChild.textContent : '',
      w: parseFloat(box.style.width) || box.getBoundingClientRect().width,
    }));
  });
}

test('broken-route boxes the dead link, never a same-page #fragment', async () => {
  await withPage(async (page) => {
    // The fixture's own pathname is what a "#..." href resolves to; hunting it
    // must find nothing (the Skip to Content regression: an invisible skip
    // link matched the source page's path and got boxed).
    const ownPath = await page.evaluate(() => location.pathname);
    await drawFindingBoxes(page, { oracle: 'broken-route', linkHref: ownPath });
    const labels = await boxLabels(page);
    assert.ok(
      !labels.some((l) => l.includes('broken')),
      'fragment link must not match: ' + JSON.stringify(labels),
    );
  });
});

test('a glyphless image-overlay dead link is captioned as an image link', async () => {
  await withPage(async (page) => {
    await drawFindingBoxes(page, { oracle: 'broken-route', linkHref: '/dead-image' });
    const labels = await boxLabels(page);
    assert.equal(labels.length, 1, 'one link box: ' + JSON.stringify(labels));
    assert.ok(labels[0].includes('broken image link'), 'captioned as image link: ' + labels[0]);
    assert.ok(labels[0].includes('Course tile'), 'named by the img alt: ' + labels[0]);
    assert.ok(labels[0].includes('/dead-image'), 'shows the dead route: ' + labels[0]);
  });
});

test('a block-level text dead link gets a text-tight box, not the full block', async () => {
  await withPage(async (page) => {
    await drawFindingBoxes(page, { oracle: 'broken-route', linkHref: '/dead-text' });
    const boxes = (await boxGeom(page)).filter((b) => b.label.includes('broken link'));
    assert.equal(boxes.length, 1, 'one link box: ' + JSON.stringify(boxes));
    assert.ok(boxes[0].label.includes('"tiny"'), 'captioned with the link text: ' + boxes[0].label);
    assert.ok(boxes[0].w < 300, 'box hugs the text, not the 600px block: ' + boxes[0].w);
  });
});
