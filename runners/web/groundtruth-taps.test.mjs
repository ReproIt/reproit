// Validates that the fuzz candidate set (snapshot().tappables) now includes
// pointer-operable controls the tappable grammar (interactive()) drops but the
// operability ground truth counts as operable: a delegated <div role=option> and
// a cursor:pointer <div>, BOTH addressed by a stable key. Without this the
// explorer never taps delegated-click SPA controls, so such an app maps to ~1
// state (the motivating coverage gap).
//
// Uses a REAL Chromium via Playwright: the predicate is DOM-bound (cursor,
// roles, viewport hit-testing), so jsdom cannot stand in. Run with `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { snapshot } from './runner.mjs';

// A delegated-click "SPA" shape: each option carries NO own handler; a single
// document listener drives every option. The options use tabindex="-1" (focusable
// only programmatically) so interactive() genuinely MISSES them -- its existing
// `tabIndex >= 0` gate does not fire. role=option (and cursor:pointer on the
// clickdiv) is what marks them operable. This is the real <div role=option
// tabindex=-1> case the operability ground truth flags. We assert CANDIDACY, not
// the click effect.
const FIXTURE = `<!doctype html><html><body style="margin:0;font:16px sans-serif">
  <button data-testid="native">Go</button>
  <div role="option" tabindex="-1" data-testid="opt" style="padding:8px">Pick me</div>
  <div data-testid="clickdiv" style="cursor:pointer;padding:8px">Click me</div>
  <div data-testid="decor" style="padding:8px">just text</div>
  <div role="option" tabindex="-1" style="padding:8px">no key, unaddressable</div>
  <script>document.addEventListener('click', () => {}, true);</script>
</body></html>`;

const EXPECTED_TESTID_SELS = [
  'key:testid:clickdiv', // cursor:pointer + keyed -> added
  'key:testid:native',   // native button -> already a candidate
  'key:testid:opt',      // delegated option + keyed -> added (the motivating case)
  // 'key:testid:decor' is deliberately ABSENT: no role/cursor/tabindex.
];

test('snapshot tappables include keyed pointer-operable controls interactive() drops', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(FIXTURE);
    const snap = await snapshot(page, []);
    const sels = snap.tappables.map((t) => t.sel);

    // Exact set of testid-keyed candidates: native + opt + clickdiv present,
    // decor absent. The keyless operable <div> contributes no testid selector
    // (correctly excluded: a repro could not address it anyway).
    const testidSels = sels.filter((s) => s.startsWith('key:testid:')).sort();
    assert.deepStrictEqual(testidSels, EXPECTED_TESTID_SELS);

    // No selector appears twice (dedup against the role-indexed tappables).
    assert.strictEqual(new Set(sels).size, sels.length, 'no duplicate selectors');
  } finally {
    await browser.close();
  }
});

test('snapshot tappables are deterministic across repeated captures', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(FIXTURE);
    const a = (await snapshot(page, [])).tappables.map((t) => t.sel).sort();
    const b = (await snapshot(page, [])).tappables.map((t) => t.sel).sort();
    assert.deepStrictEqual(a, b, 'same DOM -> same candidate set');
  } finally {
    await browser.close();
  }
});

test('accessibleName aggregates the subtree: a logo link (img alt) is labeled, a nameless icon link is not', async () => {
  // The ARIA accessible-name algorithm aggregates the subtree, so a link wrapping
  // <img alt="..."> or <svg><title> is LABELED -- flagging those as unlabeled was
  // a false positive on the common logo/icon-link pattern (surfincubator's logo).
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(`<!doctype html><html><body style="margin:0;font:16px sans-serif">
      <a href="/" data-testid="logo" style="display:inline-block"><img src="x.png" alt="SURF Incubator" style="width:40px;height:40px"></a>
      <a href="/y" data-testid="svgtitled" style="display:inline-block"><svg width="24" height="24"><title>Search</title></svg></a>
      <a href="/x" data-testid="icononly" style="display:inline-block"><svg width="24" height="24"></svg></a>
    </body></html>`);
    const by = Object.fromEntries((await snapshot(page, [])).tappables.map((t) => [t.sel, t]));
    assert.equal(by['key:testid:logo']?.unlabeled, false, 'img-alt link should be labeled');
    assert.equal(by['key:testid:svgtitled']?.unlabeled, false, 'svg-title link should be labeled');
    assert.equal(by['key:testid:icononly']?.unlabeled, true, 'nameless icon link should be flagged unlabeled');
  } finally {
    await browser.close();
  }
});
