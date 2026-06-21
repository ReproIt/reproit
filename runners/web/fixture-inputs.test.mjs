// Validates the property-matched replay path: a fuzz config can carry an
// `inputs` array (written by the CLI's crate::fixture::synthesize from the
// cloud's fixtureSpec), each {field, value} a CONCRETE, property-matched value
// reconstructed from production telemetry. When a `type:` action targets a
// field with a provided input value, the runner must type THAT exact value
// (the synthesized one) instead of only the fixed adversarial-class token, so a
// data-dependent bug actually reproduces.
//
// Two layers:
//   - PURE (no browser): loadInputs normalizes the config, inputValueFor
//     resolves a structural selector to the provided value, and precedence
//     (explicit input wins over the class token) holds.
//   - BROWSER (real Chromium via Playwright, real-Chromium pattern copied from
//     groundtruth-taps.test.mjs incl. the skip-if-no-browser guard): given a
//     resolved fixture value, typeInto writes that exact text into the field.
import { test } from 'node:test';
import assert from 'node:assert';
import { chromium } from 'playwright';
import { typeInto, loadInputs, inputValueFor } from './runner.mjs';

// Browser-backed: skip cleanly where Chromium isn't installed (e.g. the CI
// web-runner job, which runs `node --test` without `npx playwright install`),
// so this never red-flags a browserless environment. The pure tests below run
// everywhere; only the typing test is gated.
let browserUnavailable = false;
try {
  const probe = await chromium.launch();
  await probe.close();
} catch (e) {
  browserUnavailable = `chromium not launchable (${e && e.message ? e.message.split('\n')[0] : e}); skipping`;
}

// A 312-char unicode name with an emoji and a Turkish dotless i: the exact
// property-matched shape crate::fixture::synthesize emits for the motivating
// {minLen:312, charset:"unicode", emoji:true} spec. This is the value the
// runner must type verbatim for the bug to reproduce.
const SYNTH_NAME = '🚀' + 'ıİßçğ'.repeat(80); // 1 + 400 code points

test('loadInputs normalizes {field,value} and tolerates a missing/garbage array', () => {
  assert.deepStrictEqual(
    loadInputs({ inputs: [{ field: 'name', value: 'x' }, { sel: 'key:id:bio', value: '' }] }),
    [{ field: 'name', value: 'x' }, { field: 'key:id:bio', value: '' }],
  );
  // `sel` aliases `field`; numbers coerce to strings; bad entries are dropped.
  assert.deepStrictEqual(
    loadInputs({ inputs: [{ value: 42 }, null, 7, { field: 'zip', value: 90210 }] }),
    [{ field: 'zip', value: '90210' }],
  );
  // No `inputs` key, or a non-array, yields [] (config unaffected).
  assert.deepStrictEqual(loadInputs({}), []);
  assert.deepStrictEqual(loadInputs({ inputs: 'nope' }), []);
  assert.deepStrictEqual(loadInputs(undefined), []);
});

test('inputValueFor matches a structural selector by full sel or by key value', () => {
  const inputs = loadInputs({ inputs: [{ field: 'name', value: SYNTH_NAME }] });
  // field "name" matches the key VALUE of every key:<kind>:name selector.
  assert.strictEqual(inputValueFor('key:id:name', inputs), SYNTH_NAME);
  assert.strictEqual(inputValueFor('key:name:name', inputs), SYNTH_NAME);
  assert.strictEqual(inputValueFor('key:testid:name', inputs), SYNTH_NAME);
  // No match -> null, so the adversarial-class path stays in control.
  assert.strictEqual(inputValueFor('key:id:other', inputs), null);
  assert.strictEqual(inputValueFor('role:textfield#0', inputs), null);
  assert.strictEqual(inputValueFor('key:id:name', []), null);
  // A field that is itself a full selector matches exactly.
  const exact = loadInputs({ inputs: [{ field: 'role:textfield#0', value: 'v' }] });
  assert.strictEqual(inputValueFor('role:textfield#0', exact), 'v');
});

test('precedence: an explicit input value wins over the class token, else null', () => {
  const inputs = loadInputs({ inputs: [{ field: 'name', value: SYNTH_NAME }] });
  // Same resolution the runner's type: branch uses: fixture value when present,
  // null when absent (caller then falls back to the adversarial-class token).
  assert.strictEqual(inputValueFor('key:id:name', inputs), SYNTH_NAME);
  assert.strictEqual(inputValueFor('key:id:title', inputs), null);
  // An empty fixture value is still an explicit win (not null): an empty-field
  // bug reproduces. (null means "no input"; "" means "type empty".)
  const emptyInput = loadInputs({ inputs: [{ field: 'bio', value: '' }] });
  assert.strictEqual(inputValueFor('key:id:bio', emptyInput), '');
});

test('replaying a type action types the exact provided fixture value into the field',
  { skip: browserUnavailable }, async () => {
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
    // A single keyed text field, the kind a `type:key:id:name=...` action targets.
    await page.setContent(
      '<!doctype html><html><body style="margin:0;font:16px sans-serif">' +
      '<input id="name" type="text" style="width:300px;padding:8px">' +
      '</body></html>',
    );

    // Resolve the value the way runSeed's type: branch does: a config input for
    // "name" wins over whatever adversarial-class token the action carried.
    const inputs = loadInputs({ inputs: [{ field: 'name', value: SYNTH_NAME }] });
    const sel = 'key:id:name';
    const value = inputValueFor(sel, inputs);
    assert.strictEqual(value, SYNTH_NAME, 'fixture value resolved for the field');

    const ok = await typeInto(page, sel, value);
    assert.strictEqual(ok, true, 'typeInto resolved the selector and typed');

    // The field holds the EXACT synthesized value, byte-for-byte: the
    // property-matched (312-char unicode + emoji) value reproduced, not a
    // fixed adversarial token.
    const got = await page.$eval('#name', (el) => el.value);
    assert.strictEqual(got, SYNTH_NAME, 'field contains the exact fixture value');
  } finally {
    await browser.close();
  }
});
