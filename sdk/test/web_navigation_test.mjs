import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from '../../runners/web/node_modules/playwright/index.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const sdk = path.join(here, '..', 'reproit-web.js');

const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  await page.setContent(`<!doctype html><body>
    <main id="view"><button data-testid="add">Add to cart</button></main>
    <button data-testid="cart">Cart</button>
    <script>
      let added = false;
      document.addEventListener('click', (event) => {
        const control = event.target.closest('button');
        if (!control) return;
        if (control.dataset.testid === 'add') added = true;
        if (control.dataset.testid === 'cart' && added) {
          location.hash = '#/cart';
          document.querySelector('#view').innerHTML =
            '<button data-testid="checkout">Checkout</button>';
        }
      });
    </script>
  </body>`);
  await page.addScriptTag({ path: sdk });
  await page.evaluate(() => {
    window.ReproIt.init({
      appId: 'navigation-contract',
      reportAutomation: true,
      debounceMs: 10,
      flushMs: 60_000,
      onEvent: () => {},
    });
  });
  await page.waitForTimeout(30);

  await page.getByTestId('add').click();
  await page.waitForTimeout(30);
  await page.getByTestId('cart').click();
  await page.waitForTimeout(100);

  const actions = await page.evaluate(() => window.ReproIt._path.map((step) => step.action));
  assert.deepEqual(actions.slice(-2), ['tap:key:testid:add', 'tap:key:testid:cart']);
  console.log('PASS: click-driven navigation preserves the triggering structural ' + 'action');

  const earlyPage = await browser.newPage();
  await earlyPage.setContent(`<!doctype html><body>
    <button data-testid="crash">Checkout</button>
    <script>
      document.querySelector('[data-testid="crash"]').addEventListener('click', () => {
        throw new Error('early checkout crash');
      });
    </script>
  </body>`);
  await earlyPage.addScriptTag({ path: sdk });
  await earlyPage.evaluate(() => {
    window.__events = [];
    window.ReproIt.init({
      appId: 'early-crash-contract',
      reportAutomation: true,
      debounceMs: 1_000,
      flushMs: 60_000,
      onEvent: (event) => window.__events.push(event),
    });
  });
  await earlyPage.getByTestId('crash').click();
  const crash = await earlyPage.evaluate(() =>
    window.__events.find((event) => event.kind === 'error'),
  );
  assert.ok(crash, 'the synchronous click crash must be captured');
  assert.equal(crash.path.length, 1);
  assert.equal(crash.path[0].action, 'tap:key:testid:crash');
  assert.equal(typeof crash.path[0].sig, 'string');
  assert.ok(crash.path[0].sig.length > 0);
  await earlyPage.close();
  console.log('PASS: an early synchronous crash retains an executable structural path');

  const capturePage = await browser.newPage();
  await capturePage.setContent(`<!doctype html><body>
    <button data-testid="open-checkout">Open checkout</button>
    <main id="screen"></main>
    <script>
      document.querySelector('[data-testid="open-checkout"]').addEventListener('click', () => {
        document.querySelector('#screen').innerHTML = '<button data-testid="pay">Pay</button>';
      });
    </script>
  </body>`);
  await capturePage.addScriptTag({ path: sdk });
  await capturePage.evaluate(() => {
    window.__events = [];
    window.ReproIt.init({
      appId: 'tester-capture-contract',
      reportAutomation: true,
      testerCaptureShortcut: true,
      debounceMs: 10,
      flushMs: 60_000,
      onEvent: (event) => window.__events.push(event),
    });
  });
  await capturePage.waitForTimeout(30);
  await capturePage.getByTestId('open-checkout').click();
  await capturePage.waitForTimeout(30);
  await capturePage.keyboard.press('Alt+Shift+B');
  const capture = await capturePage.evaluate(() =>
    window.__events.find((event) => event.oracle === 'tester-capture'),
  );
  assert.ok(capture, 'the tester shortcut must emit a capture');
  assert.equal(capture.path.at(-1).action, 'tap:key:testid:open-checkout');
  assert.equal(capture.findingIdentity.boundary, capture.sig);
  assert.equal(capture.findingIdentity.invariant, 'tester-observed-failure');
  await capturePage.close();
  console.log('PASS: tester capture keeps the rolling structural path and state ' + 'identity');
} finally {
  await browser.close();
}
