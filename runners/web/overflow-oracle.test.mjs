import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { confirmLayoutOverflow, layoutOverflowScan } from './overflow-oracle.mjs';

async function scan(markup) {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent('<!doctype html><body>' + markup + '</body>');
    const first = await page.evaluate(layoutOverflowScan);
    const second = await page.evaluate(layoutOverflowScan);
    return confirmLayoutOverflow(first, second);
  } finally {
    await browser.close();
  }
}

test('captures stable text geometry outside an explicit container', async () => {
  const result = await scan(
    '<section id="card" data-reproit-contain style="position:relative;width:120px;height:40px">' +
      '<span id="message" style="position:absolute;left:4px;top:4px;white-space:nowrap">' +
      'a message that is much wider than its card' +
      '</span></section>',
  );
  assert.strictEqual(result.complete, true);
  assert.strictEqual(result.checks.length, 1);
  assert.strictEqual(result.checks[0].subjectKey, 'key:id:message');
  assert.strictEqual(result.checks[0].containerKey, 'key:id:card');
  assert.strictEqual(result.checks[0].stableSamples, 2);
  assert.strictEqual(result.checks[0].policy, 'contain');
  assert.ok(result.checks[0].subjectRect.right > result.checks[0].containerRect.right);
});

test('does not infer a containment contract from visual styling', async () => {
  const result = await scan(
    '<section id="card" style="border:1px solid;width:120px;height:40px">' +
      '<span id="message" style="white-space:nowrap">a message wider than its card</span>' +
      '</section>',
  );
  assert.deepStrictEqual(result.checks, []);
});

test('abstains when a containment identity is ambiguous', async () => {
  const result = await scan(
    '<section id="card" data-reproit-contain>first</section>' +
      '<section id="card" data-reproit-contain>second</section>',
  );
  assert.deepStrictEqual(result.checks, []);
});

test('marks scrolling and ellipsis as intentional policies', async () => {
  const scroll = await scan(
    '<section id="card" data-reproit-contain style="width:120px;overflow:auto">' +
      '<span id="message" style="white-space:nowrap">a message wider than its card</span>' +
      '</section>',
  );
  assert.strictEqual(scroll.checks[0].policy, 'scroll');

  const truncate = await scan(
    '<section id="card" data-reproit-contain style="width:120px">' +
      '<span id="message" style="display:block;overflow:hidden;text-overflow:ellipsis;' +
      'white-space:nowrap">a message wider than its card</span></section>',
  );
  assert.strictEqual(truncate.checks[0].policy, 'truncate');
});

test('turns an oversized state into an explicit bounded evidence defect', async () => {
  const children = Array.from(
    { length: 129 },
    (_, index) => `<span id="message-${index}">message</span>`,
  ).join('');
  const result = await scan(`<section id="card" data-reproit-contain>${children}</section>`);
  assert.strictEqual(result.complete, false);
  assert.strictEqual(result.defect, 'evidence-limit-exceeded');
  assert.deepStrictEqual(result.checks, []);
});

test('distinguishes unavailable capture from an oversized evidence batch', () => {
  const result = confirmLayoutOverflow(null, { complete: true, checks: [] });
  assert.strictEqual(result.complete, false);
  assert.strictEqual(result.defect, 'capture-unavailable');
  assert.deepStrictEqual(result.checks, []);
});
