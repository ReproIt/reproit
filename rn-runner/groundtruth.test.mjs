// Validates the React Native fiber -> EXPLORE:GROUNDTRUTH reducer (no device /
// Appium / emulator needed): feed it the flat fiber records the in-app bridge
// would collect and assert the operability gaps it derives. Run: `node --test`.
//
// The motivating case (docs/operability-graph.md): a <TouchableOpacity onPress>
// with accessible={false} / no accessibilityRole is operable by finger but
// invisible to AT -> a no_role gap. A button with a proper role + label is not.
import { test } from 'node:test';
import assert from 'node:assert';
import { groundtruthFromFiber } from './runner.mjs';

test('a press handler with no a11y role is a no-role gap', () => {
  const records = [
    // The gap: operable (onPress) but accessible={false}, no role/label.
    { id: 'buyBtn', hasPress: true, role: null, label: null, accessible: false },
  ];
  const els = groundtruthFromFiber(records, ['buyBtn']);
  assert.strictEqual(els.length, 1);
  const el = els[0];
  assert.strictEqual(el.id, 'key:buyBtn', 'addressed by its testID/nativeID join key');
  assert.strictEqual(el.operable, true);
  assert.strictEqual(el.a11y.rolePresent, false, 'no role -> the engine counts no_role');
  assert.strictEqual(el.a11y.namePresent, false);
  // We never assert keyboard dims on a touch surface (engine defaults them true).
  assert.strictEqual(el.a11y.keyboardActivatable, undefined);
  assert.strictEqual(el.a11y.inTabOrder, undefined);
});

test('a properly-labelled button is operable but NOT a gap', () => {
  const records = [
    { id: 'save', hasPress: true, role: 'button', label: 'Save', accessible: true },
  ];
  const els = groundtruthFromFiber(records, ['save']);
  assert.strictEqual(els.length, 1);
  assert.strictEqual(els[0].a11y.rolePresent, true);
  assert.strictEqual(els[0].a11y.namePresent, true);
});

test('a non-operable node (no press handler) is never emitted', () => {
  const records = [
    { id: 'layout', hasPress: false, role: null, label: null, accessible: null },
    { id: 'decor', hasPress: false, role: 'image', label: 'logo', accessible: true },
  ];
  assert.deepStrictEqual(groundtruthFromFiber(records, ['layout', 'decor']), []);
});

test('an operable node missing from the native a11y tree has no role', () => {
  // It has onPress AND a role string in JS, but its id never reached the native
  // page source (the join set) -> AT never exposed it -> rolePresent=false.
  const records = [
    { id: 'ghost', hasPress: true, role: 'button', label: 'Hidden', accessible: true },
  ];
  const els = groundtruthFromFiber(records, ['somethingElse']);
  assert.strictEqual(els.length, 1);
  assert.strictEqual(els[0].a11y.rolePresent, false, 'not in native tree -> no role to AT');
});

test('a press node with no join id is still counted (synthetic selector)', () => {
  const records = [
    { id: null, hasPress: true, role: null, label: null, accessible: null },
  ];
  const els = groundtruthFromFiber(records, []);
  assert.strictEqual(els.length, 1);
  assert.match(els[0].id, /^fiber:press#\d+$/);
  assert.strictEqual(els[0].operable, true);
  assert.strictEqual(els[0].a11y.rolePresent, false);
});

test('output is deterministic and sorted by selector', () => {
  const records = [
    { id: 'zeta', hasPress: true, role: 'button', label: 'Z', accessible: true },
    { id: 'alpha', hasPress: true, role: 'button', label: 'A', accessible: true },
  ];
  const a = groundtruthFromFiber(records, ['zeta', 'alpha']);
  const b = groundtruthFromFiber(records, ['zeta', 'alpha']);
  assert.deepStrictEqual(a, b);
  assert.deepStrictEqual(a.map((e) => e.id), ['key:alpha', 'key:zeta']);
});

test('an empty native-id set does not suppress roles (fiber-only mode)', () => {
  // When the page source produced no joinable ids (native.size === 0), we trust
  // the JS-side role rather than flag everything as no-role.
  const records = [
    { id: 'btn', hasPress: true, role: 'button', label: 'Go', accessible: true },
  ];
  const els = groundtruthFromFiber(records, []);
  assert.strictEqual(els[0].a11y.rolePresent, true);
});
