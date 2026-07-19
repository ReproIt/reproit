// Validates the BLANK-SCREEN zero-FP hardening:
//   1) a LOADING / spinner / skeleton / progress indicator is never flagged (it is
//      a mid-load state, not a white-screen-of-death), even with no text/controls;
//   2) an empty-state message ("No plants added yet") is not blank (it has text);
//   3) a loading-THEN-content app that starts blank is not flagged once settled
//      (settle-then-recheck);
//   4) a PERMANENTLY blank viewport (after settle: no text, no controls, no media,
//      no loading indicator) STILL fires -- the oracle is not neutered.
// Browser-backed via Playwright. Run `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { blankScreenScan } from './hygiene-oracles.mjs';
import { settleForSignature } from './runner.mjs';

test('a visible spinner / skeleton / progress indicator is NOT blank', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    for (const html of [
      '<div class="spinner" style="width:40px;height:40px"></div>',
      '<div class="skeleton-card" style="width:200px;height:80px"></div>',
      '<div role="progressbar" style="width:200px;height:8px"></div>',
      '<progress value="30" max="100"></progress>',
      '<div aria-busy="true" style="width:100px;height:100px"></div>',
    ]) {
      await page.setContent(`<body>${html}</body>`);
      const out = await page.evaluate(blankScreenScan);
      assert.deepEqual(out, [], `a loading indicator must not be blank: ${html}`);
    }
  } finally {
    await browser.close();
  }
});

test('an empty-state message is not blank (it has text)', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent('<body><div><p>No plants added yet.</p></div></body>');
    assert.deepEqual(await page.evaluate(blankScreenScan), [], 'visible text is never blank');
  } finally {
    await browser.close();
  }
});

test('open shadow-root text and substantial painted boxes are visible content', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent('<div id="host"></div>');
    await page.evaluate(() => {
      document.querySelector('#host').attachShadow({ mode: 'open' }).innerHTML = '<p>Visible</p>';
    });
    assert.deepEqual(await page.evaluate(blankScreenScan), []);
    await page.setContent(
      '<div style="width:50px;height:50px;background-color:rgb(34,34,34)"></div>',
    );
    assert.deepEqual(await page.evaluate(blankScreenScan), []);
  } finally {
    await browser.close();
  }
});

test('a loading-then-content app: blank while loading, NOT blank after ' + 'settle', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    // Starts with an EMPTY body (no spinner), then JS injects real content at 250ms:
    // the classic mid-load blank frame the settle-then-recheck must ride out.
    await page.setContent(`<body><div id="app"></div>
      <script>setTimeout(function(){
        document.getElementById('app').innerHTML =
          '<h1>Gift List</h1><p>No gifts added yet.</p><button>Add gift</button>';
      }, 250);</script></body>`);
    // Immediately (mid-load) the scan sees blank...
    const early = await page.evaluate(blankScreenScan);
    assert.ok(early.length > 0, 'the mid-load blank frame is a candidate (pre-settle)');
    // ...but after the settle-then-recheck the caller performs, it is NOT blank.
    await settleForSignature(page);
    const settled = await page.evaluate(blankScreenScan);
    assert.deepEqual(settled, [], 'a loading-then-content app must NOT fire after settle');
  } finally {
    await browser.close();
  }
});

test('a malformed CSS-as-text page (large trapped <style>) is NOT blank', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    // An unclosed <style> traps the whole document as CSS text: the viewport is
    // visually blank, but kilobytes of real content exist in the DOM (a markup bug,
    // not a WSOD). Build a >10KB style blob with no body content.
    const big = '.x{color:red}\n'.repeat(1000); // ~14KB
    await page.setContent(`<head><style>${big}`);
    assert.deepEqual(
      await page.evaluate(blankScreenScan),
      [],
      'a page with a large trapped <style> (CSS-as-text) is a markup bug, not ' + 'blank',
    );
  } finally {
    await browser.close();
  }
});

test('a permanently-blank viewport STILL fires (oracle not neutered)', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent('<body></body>');
    await settleForSignature(page);
    const out = await page.evaluate(blankScreenScan);
    assert.equal(out.length, 1, 'a truly blank body is a white-screen-of-death and must fire');
    assert.equal(out[0].key, 'tag:body');
  } finally {
    await browser.close();
  }
});
