// The structural signature must be STABLE across page loads. Modern UI libraries
// (Radix, base-ui, Headless UI, MUI, Reach) assign element ids from React
// `useId()`, which are RANDOM per render -- e.g. `radix-_R_96bupfdj2mdb_`. Letting
// those into the signature made the SAME screen hash differently each load
// (observed live: /docs/en/home -> 83a736a9 one run, 08a7ae45 the next), breaking
// determinism and repro-id stability. `isEphemeralId` is the guard; this pins it.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isEphemeralId } from './runner.mjs';

test('isEphemeralId flags framework-generated (random) ids', () => {
  for (const id of [
    'radix-_R_96bupfdj2mdb_',                // Radix + React 19 useId
    'radix-_r_4_',
    'radix-_R_13mbupfdj2mdb_',
    'base-ui-_R_el35bsnpflabupfdj2mdb_',     // base-ui wrapper
    ':r0:', ':r1a:',                          // React 18 useId classic
    '«r3»',                         // «r3» internal form
    'headlessui-menu-button-:r7:',            // Headless UI wrapping a useId token
    'mui-42',                                 // MUI sequential
    'reach-1',
  ]) {
    assert.equal(isEphemeralId(id), true, `expected ephemeral: ${id}`);
  }
});

test('isEphemeralId keeps developer-assigned / semantic ids', () => {
  for (const id of [
    'submit-button', 'nav-home', 'user_email', 'searchInput',
    'main', 'app-root', 'price', 'product-42',
    'radixconfig',   // "radix" NOT followed by "-" is a real word, not a wrapper
    'login_form',
  ]) {
    assert.equal(isEphemeralId(id), false, `expected stable: ${id}`);
  }
});

test('isEphemeralId tolerates non-strings', () => {
  assert.equal(isEphemeralId(null), false);
  assert.equal(isEphemeralId(undefined), false);
  assert.equal(isEphemeralId(''), false);
});
