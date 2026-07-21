import assert from 'node:assert/strict';
import test from 'node:test';
import { chromium } from 'playwright';
import { validRouteAccessPath, visitRoute } from './runner.mjs';

test('route-access paths are concrete, bounded, and same-origin', () => {
  assert.equal(validRouteAccessPath('/login'), true);
  assert.equal(validRouteAccessPath('/admin/users'), true);
  assert.equal(validRouteAccessPath('//other.example/login'), false);
  assert.equal(validRouteAccessPath('/login?next=/admin'), false);
  assert.equal(validRouteAccessPath('/white space'), false);
  assert.equal(validRouteAccessPath('/' + 'a'.repeat(256)), false);
});

test('route-access navigation observes a settled client-side redirect', async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  await page.route('http://route-access.test/**', async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/login') {
      await route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<script>setTimeout(() => location.replace("/app"), 100)</script>',
      });
      return;
    }
    await route.fulfill({ status: 200, contentType: 'text/html', body: '<h1>App</h1>' });
  });

  try {
    const result = await visitRoute(page, '/login', 'http://route-access.test');
    assert.deepEqual(result, {
      requested: '/login',
      finalRoute: '/app',
      status: 200,
      settled: true,
    });
  } finally {
    await browser.close();
  }
});
