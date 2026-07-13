// Parity gate for the Electron and Tauri runners' canonical structural
// signature. Imports the host-pure signatureOf/descriptorOf exported by each
// runner, loads the golden vectors (signature_vectors.json at the repo root),
// and asserts signatureOf(anchor, tree) === expected_sig for ALL vectors, for
// BOTH runners. These three lines mirror the Rust oracle's golden_vectors_match
// gate (docs/signature.md "Parity gate"): every implementation must reproduce
// the same hashes bit-for-bit or CI fails.
//
// Run: node runners/signature_test.mjs

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve as resolvePath } from 'node:path';

import { signatureOf as electronSig, descriptorOf as electronDesc } from './electron.mjs';
import { signatureOf as tauriSig, descriptorOf as tauriDesc } from './tauri.mjs';

const __dirname = dirname(fileURLToPath(import.meta.url));
// signature_vectors.json lives at the repo root; runners/ is one level down.
const VECTORS_PATH = resolvePath(__dirname, '..', 'signature_vectors.json');

function loadVectors() {
  const text = readFileSync(VECTORS_PATH, 'utf8');
  return JSON.parse(text);
}

function runOne(name, signatureOf, descriptorOf, vectors) {
  let pass = 0;
  const failures = [];
  for (const v of vectors) {
    const got = signatureOf(v.anchor, v.tree);
    if (got === v.expected_sig) {
      pass++;
    } else {
      failures.push({
        description: v.description,
        expected: v.expected_sig,
        got,
        descriptor: descriptorOf(v.anchor, v.tree),
      });
    }
  }
  return { name, total: vectors.length, pass, failures };
}

// The golden set has 25 vectors today (structural + anchor + value-state + one
// non-ASCII/Unicode vector). The parity gate asserts ALL of them for BOTH
// runners; a drift in either the vector file or a runner's signature
// implementation fails CI.
const EXPECTED_VECTOR_COUNT = 25;

function main() {
  const vectors = loadVectors();
  if (vectors.length < EXPECTED_VECTOR_COUNT) {
    console.error(`FAIL: need >= ${EXPECTED_VECTOR_COUNT} vectors, got ${vectors.length}`);
    process.exit(1);
  }

  const results = [
    runOne('electron.mjs', electronSig, electronDesc, vectors),
    runOne('tauri.mjs', tauriSig, tauriDesc, vectors),
  ];

  let allPass = true;
  for (const r of results) {
    if (r.failures.length === 0) {
      console.log(`PASS  ${r.name}: ${r.pass}/${r.total} vectors`);
    } else {
      allPass = false;
      console.log(`FAIL  ${r.name}: ${r.pass}/${r.total} vectors`);
      for (const f of r.failures) {
        console.log(`  - ${f.description}`);
        console.log(`      expected ${f.expected} got ${f.got}`);
        console.log(`      descriptor = ${JSON.stringify(f.descriptor)}`);
      }
    }
  }

  if (!allPass) {
    console.error('\nParity gate FAILED.');
    process.exit(1);
  }
  console.log(`\nAll ${vectors.length} vectors pass for both runners.`);
}

main();
