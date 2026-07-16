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
          document.querySelector('#view').innerHTML = '<button data-testid="checkout">Checkout</button>';
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
  assert.deepEqual(actions.slice(-2), [
    'tap:key:testid:add',
    'tap:key:testid:cart',
  ]);
  console.log('PASS: click-driven navigation preserves the triggering structural action');
} finally {
  await browser.close();
}
