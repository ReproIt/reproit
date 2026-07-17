// Validates the BROKEN-ROUTE ground-truth gate after the FP-safe fix:
//   1) the dead-route classifier fires ONLY on 404/410 (genuinely gone), never on
//      405/501 (method semantics), 3xx (redirect), 401/403/429, or 5xx (transient);
//   2) the probe fetches with GET, so a server/CDN that answers HEAD with 501 while
//      GET is 200 (the exact AdminLTE /index2.html false positive) is NOT flagged,
//      while a real GET 404 (e.g. pages/examples/invoice.html) still would be;
//   3) the link filter excludes download links and asset/download extensions from
//      the probe, while keeping navigable .html pages.
//
// (1) and (3) are host-pure and run without a browser. (2) uses a REAL Chromium via
// Playwright with request interception so HEAD and GET can diverge. Run `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { isDeadRouteStatus, isAssetPath } from './runner.mjs';

test('isDeadRouteStatus is true ONLY for 404 and 410', () => {
  assert.equal(isDeadRouteStatus(404), true, '404 not found -> dead');
  assert.equal(isDeadRouteStatus(410), true, '410 gone -> dead');
  // The former false positives: never dead.
  for (const s of [200, 204, 301, 302, 304, 401, 403, 405, 429, 500, 501, 502, 503, 0]) {
    assert.equal(isDeadRouteStatus(s), false, `${s} must NOT be a dead route`);
  }
});

test('isAssetPath excludes downloads/assets but keeps navigable pages', () => {
  // Assets / downloads -> excluded from the route probe.
  for (const p of [
    '/files/app.zip',
    '/docs/manual.pdf',
    '/mac/app.dmg',
    '/win/setup.exe',
    '/release.tar.gz',
    '/bundle.js',
    '/styles/site.css',
    '/img/logo.svg',
    '/data/export.csv',
  ]) {
    assert.equal(isAssetPath(p), true, `${p} is an asset/download -> excluded`);
  }
  // Navigable app routes -> probed (NOT excluded). .html/.htm are pages: a real
  // 404 on pages/examples/invoice.html must still be reachable by the probe.
  for (const p of [
    '/',
    '/pages/examples/invoice.html',
    '/index2.html',
    '/jobs/role/123',
    '/settings',
    '/cart/checkout',
  ]) {
    assert.equal(isAssetPath(p), false, `${p} is a page -> must be probed`);
  }
});

test(
  'a GET probe sees 200 where a HEAD probe would see 501 (the AdminLTE FP)' +
    ', and 404 stays dead',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage();
      // Intercept before the network: /index2.html answers HEAD 501 but GET 200 (a
      // CDN / static host that does not implement HEAD); /invoice.html is genuinely
      // gone (404 on GET too); the document root serves a trivial page.
      await page.route('**/*', (route) => {
        const url = new URL(route.request().url());
        const method = route.request().method();
        if (url.pathname === '/index2.html') {
          return route.fulfill(
            method === 'HEAD'
              ? { status: 501 }
              : { status: 200, contentType: 'text/html', body: 'ok' },
          );
        }
        if (url.pathname === '/invoice.html') {
          return route.fulfill({ status: 404, contentType: 'text/html', body: 'gone' });
        }
        return route.fulfill({
          status: 200,
          contentType: 'text/html',
          body: '<!doctype html><title>root</title>',
        });
      });
      await page.goto('http://route.test/');
      const seen = await page.evaluate(async () => {
        const get = async (p) => (await fetch(p, { method: 'GET', redirect: 'manual' })).status;
        const head = async (p) => (await fetch(p, { method: 'HEAD', redirect: 'manual' })).status;
        return {
          index2Get: await get('/index2.html'),
          index2Head: await head('/index2.html'),
          invoiceGet: await get('/invoice.html'),
        };
      });
      // The old HEAD probe would have seen 501 and (mis)flagged a live page.
      assert.equal(seen.index2Head, 501, 'HEAD lies: 501 method-not-implemented');
      // The new GET probe sees the truth: the page is live.
      assert.equal(seen.index2Get, 200, 'GET sees the live 200');
      assert.equal(
        isDeadRouteStatus(seen.index2Get),
        false,
        'a live GET-200 page is NOT a broken route',
      );
      // A genuinely-gone page is still caught on GET.
      assert.equal(seen.invoiceGet, 404, 'GET sees the real 404');
      assert.equal(isDeadRouteStatus(seen.invoiceGet), true, 'a GET-404 page IS a broken route');
    } finally {
      await browser.close();
    }
  },
);
