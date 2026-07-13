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
import { chromium, firefox, webkit } from 'playwright';
import {
  brokenAssetScan, installCriticalResourceObserver, criticalResourceScan,
} from './hygiene-oracles.mjs';

// The exact adversarial injection payload the fuzzer types (runner.mjs ADVERSARIAL).
const INJECT = '"><img src=x onerror=alert(1)>{{7*7}}';
const browserType = { chromium, firefox, webkit }[process.env.REPROIT_TEST_BROWSER || 'chromium'];
if (!browserType) throw new Error(`unknown REPROIT_TEST_BROWSER: ${process.env.REPROIT_TEST_BROWSER}`);

async function criticalFacts(page) {
  const facts = new Map();
  let sequence = 0;
  page.on('response', async (resp) => {
    const type = resp.request().resourceType();
    if (type !== 'stylesheet' && type !== 'script') return;
    const observed = ++sequence;
    facts.set(resp.url(), { url: resp.url(), status: resp.status(), contentType: '', resourceType: type, optional: /\/analytics\.js/.test(resp.url()), sequence: observed });
    const headers = await resp.allHeaders().catch(() => ({}));
    if (facts.get(resp.url())?.sequence !== observed) return;
    facts.set(resp.url(), { ...facts.get(resp.url()), contentType: headers['content-type'] || '' });
  });
  page.on('requestfailed', (req) => {
    const type = req.resourceType();
    if (type !== 'stylesheet' && type !== 'script') return;
    const failure = req.failure()?.errorText || 'request failed';
    const cancelled = /(ERR_ABORTED|NS_BINDING_ABORTED|cancelled|canceled)/i.test(failure);
    const observed = ++sequence;
    facts.set(req.url(), { ...(facts.get(req.url()) || {}), url: req.url(), failure, cancelled, resourceType: type, sequence: observed });
  });
  await page.addInitScript(installCriticalResourceObserver);
  return facts;
}

async function waitForFacts(page, facts, count) {
  for (let i = 0; i < 50 && facts.size < count; i++) await page.waitForTimeout(20);
  assert.ok(facts.size >= count, `expected ${count} critical resource facts, got ${facts.size}`);
  for (let i = 0; i < 50 && [...facts.values()].some((fact) => !fact.contentType && !fact.failure); i++) await page.waitForTimeout(20);
}

test('missing CSS and MIME-blocked JavaScript are critical broken assets', async () => {
  const browser = await browserType.launch();
  try {
    const page = await browser.newPage();
    const facts = await criticalFacts(page);
    await page.route('**/*', (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path === '/missing.css') return route.fulfill({ status: 404, contentType: 'text/css', body: '' });
      if (path === '/wrong-mime.js') return route.fulfill({ status: 200, contentType: 'text/html', headers: { 'X-Content-Type-Options': 'nosniff' }, body: '<!doctype html>' });
      return route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<!doctype html><link id="theme" rel="stylesheet" href="/missing.css"><script id="app" src="/wrong-mime.js" defer></script><h1>Styled app</h1>',
      });
    });
    await page.goto('http://asset.test/');
    await waitForFacts(page, facts, 2);
    const out = await page.evaluate(criticalResourceScan, [...facts.values()]);
    assert.ok(out.some((item) => item.key === 'key:id:theme' && item.reason === 'stylesheet-http' && /status=404/.test(item.detail)), JSON.stringify(out));
    assert.ok(out.some((item) => item.key === 'key:id:app'
      && ((item.reason === 'script-mime' && /content-type=text\/html/.test(item.detail))
        || (item.reason === 'script-request' && /NS_ERROR_CORRUPTED_CONTENT/.test(item.detail)))), JSON.stringify(out));
  } finally {
    await browser.close();
  }
});

test('nested CSS imports and JavaScript modules identify the exact failed dependency', async () => {
  const browser = await browserType.launch();
  try {
    const page = await browser.newPage();
    const facts = await criticalFacts(page);
    await page.route('**/*', (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path === '/app.css') return route.fulfill({ status: 200, contentType: 'text/css', body: '@import "/theme.css"; body{color:black}' });
      if (path === '/theme.css') return route.fulfill({ status: 404, contentType: 'text/css', body: '' });
      if (path === '/app.js') return route.fulfill({ status: 200, contentType: 'application/javascript', body: 'import "./checkout.js";' });
      if (path === '/checkout.js') return route.fulfill({ status: 404, contentType: 'application/javascript', body: '' });
      return route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<!doctype html><link id="theme-root" rel="stylesheet" href="/app.css"><script id="app-root" type="module" src="/app.js"></script><h1>Dependencies</h1>',
      });
    });
    await page.goto('http://asset.test/');
    await waitForFacts(page, facts, 4);
    const out = await page.evaluate(criticalResourceScan, [...facts.values()]);
    assert.ok(out.some((item) => item.reason === 'stylesheet-import-http' && /\/theme\.css/.test(item.detail) && /root=http:\/\/asset\.test\/app\.css/.test(item.detail)), JSON.stringify(out));
    assert.ok(out.some((item) => item.reason === 'module-dependency-http' && /\/checkout\.js/.test(item.detail) && /root=http:\/\/asset\.test\/app\.js/.test(item.detail) && /parent=unavailable/.test(item.detail)), JSON.stringify(out));
    assert.ok(!out.some((item) => /app\.(css|js) status=200/.test(item.detail)), JSON.stringify(out));
  } finally {
    await browser.close();
  }
});

test('multiple failed module roots never receive a guessed dependency parent', async () => {
  const browser = await browserType.launch();
  try {
    const page = await browser.newPage();
    const facts = await criticalFacts(page);
    await page.route('**/*', (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path === '/one.js') return route.fulfill({ status: 200, contentType: 'application/javascript', body: 'import "./one-child.js";' });
      if (path === '/two.js') return route.fulfill({ status: 200, contentType: 'application/javascript', body: 'import "./two-child.js";' });
      if (path.endsWith('-child.js')) return route.fulfill({ status: 404, contentType: 'application/javascript', body: '' });
      return route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<!doctype html><script id="one" type="module" src="/one.js"></script><script id="two" type="module" src="/two.js"></script>',
      });
    });
    await page.goto('http://asset.test/');
    await waitForFacts(page, facts, 4);
    const out = await page.evaluate(criticalResourceScan, [...facts.values()]);
    const dependencies = out.filter((item) => item.reason === 'module-dependency-http');
    assert.equal(dependencies.length, 2, JSON.stringify(out));
    assert.ok(dependencies.every((item) => /parent=unavailable/.test(item.detail) && !/root=/.test(item.detail)), JSON.stringify(out));
  } finally {
    await browser.close();
  }
});

test('healthy, third-party, and inactive resources stay silent', async () => {
  const browser = await browserType.launch();
  try {
    const page = await browser.newPage();
    const facts = await criticalFacts(page);
    await page.route('**/*', (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      if (path === '/app.css') return route.fulfill({ status: 200, contentType: 'text/css', body: 'h1{color:green}' });
      if (path === '/app.js') return route.fulfill({ status: 200, contentType: 'application/javascript', body: 'window.ready=true' });
      if (path === '/print.css') return route.fulfill({ status: 404, contentType: 'text/css', body: '' });
      if (path === '/alternate.css') return route.fulfill({ status: 404, contentType: 'text/css', body: '' });
      if (path === '/analytics.js') return route.fulfill({ status: 404, contentType: 'application/javascript', body: '' });
      if (new URL(req.url()).hostname === 'analytics.test') return route.fulfill({ status: 404, body: '' });
      return route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<!doctype html><link rel="stylesheet" href="/app.css"><link rel="stylesheet" media="print" href="/print.css"><link rel="alternate stylesheet" title="Other" href="/alternate.css"><script src="/app.js"></script><script src="/analytics.js"></script><script src="https://analytics.test/tracker.js"></script><h1>Healthy</h1>',
      });
    });
    await page.goto('http://asset.test/');
    await waitForFacts(page, facts, 6);
    assert.deepEqual(await page.evaluate(criticalResourceScan, [...facts.values()]), []);
  } finally {
    await browser.close();
  }
});

test('a successful same-URL retry clears an earlier transient load error', async () => {
  const browser = await browserType.launch();
  try {
    const page = await browser.newPage();
    const facts = await criticalFacts(page);
    let attempts = 0;
    await page.route('**/*', (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path === '/retry.js') {
        attempts++;
        return attempts === 1
          ? route.fulfill({ status: 503, contentType: 'application/javascript', body: '' })
          : route.fulfill({ status: 200, contentType: 'application/javascript', body: 'window.retryLoaded=true' });
      }
      return route.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><h1>Retry</h1>' });
    });
    await page.goto('http://asset.test/');
    await page.evaluate(async () => {
      const load = () => new Promise((resolve) => {
        const script = document.createElement('script');
        script.src = '/retry.js';
        script.onload = script.onerror = resolve;
        document.head.appendChild(script);
      });
      await load();
      await load();
    });
    await waitForFacts(page, facts, 1);
    assert.equal(attempts, 2);
    assert.deepEqual(await page.evaluate(criticalResourceScan, [...facts.values()]), []);
  } finally {
    await browser.close();
  }
});

test('an intentionally cancelled critical request never falls through to load failure', async () => {
  const browser = await browserType.launch();
  try {
    const page = await browser.newPage();
    await page.addInitScript(installCriticalResourceObserver);
    await page.route('**/*', (route) => route.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><script id="cancelled" src="/cancelled.js"></script>' }));
    await page.goto('http://asset.test/');
    await page.evaluate(() => document.getElementById('cancelled').dispatchEvent(new Event('error')));
    const out = await page.evaluate(criticalResourceScan, [{
      url: 'http://asset.test/cancelled.js', failure: 'net::ERR_ABORTED', cancelled: true,
    }]);
    assert.deepEqual(out, []);
  } finally {
    await browser.close();
  }
});

test('favicon/chrome icons are excluded; a genuine dead <img> still fires', async () => {
  const browser = await browserType.launch();
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
  const browser = await browserType.launch();
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
  const browser = await browserType.launch();
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
  const browser = await browserType.launch();
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
  const browser = await browserType.launch();
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
