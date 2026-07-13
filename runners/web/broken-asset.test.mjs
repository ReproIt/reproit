// Validates the BROKEN-ASSET tightening (FP hardening):
//   1) a favicon / touch-icon (browser chrome, never painted into content) is NOT
//      flagged even when it fails to load;
//   2) an asset that only exists because a FUZZER-INJECTED value was reflected into
//      the DOM (the XSS-probe `<img src=x>` typed into a field the app echoes) is
//      excluded by provenance, and so is fuzzer-typed tofu;
//   3) a GENUINE dead <img> and genuine app tofu still fire.
// Browser-backed (brokenAssetScan reads live DOM/resource status). Run `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { brokenAssetScan } from './hygiene-oracles.mjs';

// The exact adversarial injection payload the fuzzer types (runner.mjs ADVERSARIAL).
const INJECT = '"><img src=x onerror=alert(1)>{{7*7}}';

test('favicon/chrome icons are excluded; a genuine dead <img> still fires', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    // Every image request 404s, so each <img> ends up complete with naturalWidth 0.
    await page.route('**/*', (route) => {
      const rt = route.request().resourceType();
      if (rt === 'image') return route.fulfill({ status: 404, body: '' });
      return route.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><title>t</title>' });
    });
    await page.goto('http://asset.test/');
    await page.setContent(`
      <img id="fav" src="/favicon.ico" width="32" height="32">
      <img id="touch" src="/apple-touch-icon.png" width="64" height="64">
      <img id="real" src="/hero.png" width="200" height="120">
    `);
    await page.waitForTimeout(400);
    const out = await page.evaluate(brokenAssetScan, []);
    const keys = out.filter((o) => o.reason === 'img').map((o) => o.detail);
    assert.ok(!keys.some((d) => /favicon|apple-touch-icon/i.test(d)), 'favicon/touch-icon must NOT be flagged');
    assert.ok(keys.some((d) => /hero\.png/.test(d)), 'a genuine dead content <img> must still fire');
  } finally {
    await browser.close();
  }
});

test('a fuzzer-injected <img> is excluded by provenance; a real one is not', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.route('**/*', (route) => {
      const rt = route.request().resourceType();
      if (rt === 'image') return route.fulfill({ status: 404, body: '' });
      return route.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><title>t</title>' });
    });
    await page.goto('http://asset.test/');
    // Simulate the app REFLECTING the fuzzer's XSS probe into a preview area: the
    // broken `<img src=x>` exists only because the fuzzer typed INJECT.
    await page.setContent(`
      <div id="preview"><img id="probe" src="x" width="150" height="90"></div>
      <img id="real" src="/photo.png" width="150" height="90">
    `);
    await page.waitForTimeout(400);
    // With the injected value known, the probe img is attributed to the fuzzer.
    const guarded = await page.evaluate(brokenAssetScan, [INJECT]);
    const gkeys = guarded.filter((o) => o.reason === 'img').map((o) => o.detail);
    assert.ok(!gkeys.includes('x'), 'fuzzer-injected <img src=x> must be excluded by provenance');
    assert.ok(gkeys.some((d) => /photo\.png/.test(d)), 'a real dead <img> alongside it must still fire');
    // Without provenance (no injected values recorded), the probe WOULD have fired,
    // proving the exclusion is provenance-driven, not a blanket skip of src=x.
    const naive = await page.evaluate(brokenAssetScan, []);
    assert.ok(naive.some((o) => o.reason === 'img' && o.detail === 'x'), 'src=x fires when NOT attributable to a fuzz value');
  } finally {
    await browser.close();
  }
});

test('fuzzer-typed tofu is excluded; genuine encoding tofu still fires', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(`
      <p id="app">caf�</p>
      <p id="echo">user said: �� probe</p>
    `);
    await page.waitForTimeout(50);
    const injectedTofu = '�� probe';
    const out = await page.evaluate(brokenAssetScan, [injectedTofu]);
    const tofu = out.filter((o) => o.reason === 'tofu').map((o) => o.detail);
    assert.ok(tofu.some((d) => /caf/.test(d)), 'genuine app tofu must still fire');
    assert.ok(!tofu.some((d) => /probe/.test(d)), 'fuzzer-typed tofu must be excluded by provenance');
  } finally {
    await browser.close();
  }
});

test('an off-screen / hidden / zero-size broken img is NOT flagged (Next.js /_next/image residual)', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.route('**/*', (route) => {
      const rt = route.request().resourceType();
      if (rt === 'image') return route.fulfill({ status: 404, body: '' });
      return route.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><title>t</title>' });
    });
    await page.goto('http://asset.test/');
    await page.setContent(`
      <img id="onscreen" src="/_next/image?url=%2Fvisible.png" width="200" height="120">
      <img id="hidden" src="/_next/image?url=%2Fh.png" width="200" height="120" style="display:none">
      <img id="zerosize" src="/_next/image?url=%2Fz.png" width="0" height="0">
      <div style="position:absolute;top:5000px"><img id="offscreen" src="/_next/image?url=%2Foff.png" width="200" height="120"></div>
    `);
    await page.waitForTimeout(400);
    const out = await page.evaluate(brokenAssetScan, []);
    const imgs = out.filter((o) => o.reason === 'img').map((o) => o.detail);
    // Only the on-screen, visibly-broken image fires; hidden/zero/offscreen do not.
    assert.ok(imgs.some((d) => /visible\.png/.test(d)), 'a visibly-rendered broken img still fires');
    assert.ok(!imgs.some((d) => /(%2Fh|%2Fz|%2Foff)/.test(d)), 'hidden / zero-size / off-screen broken imgs must NOT fire');
  } finally {
    await browser.close();
  }
});

test('a webfont whose FontFace errored but text renders fine is NOT flagged (no font-status path)', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    // A @font-face pointing at a missing file: FontFace.status becomes 'error', but
    // the text renders in the system fallback with NO tofu. Must stay silent.
    await page.setContent(`
      <style>@font-face { font-family: Ghost; src: url('/missing-font.woff2'); }</style>
      <p style="font-family: Ghost, sans-serif">Perfectly readable fallback text</p>
    `);
    await page.waitForTimeout(200);
    const out = await page.evaluate(brokenAssetScan, []);
    assert.ok(!out.some((o) => o.reason === 'font'), 'a font-status error with clean rendered text must NOT fire');
  } finally {
    await browser.close();
  }
});
