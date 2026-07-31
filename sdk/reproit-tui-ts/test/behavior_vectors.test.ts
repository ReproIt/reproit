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
// redactCausal folds a secret string to its length form; the bare
// <reproit:secret> placeholder is produced only by the header slot, so the
// second test drives the real capture path for the fields that are legal HTTP
// header names.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { installCausalFetch, redactCausal } from '../causal.ts';

const vectors = JSON.parse(
  readFileSync(new URL('../../capture-behavior-v1.json', import.meta.url), 'utf8'),
).causalRedaction;
const headerName = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;

test('causalRedaction folding cases fold exactly as the shared vector says', () => {
  assert.ok(vectors.foldingCases.length > 0);
  for (const { field, secret } of vectors.foldingCases) {
    const safe = redactCausal({ [field]: 'raw-value' });
    assert.equal(
      safe[field],
      secret ? '<reproit:string:length=9>' : 'raw-value',
      `${field} should${secret ? '' : ' not'} be treated as secret`,
    );
  }
});

test('causalRedaction placeholder is what the header slot emits', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'reproit-tui-vectors-'));
  const network = join(dir, 'network.ndjson');
  writeFileSync(network, '');
  const prior = globalThis.fetch;
  const env = process.env;
  env.REPROIT_NETWORK_FILE = network;
  globalThis.fetch = (async () =>
    new Response('{}', {
      status: 200,
      headers: { 'content-type': 'application/json' },
    })) as typeof fetch;
  const cases = vectors.foldingCases.filter((c: { field: string }) => headerName.test(c.field));
  try {
    const uninstall = installCausalFetch();
    await fetch('https://app.test/feed', {
      headers: Object.fromEntries(cases.map((c: { field: string }) => [c.field, 'raw-value'])),
    });
    uninstall();
    const exchange = JSON.parse(readFileSync(network, 'utf8').trim());
    for (const { field, secret } of cases)
      assert.equal(
        exchange.requestHeaders[field.toLowerCase()],
        secret ? vectors.placeholder : 'raw-value',
        `header ${field} should${secret ? '' : ' not'} be treated as secret`,
      );
  } finally {
    globalThis.fetch = prior;
    delete env.REPROIT_NETWORK_FILE;
    rmSync(dir, { recursive: true, force: true });
  }
});
