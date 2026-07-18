// Validates the BROKEN-ROUTE tightening (FP hardening):
//   1) collectRouteLinks EXCLUDES rel=nofollow / rel=external (POST-only OAuth
//      buttons -- the OpenStreetMap login-button false 404s), form-submit targets,
//      javascript:/mailto: links and asset extensions; and HONORS <base href> when
//      resolving relative links (so it does not invent a 404 off the wrong base);
//   2) the SPA soft-404 decision (soft404View + isSoftHandled) treats a static-host
//      404 that still hydrates the real app view as NOT a broken route, while a bare
//      error page stays dead.
// Host-pure logic exercised in a REAL DOM via Playwright. Run `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import {
  collectRouteLinks,
  soft404View,
  isSoftHandled,
  ASSET_EXT_SOURCE,
  inspectLinkedRoutes,
  publicRouteKey,
  requestRouteKey,
} from './route-inspection.mjs';
import { snapshot } from './runner.mjs';

test('collectRouteLinks skips nofollow/submit/asset links and keeps real ' + 'routes', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.route('**/*', (route) =>
      route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<!doctype html><title>t</title>',
      }),
    );
    await page.goto('http://app.test/');
    await page.setContent(`
      <a href="/dashboard">Dashboard</a>
      <a href="/settings/">Settings</a>
      <a href="/auth/google" rel="nofollow">Sign in with Google</a>
      <a href="/sponsor" rel="external nofollow">Sponsor</a>
      <a href="/manual.pdf">Manual</a>
      <a href="javascript:void(0)">JS</a>
      <a href="mailto:x@y.z">Mail</a>
      <pre><code><a href="/sample-only">Example route</a></code></pre>
      <form action="/logout" method="post"><a href="/logout" type="submit">Log out</a></form>
      <a href="https://other.test/x">Off-site</a>
    `);
    const links = await page.evaluate(collectRouteLinks, ASSET_EXT_SOURCE);
    assert.deepEqual(
      [...links].sort(),
      ['/dashboard', '/settings'],
        'only same-origin GET routes survive; code/nofollow/submit/asset/js/' +
        'mailto/off-site dropped, trailing slash normalized',
    );
  } finally {
    await browser.close();
  }
});

test('collectRouteLinks honors <base href> when resolving relative links', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.route('**/*', (route) =>
      route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<!doctype html><title>t</title>',
      }),
    );
    await page.goto('http://app.test/app/examples/');
    // A <base> repoints relative resolution: `builder` must resolve under the base,
    // not the document URL. Without base support this manufactured a wrong-path 404.
    await page.setContent(`
      <base href="http://app.test/app/examples/">
      <a href="builder">Builder</a>
    `);
    const links = await page.evaluate(collectRouteLinks, ASSET_EXT_SOURCE);
    assert.ok(
      links.includes('/app/examples/builder'),
      `base-relative link resolved under <base>; got ${JSON.stringify(links)}`,
    );
  } finally {
    await browser.close();
  }
});

test('query routes stay exact internally while evidence redacts secret values', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.route('**/*', (route) =>
      route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<!doctype html><main><a href="/">Home</a></main>',
      }),
    );
    const exact = '/login?returnTo=%2Fdownload&token=top-secret&utm_source=test';
    await page.goto('http://app.test' + exact);
    const snap = await snapshot(page);

    assert.equal(
      requestRouteKey('/login', '?returnTo=%2Fdownload&token=top-secret'),
      '/login?returnTo=%2Fdownload&token=top-secret',
      'the internal request key retains the exact query needed for navigation',
    );
    assert.notEqual(
      publicRouteKey('/login?returnTo=%2Fa'),
      publicRouteKey('/login?returnTo=%2Fb'),
      'ordinary routing parameters remain distinct',
    );
    assert.ok(snap.anchor.includes('returnTo=%2Fdownload'), snap.anchor);
    assert.ok(!snap.anchor.includes('top-secret'), snap.anchor);
    assert.ok(!snap.anchor.includes('utm_source'), snap.anchor);
    assert.ok(snap.anchor.includes('token=%3Credacted%3E'), snap.anchor);
  } finally {
    await browser.close();
  }
});

test('one-hop HTML inspection finds a dead child without recursively crawling', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    const requests = [];
    await page.route('**/*', (route) => {
      const request = route.request();
      const url = new URL(request.url());
      requests.push({ path: url.pathname + url.search, type: request.resourceType() });
      if (url.pathname === '/login' && url.search === '?returnTo=%2Fdownload') {
        return route.fulfill({
          status: 200,
          contentType: 'text/html',
          body:
            '<!doctype html><main data-sig="login-sig"><a href="/download">Download</a>' +
            '<a href="/level-two">More</a></main>',
        });
      }
      if (url.pathname === '/login') {
        return route.fulfill({ status: 400, contentType: 'text/html', body: 'query required' });
      }
      if (url.pathname === '/download') {
        return route.fulfill({
          status: 404,
          contentType: 'text/html',
          body: '<h1>404 - Page not found</h1>',
        });
      }
      if (url.pathname === '/level-two') {
        return route.fulfill({
          status: 200,
          contentType: 'text/html',
          body: '<main><a href="/deep-dead">Deep dead link</a></main>',
        });
      }
      if (url.pathname === '/deep-dead') {
        return route.fulfill({ status: 404, contentType: 'text/html', body: '<h1>404</h1>' });
      }
      if (url.pathname === '/skipped-success') {
        return route.fulfill({
          status: 200,
          contentType: 'text/html',
          body: '<main><a href="/never-inspected">Skipped by inspection cap</a></main>',
        });
      }
      return route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<main><a href="/login?returnTo=%2Fdownload">Login</a></main>',
      });
    });
    await page.goto('http://app.test/');
    const logs = [];
    const result = await inspectLinkedRoutes(page, {
      origin: 'http://app.test',
      seenLinks: new Map([
        ['/login?returnTo=%2Fdownload', 'root-sig'],
        ['/skipped-success', 'root-sig'],
      ]),
      navStatus: {},
      observe: async () => ({
        sig: await page.evaluate(
          () => document.querySelector('[data-sig]')?.getAttribute('data-sig') || null,
        ),
      }),
      log: (line) => logs.push(line),
      fetchCap: 10,
      inspectCap: 1,
      renderCap: 1,
    });

    assert.equal(result.inspected, 1, JSON.stringify(result));
    assert.equal(result.rendered, 1, JSON.stringify(result));
    assert.equal(result.findings, 1, JSON.stringify(result));
    assert.equal(result.coverageGaps, 1, 'a successful page skipped by the cap is explicit');
    assert.ok(
      requests.some(
        (request) =>
          request.type === 'document' &&
          request.path === '/login?returnTo=%2Fdownload',
      ),
      'the successful page is rendered with its exact query',
    );
    assert.ok(
      logs.some(
        (line) =>
          line.includes('EXPLORE:BROKENROUTE') &&
          line.includes('"sig":"login-sig"') &&
          line.includes('"route":"/download"'),
      ),
      logs.join('\n'),
    );
    assert.ok(
      !requests.some((request) => request.path === '/deep-dead'),
      'successful child pages are not recursively inspected',
    );
    assert.ok(
      !requests.some((request) => request.path === '/never-inspected'),
      'a capped successful page is not silently inspected',
    );
    assert.equal(new URL(page.url()).pathname, '/', 'the helper restores the original page');
  } finally {
    await browser.close();
  }
});

test('SPA soft-404 (filled app view) is NOT dead; a bare error page IS dead', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.route('**/*', (route) =>
      route.fulfill({
        status: 200,
        contentType: 'text/html',
        body: '<!doctype html><title>t</title>',
      }),
    );
    await page.goto('http://app.test/');

    // A static host 404s /deep but serves index.html; the router hydrates the real
    // app: a filled #app mount with plenty of controls and no not-found heading.
    let controls = '';
    for (let i = 0; i < 14; i++) controls += `<a href="/x${i}">Item ${i}</a>`;
    await page.setContent(
      `<div id="app"><nav>${controls}</nav><main><h1>Components</h1>` +
        '<button>Go</button></main></div>',
    );
    const appView = await page.evaluate(soft404View);
    assert.ok(
      isSoftHandled(appView),
      `a hydrated app view must be treated as soft-404 (not dead); got ${JSON.stringify(appView)}`,
    );

    // A genuine error page: a not-found heading and little else.
    await page.setContent(`<div id="app"><h1>404 - Page not found</h1><a href="/">Home</a></div>`);
    const errView = await page.evaluate(soft404View);
    assert.ok(
      !isSoftHandled(errView),
      `a bare 404 page must stay dead; got ${JSON.stringify(errView)}`,
    );
  } finally {
    await browser.close();
  }
});
