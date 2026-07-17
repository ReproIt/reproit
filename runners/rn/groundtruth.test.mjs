// Validates the React Native fiber -> EXPLORE:GROUNDTRUTH reducer (no device /
// Appium / emulator needed): feed it the flat fiber records the in-app bridge
// would collect and assert the operability gaps it derives. Run: `node --test`.
//
// The motivating case (docs/operability-graph.md): a <TouchableOpacity onPress>
// with accessible={false} / no accessibilityRole is operable by finger but
// invisible to AT -> a no_role gap. A button with a proper role + label is not.
import { test } from 'node:test';
import assert from 'node:assert';
import {
  groundtruthFromFiber,
  groundtruthFromNative,
  reconcileComposeControls,
} from './runner.mjs';

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
  const records = [{ id: 'save', hasPress: true, role: 'button', label: 'Save', accessible: true }];
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
  const records = [{ id: null, hasPress: true, role: null, label: null, accessible: null }];
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
  assert.deepStrictEqual(
    a.map((e) => e.id),
    ['key:alpha', 'key:zeta'],
  );
});

test('an empty native-id set does not suppress roles (fiber-only mode)', () => {
  // When the page source produced no joinable ids (native.size === 0), we trust
  // the JS-side role rather than flag everything as no-role.
  const records = [{ id: 'btn', hasPress: true, role: 'button', label: 'Go', accessible: true }];
  const els = groundtruthFromFiber(records, []);
  assert.strictEqual(els[0].a11y.rolePresent, true);
});

// ---- NATIVE FALLBACK (groundtruthFromNative) -------------------------------
// On a real Android device the uiautomator2 driver has NO JS transport into the
// RN runtime, so the fiber probe yields nothing. We then derive groundtruth from
// the native a11y tree the runner already walks: a pointer-operable element that
// renders as a bare android.view.ViewGroup (canonical role `group`, no AT role)
// is the WCAG 4.1.2 no_role gap; one that renders as android.widget.Button
// (role `button`) is clean. This mirrors the LIVE com.rnoperability fixture:
//   cleanBtn -> android.widget.Button (rolePresent=true)  -> NOT a gap
//   fakeBtn  -> android.view.ViewGroup (rolePresent=false) -> no_role + pointer_only

test('native fallback: a role-less operable element is a no_role + ' + 'pointer_only gap', () => {
  // fakeBtn: clickable android.view.ViewGroup, accessibilityRole absent ->
  // exposesAtRole=false. The runner sets rolePresent=false; it still has a
  // content-desc so namePresent=true (matching the live fixture's "Buy (fake)").
  const candidates = [{ id: 'fakeBtn', rolePresent: false, namePresent: true }];
  const els = groundtruthFromNative(candidates);
  assert.strictEqual(els.length, 1);
  const el = els[0];
  assert.strictEqual(el.id, 'key:fakeBtn', 'addressed by its resource-id join key');
  assert.strictEqual(el.operable, true);
  assert.strictEqual(
    el.a11y.rolePresent,
    false,
    'ViewGroup with no AT role -> engine counts no_role',
  );
  // Pointer-only: no exposed semantics for a keyboard/switch user to activate.
  assert.strictEqual(el.a11y.keyboardActivatable, false, 'engine counts pointer_only');
  assert.strictEqual(el.a11y.inTabOrder, false);
  assert.strictEqual(el.a11y.focusable, false);
});

test('native fallback: a real Button role is operable but NOT a gap', () => {
  // cleanBtn: android.widget.Button -> canonical role `button` -> exposesAtRole.
  const candidates = [{ id: 'cleanBtn', rolePresent: true, namePresent: true }];
  const els = groundtruthFromNative(candidates);
  assert.strictEqual(els.length, 1);
  assert.strictEqual(els[0].a11y.rolePresent, true);
  assert.strictEqual(els[0].a11y.namePresent, true);
  assert.strictEqual(
    els[0].a11y.keyboardActivatable,
    true,
    'real role -> keyboard-activatable, no gap',
  );
  assert.strictEqual(els[0].a11y.inTabOrder, true);
});

test('native fallback: the live fixture state (cleanBtn clean, fakeBtn gap)', () => {
  // The exact two-candidate set the runner collects on the com.rnoperability
  // home screen, in document order; the reducer sorts by selector.
  const candidates = [
    { id: 'cleanBtn', rolePresent: true, namePresent: true },
    { id: 'fakeBtn', rolePresent: false, namePresent: true },
  ];
  const els = groundtruthFromNative(candidates);
  assert.deepStrictEqual(
    els.map((e) => e.id),
    ['key:cleanBtn', 'key:fakeBtn'],
  );
  const clean = els.find((e) => e.id === 'key:cleanBtn');
  const fake = els.find((e) => e.id === 'key:fakeBtn');
  assert.strictEqual(clean.a11y.rolePresent, true);
  assert.strictEqual(fake.a11y.rolePresent, false);
  assert.strictEqual(fake.a11y.keyboardActivatable, false);
  // Exactly one no_role + pointer_only gap, like the WPF/Flutter validations.
  const gaps = els.filter((e) => e.a11y.rolePresent === false);
  assert.strictEqual(gaps.length, 1, 'fakeBtn alone is the gap');
});

test('native fallback: candidates without an id are skipped', () => {
  // Dev-build chrome (the "Open debugger" warning bubble) is clickable but
  // id-less; the collector never adds it, but guard the reducer too.
  const els = groundtruthFromNative([
    { id: null, rolePresent: false, namePresent: false },
    { rolePresent: false, namePresent: true },
  ]);
  assert.deepStrictEqual(els, []);
});

test('native fallback: output is deterministic and sorted by selector', () => {
  const candidates = [
    { id: 'zeta', rolePresent: true, namePresent: true },
    { id: 'alpha', rolePresent: false, namePresent: false },
  ];
  const a = groundtruthFromNative(candidates);
  const b = groundtruthFromNative(candidates);
  assert.deepStrictEqual(a, b);
  assert.deepStrictEqual(
    a.map((e) => e.id),
    ['key:alpha', 'key:zeta'],
  );
});

test('Compose semantics: keyed generic wrapper and semantic child become one ' + 'control', () => {
  const elements = [
    {
      sel: 'key:save',
      key: 'save',
      role: 'node',
      label: '',
      bounds: [10, 20, 120, 48],
      nokey: false,
    },
    {
      sel: 'role:button#0',
      key: null,
      role: 'button',
      label: 'Save',
      bounds: [10, 20, 120, 48],
      nokey: true,
    },
  ];
  const candidates = [{ id: 'save', rolePresent: false, namePresent: false }];
  const got = reconcileComposeControls(elements, candidates);
  assert.deepStrictEqual(got.elements, [
    {
      sel: 'key:save',
      key: 'save',
      role: 'button',
      label: 'Save',
      bounds: [10, 20, 120, 48],
      nokey: false,
    },
  ]);
  assert.deepStrictEqual(got.nativeCandidates, [
    { id: 'save', rolePresent: true, namePresent: true },
  ]);
  const truth = groundtruthFromNative(got.nativeCandidates);
  assert.strictEqual(truth[0].a11y.rolePresent, true);
  assert.strictEqual(truth[0].a11y.namePresent, true);
});

test('Compose semantics: distinct overlapping controls are not collapsed', () => {
  const elements = [
    { sel: 'key:menu', key: 'menu', role: 'node', label: '', bounds: [0, 0, 100, 40] },
    {
      sel: 'role:button#0',
      key: null,
      role: 'button',
      label: 'Open',
      bounds: [4, 0, 96, 40],
      nokey: true,
    },
  ];
  const got = reconcileComposeControls(elements, []);
  assert.strictEqual(got.elements.length, 2);
  assert.deepStrictEqual(
    got.elements.map((e) => e.sel),
    ['key:menu', 'role:button#0'],
  );
});

test('Compose semantics: role selectors are renumbered after duplicate ' + 'removal', () => {
  const elements = [
    { sel: 'key:first', key: 'first', role: 'node', label: '', bounds: [0, 0, 100, 40] },
    {
      sel: 'role:button#0',
      key: null,
      role: 'button',
      label: 'First',
      bounds: [0, 0, 100, 40],
      nokey: true,
    },
    {
      sel: 'role:button#1',
      key: null,
      role: 'button',
      label: 'Second',
      bounds: [0, 50, 100, 40],
      nokey: true,
    },
  ];
  const got = reconcileComposeControls(elements, []);
  assert.deepStrictEqual(
    got.elements.map((e) => e.sel),
    ['key:first', 'role:button#0'],
  );
});
