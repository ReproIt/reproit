// What mounting the express adapter actually costs a request.
//
// benchmark.mjs measures `beginBackendTrace`, the primitive underneath. That
// number is useful and it is NOT the question people ask, which is "what does
// adding this middleware cost me". So this drives a real node:http server over
// a real socket in three shapes and reports the DELTA between them:
//
//   baseline  the handler alone
//   inactive  adapter mounted, request carries no trace context (the shape
//             almost every production request has)
//   active    adapter mounted, request carries `x-reproit-trace`
//
// HTTP, socket and JSON costs are present in all three, so subtracting the
// baseline leaves the adapter. Keep-alive is on, because otherwise connection
// setup dominates and the adapter disappears into the noise, which would
// flatter the result rather than measure it.

import assert from 'node:assert/strict';
import http from 'node:http';
import { performance } from 'node:perf_hooks';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const SDK = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../sdk/reproit-backend-node',
);
const reproitExpress = require(SDK + '/express.js');

const RUNS = Number(process.env.REPROIT_ADAPTER_BENCH_RUNS || 3000);
const WARMUP = Math.min(500, Math.floor(RUNS / 4));

// Express populates these; the adapter reads them, so a faithful stand-in has
// to as well or the measurement is of a smaller object than production sees.
function expressShim(request) {
  const url = new URL(request.url, 'http://127.0.0.1');
  request.path = url.pathname;
  request.query = Object.fromEntries(url.searchParams);
  request.body = { accountId: 42, note: 'benchmark' };
}

function jsonShim(response) {
  response.json = function json(body) {
    const encoded = JSON.stringify(body);
    response.writeHead(response.statusCode || 200, {
      'content-type': 'application/json',
      'content-length': Buffer.byteLength(encoded),
    });
    response.end(encoded);
    return response;
  };
}

function serve(middleware) {
  const server = http.createServer((request, response) => {
    expressShim(request);
    jsonShim(response);
    const handler = () => response.json({ account: { id: 42, ok: true } });
    if (middleware === null) {
      handler();
    } else {
      middleware(request, response, handler);
    }
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

function fire(agent, port, headers) {
  return new Promise((resolve, reject) => {
    const request = http.request(
      { host: '127.0.0.1', port, path: '/account?id=42', agent, headers },
      (response) => {
        response.resume();
        response.on('end', resolve);
      },
    );
    request.on('error', reject);
    request.end();
  });
}

async function measure(middleware, headers) {
  const server = await serve(middleware);
  const { port } = server.address();
  const agent = new http.Agent({ keepAlive: true, maxSockets: 1 });
  try {
    for (let index = 0; index < WARMUP; index += 1) {
      await fire(agent, port, headers);
    }
    const started = performance.now();
    for (let index = 0; index < RUNS; index += 1) {
      await fire(agent, port, { ...headers });
    }
    return ((performance.now() - started) * 1000) / RUNS;
  } finally {
    agent.destroy();
    await new Promise((resolve) => server.close(resolve));
  }
}

// Measured in ALTERNATING rounds, median of each shape, plus a second
// baseline per round. A single pass of each shape put the inactive adapter at
// MINUS 6us, which is not a result, it is drift: the inactive path costs tens
// of nanoseconds and the machine wanders by microseconds between rounds.
// Interleaving cancels the drift, and the gap between the two baselines is the
// method's own noise floor, reported so nobody reads a number smaller than it
// as signal.
const ROUNDS = Number(process.env.REPROIT_ADAPTER_BENCH_ROUNDS || 5);
const adapter = reproitExpress({});
const samples = { baseline: [], control: [], inactive: [], active: [] };
for (let round = 0; round < ROUNDS; round += 1) {
  samples.baseline.push(await measure(null, {}));
  samples.inactive.push(await measure(adapter, {}));
  samples.active.push(await measure(adapter, { 'x-reproit-trace': 'bench-trace' }));
  samples.control.push(await measure(null, {}));
}

const median = (values) => {
  const sorted = [...values].sort((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};

const baseline = median(samples.baseline);
const inactive = median(samples.inactive);
const active = median(samples.active);
// Two identical shapes measured apart: whatever separates them is noise, so a
// smaller difference cannot be called a cost.
const noiseFloor = Math.abs(median(samples.control) - baseline);

const inactiveCost = inactive - baseline;
const activeCost = active - baseline;

// Ceilings, not targets, and sized for a shared CI runner rather than this
// laptop: three local runs put the active cost at 22-26us and the noise floor
// under 3us, so these sit far enough above that ordinary contention cannot
// fail a build, while an adapter that started doing real per-request work
// still would. A gate that flakes gets ignored, and an ignored gate measures
// nothing.
assert.ok(
  noiseFloor < 120,
  `the method's own noise is ${noiseFloor.toFixed(2)}us, too loud for this run to mean anything`,
);
assert.ok(
  inactiveCost < 120,
  `inactive adapter adds ${inactiveCost.toFixed(2)}us per request, over the 120us ceiling`,
);
assert.ok(
  activeCost < 400,
  `active adapter adds ${activeCost.toFixed(2)}us per request, over the 400us ceiling`,
);

process.stdout.write(
  `${JSON.stringify({
    runs: RUNS,
    rounds: ROUNDS,
    noiseFloorMicros: Number(noiseFloor.toFixed(2)),
    baselineMicros: Number(baseline.toFixed(2)),
    inactiveMicros: Number(inactive.toFixed(2)),
    activeMicros: Number(active.toFixed(2)),
    inactiveCostMicros: Number(inactiveCost.toFixed(2)),
    activeCostMicros: Number(activeCost.toFixed(2)),
    inactiveBelowNoiseFloor: inactiveCost < noiseFloor,
  })}\n`,
);
