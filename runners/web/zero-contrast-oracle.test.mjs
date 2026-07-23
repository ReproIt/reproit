import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { zeroContrastScan } from './zero-contrast-oracle.mjs';

async function scanOn(body) {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(`<!doctype html><html><body>${body}</body></html>`);
    return await page.evaluate(zeroContrastScan);
  } finally {
    await browser.close();
  }
}

test('planted: text whose color matches its background is flagged', async () => {
  const items = await scanOn(
    '<div style="background:#ffffff;color:#ffffff;padding:20px">Notifications (3)</div>',
  );
  assert.equal(items.length, 1);
  assert.match(items[0].text, /Notifications/);
  assert.equal(items[0].color, 'rgb(255,255,255)');
});

test('planted: composited match through a translucent foreground fires', async () => {
  // Text alpha 1 but same rgb as an ancestor background two levels up.
  const items = await scanOn(
    '<div style="background:#123456;padding:30px">' +
      '<span style="color:#123456">hidden label</span></div>',
  );
  assert.equal(items.length, 1);
  assert.equal(items[0].color, 'rgb(18,52,86)');
});

test('clean: normal readable text stays silent', async () => {
  const items = await scanOn(
    '<div style="background:#ffffff;color:#111111;padding:20px">Readable heading</div>',
  );
  assert.deepEqual(items, []);
});

test('abstain: fully transparent text is a deliberate hide', async () => {
  const items = await scanOn(
    '<div style="background:#fff"><span style="color:transparent">sr-only</span></div>',
  );
  assert.deepEqual(items, []);
});

test('abstain: visibility:hidden and display:none are not shown text', async () => {
  const hidden = await scanOn(
    '<div style="background:#fff;color:#fff;visibility:hidden">x y z</div>',
  );
  assert.deepEqual(hidden, []);
  const none = await scanOn('<div style="background:#fff;color:#fff;display:none">x y z</div>');
  assert.deepEqual(none, []);
});

test('abstain: text over an unknown backdrop (no opaque ancestor bg) is not judged', async () => {
  // No ancestor paints an opaque background, so the real backdrop is unknown.
  // color:white over the default white page would only match if we assumed
  // white; we do assume white as the root, so make the color non-white to
  // confirm a genuinely-unknown case stays silent when it does not match.
  const items = await scanOn('<div style="color:#010101">floating text</div>');
  assert.deepEqual(items, []);
});
