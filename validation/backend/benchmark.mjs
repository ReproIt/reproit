import assert from 'node:assert/strict';
import { performance } from 'node:perf_hooks';
import { beginBackendTrace } from './sdk-node.mjs';

const runs = Number(process.env.REPROIT_BACKEND_BENCH_RUNS || 50_000);

let started = performance.now();
for (let index = 0; index < runs; index += 1) {
  assert.equal(beginBackendTrace({}, { operation: 'getAccount' }), null);
}
const inactiveMicros = ((performance.now() - started) * 1000) / runs;

started = performance.now();
let encoded;
for (let index = 0; index < runs; index += 1) {
  const trace = beginBackendTrace({ 'x-reproit-trace': `bench-${index}` }, {
    operation: 'getAccount',
    input: { accountId: index, authorization: 'must-not-leak' },
  });
  trace.finish({ account: { id: index, email: 'must-not-leak@example.test' } });
  encoded = trace.header();
}
const activeMicros = ((performance.now() - started) * 1000) / runs;
const decoded = Buffer.from(encoded, 'base64url').toString('utf8');
assert.equal(decoded.includes('must-not-leak'), false);
assert.ok(inactiveMicros < 5, `inactive adapter cost ${inactiveMicros.toFixed(2)}us exceeds 5us`);
assert.ok(activeMicros < 100, `active adapter cost ${activeMicros.toFixed(2)}us exceeds 100us`);
assert.ok(encoded.length < 60_000);

process.stdout.write(`${JSON.stringify({
  runs,
  inactiveMicros: Number(inactiveMicros.toFixed(3)),
  activeMicros: Number(activeMicros.toFixed(3)),
  evidenceBytes: encoded.length,
})}\n`);
