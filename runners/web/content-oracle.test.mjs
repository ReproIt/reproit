// Validates the CONTENT-BUG oracle (detectContentBugs) after the FP-safe
// tightening: it FIRES only on a GROUND-TRUTH artifact impossible to render as
// legitimate copy (an unrendered `{{ ... }}` / `${ ... }` template binding, or a
// literal `[object Object]`), and stays SILENT on the former false positives -- a
// bare `undefined`/`null`/`NaN` word in real prose, and any template/markup syntax
// shown inside a CODE context (docs, code samples, editable fields).
//
// Uses a REAL Chromium via Playwright: the predicate is DOM-bound (own-text scan,
// ancestor walk for the code-context guard, computed-style visibility), so jsdom
// cannot stand in. Run with `node --test`.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { detectContentBugs } from './runner.mjs';

test(
  'detectContentBugs FIRES on an unrendered template binding and [object ' + 'Object]',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      await page.setContent(
        '<!doctype html><html><body>' +
          '<h1 id="greeting">Hello {{ user.name }}</h1>' + // moustache binding survived
          '<p id="total">Total: ${cart.total}</p>' + // template-literal binding survived
          '<span id="row">[object Object]</span>' + // object coerced to a string
          '</body></html>',
      );
      const items = await page.evaluate(detectContentBugs);
      assert.ok(
        items.some((i) => i.key === 'id:greeting' && i.reason === 'unrendered-template'),
        `expected an unrendered-template finding for #greeting, got ${JSON.stringify(items)}`,
      );
      assert.ok(
        items.some((i) => i.key === 'id:total' && i.reason === 'unrendered-template'),
        `expected an unrendered-template finding for #total, got ${JSON.stringify(items)}`,
      );
      assert.ok(
        items.some((i) => i.key === 'id:row' && i.reason === 'object-object'),
        `expected an object-object finding for #row, got ${JSON.stringify(items)}`,
      );
    } finally {
      await browser.close();
    }
  },
);

test('detectContentBugs is SILENT on bare undefined/null/NaN words in real ' + 'copy', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    // Legitimate copy that merely CONTAINS the words as whole tokens: dropping the
    // bare-word match is exactly the FP fix.
    await page.setContent(
      '<!doctype html><html><body>' +
        '<p id="a">This is undefined behavior in C.</p>' +
        '<p id="b">Ship it to Null Island, coordinates null.</p>' +
        '<p id="c">The result was NaN until we clamped it.</p>' +
        '</body></html>',
    );
    const items = await page.evaluate(detectContentBugs);
    assert.deepStrictEqual(
      items,
      [],
      `bare undefined/null/NaN prose must not fire, got ${JSON.stringify(items)}`,
    );
  } finally {
    await browser.close();
  }
});

test('detectContentBugs SKIPS template/markup syntax inside a CODE context', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    // Documentation / code samples legitimately DISPLAY binding syntax as text.
    await page.setContent(
      '<!doctype html><html><body>' +
        '<code id="c1">{{ user.name }}</code>' +
        '<pre id="p1">const x = `${a.b}`;</pre>' +
        '<div contenteditable="true" id="e1">type {{ here }}</div>' +
        '<textarea id="t1">{{ raw }}</textarea>' +
        '</body></html>',
    );
    const items = await page.evaluate(detectContentBugs);
    assert.deepStrictEqual(
      items,
      [],
      `code-context syntax must not fire, got ${JSON.stringify(items)}`,
    );
  } finally {
    await browser.close();
  }
});

test('detectContentBugs is deterministic across repeated captures', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(
      '<!doctype html><html><body><span id="row">[object Object]</span>' +
        '<h1 id="g">Hello {{ name }}</h1></body></html>',
    );
    const a = await page.evaluate(detectContentBugs);
    const b = await page.evaluate(detectContentBugs);
    assert.deepStrictEqual(a, b, 'same DOM -> same content-bug findings');
  } finally {
    await browser.close();
  }
});

test(
  'detectContentBugs is SILENT on "[object Object]" mentioned in ' + 'explanatory PROSE',
  async () => {
    const browser = await chromium.launch();
    try {
      const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
      // Documentation that EXPLAINS the artifact (Vue's FAQ page: this was the
      // vuejs.org false positive). A short field-label leak still fires.
      await page.setContent(
        '<!doctype html><html><body>' +
          ('<span id="doc">The rendered result will be [object Object] because ' +
            'objects have no default string form.</span>') +
          '<span id="leak">Price: [object Object]</span>' +
          '<span id="bare">[object Object]</span>' +
          '</body></html>',
      );
      const items = await page.evaluate(detectContentBugs);
      const keys = items.map((i) => i.key).sort();
      assert.ok(
        !keys.includes('id:doc'),
        `prose mentioning the phrase must not fire, got ${JSON.stringify(items)}`,
      );
      assert.ok(
        keys.includes('id:leak'),
        'a short field-label leak ("Price: [object Object]") must still fire',
      );
      assert.ok(keys.includes('id:bare'), 'a bare "[object Object]" label must still fire');
    } finally {
      await browser.close();
    }
  },
);

test('detectContentBugs is SILENT on "{{ }}" template syntax mentioned in ' + 'PROSE', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    // Documentation prose that mentions the template braces as syntax, NOT in a code
    // block (this was the svelte.dev FAQ false positive). A standalone unrendered
    // binding still fires.
    await page.setContent(
      '<!doctype html><html><body>' +
        ('<span id="doc">As with actions and transitions, animations can have ' +
          'parameters. The double {{curly}} braces are Svelte syntax.</span>') +
        '<span id="leak">{{ user.name }}</span>' +
        '<span id="dollar">Total: ${price}</span>' +
        '</body></html>',
    );
    const items = await page.evaluate(detectContentBugs, []);
    const keys = items.map((i) => i.key);
    assert.ok(
      !keys.includes('id:doc'),
      `prose mentioning {{ }} must not fire, got ${JSON.stringify(items)}`,
    );
    assert.ok(keys.includes('id:leak'), 'a standalone unrendered {{ binding }} must still fire');
    assert.ok(keys.includes('id:dollar'), 'a short-label ${price} leak must still fire');
  } finally {
    await browser.close();
  }
});

test('detectContentBugs is SILENT on a reflected fuzzer-injected probe', async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    // reproit's own XSS/template-injection probe reflected into a label (this was
    // the vuejs.org/about/faq.html <strong> false positive).
    const probe = '"><img src=x onerror=alert(1)>{{7*7}}';
    await page.setContent(
      '<!doctype html><html><body>' +
        `<strong id="reflected">${probe}</strong>` +
        '<span id="realbug">{{ user.name }}</span>' +
        '</body></html>',
    );
    // With the probe in INJECTED_VALUES, the reflection is suppressed; a genuine
    // unrendered binding NOT in the injected set still fires.
    const items = await page.evaluate(detectContentBugs, [probe]);
    const keys = items.map((i) => i.key);
    assert.ok(
      !keys.includes('id:reflected'),
      `a reflected fuzzer probe must not fire, got ${JSON.stringify(items)}`,
    );
    assert.ok(keys.includes('id:realbug'), 'a genuine unrendered binding must still fire');
  } finally {
    await browser.close();
  }
});
