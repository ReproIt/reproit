// Planted order-dependent test failure that fires only under CI-like
// conditions, for the flaky-CI wedge (Track 3).
//
// The first test runs ONLY on the CI legacy matrix (CI_LEGACY_MATRIX=1) and
// leaks state into the shared config service: it switches the service to
// its legacy response format, which returns the tax rate as a string. The
// second test then computes a wrong total and fails. A plain local run
// never takes the legacy branch, so the suite passes and the failure looks
// unreproducible ("flaky"). The capsule spooled by the CI run carries the
// recorded legacy response, so `reproit check <capsule> --exec "node
// tests/checkout.test.mjs"` re-executes the exact failing run anywhere.
//
// Run it directly (`node tests/checkout.test.mjs`), not via `node --test`:
// the test runner's child processes would swallow the stderr markers
// `reproit check` parses.
import { createRequire } from 'node:module';
import http from 'node:http';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { orderTotal } from '../order.mjs';

const SDK = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../sdk/reproit-backend-node',
);
const require = createRequire(SDK + '/index.js');
const ci = require(SDK + '/ci.js');

const PORT = 19991;
const CONFIG_URL = 'http://127.0.0.1:' + PORT;

// The shared config service both tests talk to. Stateful on purpose: the
// legacy-format test leaks its toggle into it. Never started under replay,
// where the SDK serves the recorded exchanges in process and any real
// socket attempt would surface as a divergence, not a connection.
let legacy = false;
if (!process.env.REPROIT_REPLAY) {
  const server = http.createServer((req, res) => {
    if (req.method === 'POST' && req.url === '/format/legacy') {
      legacy = true;
      res.writeHead(204);
      res.end();
      return;
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(legacy ? { rate: '0.25' } : { rate: 0.25 }));
  });
  await new Promise((resolve) => server.listen(PORT, resolve));
  server.unref();
}

const test = ci.suite('checkout');

test('legacy config format toggles', async () => {
  // CI-only: this is the state leak that makes the next test order
  // dependent. A local run never takes this branch.
  if (process.env.CI_LEGACY_MATRIX !== '1') return;
  const response = await fetch(CONFIG_URL + '/format/legacy', { method: 'POST' });
  assert.equal(response.status, 204);
});

test('order total applies the configured tax rate', async () => {
  assert.equal(await orderTotal(100, CONFIG_URL), 125);
});
