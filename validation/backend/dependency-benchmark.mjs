// Per-dependency capture cost for the Node adapter. Each sample creates the
// same bounded trace; active samples additionally append 64 representative
// HTTP exchanges. Alternating baseline/capture/control rounds and the second
// baseline expose the method's own noise floor.
import assert from 'node:assert/strict';
import { performance } from 'node:perf_hooks';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const sdk = require(path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../sdk/reproit-backend-node',
));
const RUNS = Number(process.env.REPROIT_DEPENDENCY_BENCH_RUNS || 300);
const ROUNDS = Number(process.env.REPROIT_DEPENDENCY_BENCH_ROUNDS || 7);
const DEPENDENCIES = 64;
const context = { traceId: 'dependency-benchmark', actionIndex: 1 };
const exchange = {
  request: { method: 'GET', url: 'http://pricing.test/quote?tier=gold' },
  response: { status: 200, body: { price: 42, currency: 'USD' } },
};

function measure(captured) {
  const started = performance.now();
  for (let run = 0; run < RUNS; run += 1) {
    const trace = sdk.BackendTrace.begin(context, 'dependencyBenchmark');
    if (captured) {
      for (let index = 0; index < DEPENDENCIES; index += 1) {
        trace.effect('call', { resource: 'pricing', key: String(index), exchange });
      }
    }
  }
  return ((performance.now() - started) * 1000) / (RUNS * DEPENDENCIES);
}

const samples = { baseline: [], captured: [], control: [] };
for (let round = 0; round < ROUNDS; round += 1) {
  samples.baseline.push(measure(false));
  samples.captured.push(measure(true));
  samples.control.push(measure(false));
}
const median = (values) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
const baseline = median(samples.baseline);
const cost = median(samples.captured) - baseline;
const noiseFloor = Math.abs(median(samples.control) - baseline);
assert.ok(noiseFloor < 10, `dependency benchmark noise ${noiseFloor.toFixed(2)}us is too high`);
assert.ok(cost < 50, `dependency capture adds ${cost.toFixed(2)}us, over the 50us ceiling`);
process.stdout.write(`${JSON.stringify({
  language: 'node', runs: RUNS, rounds: ROUNDS, dependenciesPerTrace: DEPENDENCIES,
  noiseFloorMicros: Number(noiseFloor.toFixed(2)),
  captureCostMicros: Number(cost.toFixed(2)), ceilingMicros: 50,
})}\n`);
