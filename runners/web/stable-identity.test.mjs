import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chromium } from 'playwright';
import { snapshot, gtCollect, tap, redactNetworkValue, redactNetworkHeaders } from './runner.mjs';

test('causal network capture redacts recursively before persistence', () => {
  assert.deepEqual(
    redactNetworkValue({ profile: { email: 'a@example.com', name: 'Ada' }, token: 'raw' }),
    {
      profile: {
        email: { $reproit: { redacted: true, type: 'string', length: 13 } },
        name: 'Ada',
      },
      token: { $reproit: { redacted: true, type: 'string', length: 3 } },
    },
  );
  assert.deepEqual(
    redactNetworkHeaders({ authorization: 'Bearer raw', 'content-type': 'application/json' }),
    { authorization: '<reproit:secret>', 'content-type': 'application/json' },
  );
});

test('auth purposes are structural and locale independent', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    await page.setContent(`<label>Número de teléfono<input data-testid="phone" type="tel" autocomplete="tel"></label>`);
    const spanish = await snapshot(page, []);
    assert.equal(spanish.tappables[0].purpose, 'phone');
    await page.locator('label').evaluate((el) => { el.firstChild.textContent = 'Номер телефона'; });
    const russian = await snapshot(page, []);
    assert.equal(russian.tappables[0].purpose, 'phone');
    assert.equal(russian.sig, spanish.sig, 'translated copy cannot change structural identity');
  } finally {
    await browser.close();
  }
});

test('arbitrary DOM id allocator churn is not canonical identity', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(`
      <button id="friendly-human-readable-17">Support</button>
      <a id="x9Q-4f7a-opaque" href="#">FAQ</a>
    `);
    const before = await snapshot(page, []);
    await page.locator('button').evaluate((el) => { el.id = 'friendly-human-readable-18'; });
    await page.locator('a').evaluate((el) => { el.id = 'totally-different-shape'; });
    const after = await snapshot(page, []);

    assert.equal(after.sig, before.sig, 'implementation-only id churn is the same screen');
    assert.deepEqual(after.tappables.map((el) => el.sel), before.tappables.map((el) => el.sel));
    assert.ok(after.tappables.every((el) => !el.sel.startsWith('key:id:')));
    assert.deepEqual(
      (await gtCollect(page)).map((el) => el.sel),
      after.tappables.map((el) => el.sel),
      'ground truth and replay use the same structural fallback',
    );
  } finally {
    await browser.close();
  }
});

test('explicit semantic identity remains canonical and replayable', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(`
      <button id="allocated-1" data-testid="checkout-step-1">Continue</button>
      <input id="allocated-2" name="account_email">
    `);
    const before = await snapshot(page, []);
    assert.deepEqual(before.tappables.map((el) => el.sel), [
      'key:testid:checkout-step-1',
      'key:name:account_email',
    ]);

    await page.locator('button').evaluate((el) => {
      el.id = 'allocated-999';
      el.dataset.testid = 'checkout-step-2';
    });
    const after = await snapshot(page, []);
    assert.notEqual(after.sig, before.sig, 'semantic author contract distinguishes real state');
    assert.equal(after.tappables[0].sel, 'key:testid:checkout-step-2');

    await page.locator('input').evaluate((el) => { el.name = 'billing_email'; });
    const renamed = await snapshot(page, []);
    assert.notEqual(renamed.sig, after.sig, 'name is also an explicit semantic contract');
    assert.equal(renamed.tappables[1].sel, 'key:name:billing_email');
  } finally {
    await browser.close();
  }
});

test('positional replay identity survives a different viewport height', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 300 } });
    await page.setContent(`
      <button>Header action</button>
      <div style="height:700px"></div>
      <button id="target" onclick="window.hit=(window.hit||0)+1">Checkout</button>
    `);

    // The production SDK assigns Checkout button#1 across every style-visible
    // control. It is below this runner's fold, but must keep that identity.
    const top = await snapshot(page, []);
    assert.deepEqual(top.tappables.map((el) => el.sel), ['role:button#0']);

    await page.locator('#target').evaluate((el) => el.scrollIntoView({ block: 'center' }));
    const scrolled = await snapshot(page, []);
    assert.ok(scrolled.tappables.some((el) => el.sel === 'role:button#1'));

    await page.evaluate(() => window.scrollTo(0, 0));
    assert.equal(await tap(page, 'role:button#1'), true);
    assert.equal(await page.evaluate(() => window.hit), 1);
  } finally {
    await browser.close();
  }
});
