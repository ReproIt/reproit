// The BEHAVIOURAL half of the selector-identity gate (the structural half is
// validation/self-dogfood/test_runner_selector_space.py). Everything here runs
// the SHIPPED bundles in a REAL Chromium, because the predicates are DOM-bound
// (layout boxes, computed style, hit-testing) and because the claim under test
// is about what the runner actually selects, not about what its source says.
//
// Chromium is the right engine for all three DOM runners' in-page code: the web
// runner drives it directly, the Electron renderer IS Chromium, and the Tauri
// pieces exercised here are plain DOM with no WebKit-specific surface. The one
// thing Chromium cannot stand in for is Tauri's WebDriver TRANSPORT, so the
// Tauri assertions below run its execute() source strings rather than claiming
// a tauri-driver session.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import {
  RESOLVE_STRUCTURAL_TARGET_SRC,
  DETECT_CONTENT_BUGS_SRC,
  resolveStructuralTarget,
  detectContentBugs,
} from '../shared/dom-walk.mjs';
import { snapshotJs } from '../tauri-snapshot.mjs';

// A text field, then a textarea, then a button: the smallest page on which the
// two former grammars disagree. Under the rejected rule `<input type=text>` was
// not a tappable, so the textarea became `role:textfield#0` and the input was
// unaddressable; under the canonical rule the input is #0 and the textarea is
// #1. Nothing is keyed, so every element is addressed by role and index -- the
// case the index space actually has to get right.
const FIXTURE =
  '<!doctype html><html><body>' +
  '<input type="text">' +
  '<textarea></textarea>' +
  '<button>go</button>' +
  '<span>[object Object]</span>' +
  '</body></html>';

async function withPage(body) {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    await page.setContent(FIXTURE);
    return await body(page);
  } finally {
    await browser.close();
  }
}

// Describe what a selector resolves to, via the WebDriver transport (Tauri):
// the shared resolver is interpolated as source into an execute() body. Run
// here through new Function, which is exactly how a WebDriver endpoint would
// evaluate the same string.
function describeViaSource(page, sel) {
  const body = new Function(
    'sel',
    'const resolveStructuralTarget = ' +
      RESOLVE_STRUCTURAL_TARGET_SRC +
      '; const el = resolveStructuralTarget(sel);' +
      "return el ? el.tagName.toLowerCase() + '/' + (el.type || '') : null;",
  );
  return page.evaluate(body, sel);
}

test('a text field occupies its own slot in the role:textfield index space', async () => {
  const seen = await withPage(async (page) => ({
    zero: await describeViaSource(page, 'role:textfield#0'),
    one: await describeViaSource(page, 'role:textfield#1'),
    button: await describeViaSource(page, 'role:button#0'),
  }));
  assert.strictEqual(
    seen.zero,
    'input/text',
    'role:textfield#0 must be the input; the rejected grammar skipped it, which ' +
      'made every finding on a text field unreportable',
  );
  assert.strictEqual(seen.one, 'textarea/textarea');
  assert.strictEqual(seen.button, 'button/submit');
});

test('both transports resolve a selector to the SAME element', async () => {
  await withPage(async (page) => {
    for (const sel of ['role:textfield#0', 'role:textfield#1', 'role:button#0']) {
      const handle = await page.evaluateHandle(resolveStructuralTarget, sel);
      const viaFunction = await page.evaluate(
        (el) => (el ? el.tagName.toLowerCase() + '/' + (el.type || '') : null),
        handle,
      );
      await handle.dispose();
      assert.strictEqual(
        viaFunction,
        await describeViaSource(page, sel),
        `${sel} resolves differently over Playwright than over WebDriver`,
      );
    }
  });
});

test("the Tauri snapshot's index space is the one the shared resolver reads back", async () => {
  await withPage(async (page) => {
    // snapshotJs() is the ACTUAL walk that assigns Tauri's role:<role>#<idx>.
    const snap = await page.evaluate(new Function(snapshotJs([])));
    const byRole = snap.tappables.filter((t) => t.sel.startsWith('role:'));
    assert.ok(byRole.length >= 3, `expected role-addressed tappables, got ${byRole.length}`);
    for (const t of byRole) {
      const handle = await page.evaluateHandle(resolveStructuralTarget, t.sel);
      const resolvedRole = await page.evaluate(
        (el) => (el ? el.tagName.toLowerCase() : null),
        handle,
      );
      await handle.dispose();
      assert.notStrictEqual(
        resolvedRole,
        null,
        `snapshot emitted ${t.sel} but the resolver finds no such element; the ` +
          'assigner and the reader disagree about the index space',
      );
    }
    // The specific pair the two grammars disputed.
    const fields = byRole.filter((t) => t.role === 'textfield').map((t) => t.sel);
    assert.deepStrictEqual(fields, ['role:textfield#0', 'role:textfield#1']);
  });
});

test('an UNKEYED broken-render artifact still produces a content-bug finding', async () => {
  const [viaFunction, viaSource] = await withPage(async (page) => [
    await page.evaluate(detectContentBugs, []),
    await page.evaluate(new Function('a', DETECT_CONTENT_BUGS_SRC), []),
  ]);
  const expected = [{ key: 'tag:span#0', reason: 'object-object', text: '[object Object]' }];
  assert.deepStrictEqual(
    viaFunction,
    expected,
    'a bare <span>[object Object]</span> must be reported; the Electron and ' +
      'Tauri copies dropped it while declaring content-bug supported',
  );
  assert.deepStrictEqual(viaSource, expected, 'the WebDriver body must agree');
});

test('a real pointer tap is blocked by an overlay; el.click() is not', async () => {
  // The reason the Electron runner had to stop calling el.click(). Both taps
  // "succeed" as far as the runner can tell, but only one of them is something
  // a user could have done, and every oracle after the tap judges whatever
  // state the tap produced.
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 400, height: 300 } });
    await page.setContent(
      '<!doctype html><html><body style="margin:0">' +
        '<button id="b" style="position:absolute;left:0;top:0;width:200px;height:100px">go</button>' +
        '<div id="veil" style="position:absolute;left:0;top:0;width:400px;height:300px"></div>' +
        '<script>window.fired = 0;' +
        "document.getElementById('b').addEventListener('click', () => { window.fired++; });" +
        '</script>' +
        '</body></html>',
    );
    const handle = await page.evaluateHandle(resolveStructuralTarget, 'key:id:b');
    const point = await page.evaluate((el) => {
      const r = el.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    }, handle);
    await page.mouse.click(point.x, point.y, { delay: 10 });
    assert.strictEqual(
      await page.evaluate(() => window.fired),
      0,
      'a real pointer click must be intercepted by the covering element',
    );
    await page.evaluate((el) => el.click(), handle);
    assert.strictEqual(
      await page.evaluate(() => window.fired),
      1,
      'el.click() reaches a control no user can reach, which is why it is not ' +
        'a tap',
    );
    await handle.dispose();
  } finally {
    await browser.close();
  }
});
