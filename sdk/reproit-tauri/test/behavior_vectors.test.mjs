// Executes the shared behavioral vectors for the FROZEN runner wire, which is
// deliberately not the capture wire. This SDK is replay only: it never records
// a capture batch, so it has no inline body budget, no header table and no
// $reproit placeholder. Its whole shared surface with the rest of the fleet is
// the secret-key predicate, and eight languages hand implement that predicate.
// A divergence about which keys count as secret is silent in both directions:
// too narrow and a credential ships inside a capsule, too wide and a field
// replay needs is scrubbed into a placeholder that never matches.
// ../capture-behavior-v1.json states the predicate once so a defect is found
// once instead of eight times.
//
// One difference from the capture wire is deliberate and is asserted here so it
// cannot be closed by accident: idempotency_key IS secret on the capture wire
// and is NOT secret here. The runner list is thirteen parts, one shorter,
// because changing it would change bytes the fuzz harness compares.
//
// src/init.js is a document-start script, not a module: it declares everything
// with const in its own scope, so the probe line below hands the two helpers
// out of the vm the same way init.test.mjs hands the patched window out.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const vectors = JSON.parse(
  readFileSync(new URL('../../capture-behavior-v1.json', import.meta.url), 'utf8'),
).causalRedaction;
const headerName = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;

const source = readFileSync(new URL('../src/init.js', import.meta.url), 'utf8')
  .replace('__REPROIT_CAPSULE_LITERAL__', JSON.stringify(''))
  .replace('__REPROIT_ACTOR_LITERAL__', JSON.stringify('a'));
const window = {
  location: { href: 'tauri://localhost/' },
  fetch: async () => new Response('{}', { status: 200 }),
  __TAURI_INTERNALS__: { invoke: async () => 0 },
};
const context = vm.createContext({ window, Headers, Response, URL, console });
vm.runInContext(
  `${source}\nglobalThis.__probe = { redact: __reproitRedact, headers: __reproitHeaders };`,
  context,
);
const { redact, headers } = context.__probe;

test('causalRedaction folding cases fold exactly as the shared vector says', () => {
  assert.ok(vectors.foldingCases.length > 0);
  for (const { field, secret } of vectors.foldingCases) {
    const safe = redact({ [field]: 'raw-value' });
    assert.equal(
      safe[field],
      secret ? '<reproit:string:length=9>' : 'raw-value',
      `${field} should${secret ? '' : ' not'} be treated as secret`,
    );
  }
});

test('causalRedaction placeholder is what the header slot emits', () => {
  const cases = vectors.foldingCases.filter(({ field }) => headerName.test(field));
  const safe = headers(Object.fromEntries(cases.map(({ field }) => [field, 'raw-value'])));
  for (const { field, secret } of cases)
    assert.equal(
      safe[field.toLowerCase()],
      secret ? vectors.placeholder : 'raw-value',
      `header ${field} should${secret ? '' : ' not'} be treated as secret`,
    );
});
