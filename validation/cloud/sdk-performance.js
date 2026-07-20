#!/usr/bin/env node
'use strict';

const assert = require('assert');
const fs = require('fs');
const { performance } = require('perf_hooks');
const ReproIt = require('../../sdk/reproit-web.js');

const fingerprintRuns = Number(process.env.REPROIT_WEB_BENCH_FINGERPRINT_RUNS || 100000);
const flushRuns = Number(process.env.REPROIT_WEB_BENCH_FLUSH_RUNS || 10000);
const out =
  process.env.REPROIT_WEB_BENCH_OUT ||
  require('path').join(__dirname, 'artifacts', 'sdk-latest.json');
const sentinel = 'private-sdk-benchmark-value';

let started = performance.now();
let fingerprint;
for (let index = 0; index < fingerprintRuns; index += 1) {
  fingerprint = ReproIt.fingerprintValue(`José مرحبا ${sentinel} ${index}`);
}
const fingerprintMicros = ((performance.now() - started) * 1000) / fingerprintRuns;
assert.equal(JSON.stringify(fingerprint).includes(sentinel), false);

const bodies = [];
global.fetch = (_url, options) => {
  if (bodies.length < 2) bodies.push(options.body);
  return { catch() {} };
};
ReproIt._cfg = {
  appId: 'sdk-benchmark',
  endpoint: 'https://ingest.example/v1/events',
  key: 'pk_benchmark',
  onEvent: null,
};
ReproIt._build = { version: 'benchmark' };
started = performance.now();
for (let index = 0; index < flushRuns; index += 1) {
  ReproIt._buf = Array.from({ length: 50 }, (_, eventIndex) => ({
    kind: 'edge',
    from: 'home',
    action: `tap:key:testid:item-${eventIndex}`,
    to: 'cart',
    t: index,
  }));
  ReproIt._flush();
}
const flushMicros = ((performance.now() - started) * 1000) / flushRuns;
delete global.fetch;

const batch = JSON.parse(bodies[0]);
assert.equal(batch.version, 1);
assert.equal(batch.frames.length, 50);
assert.ok(
  fingerprintMicros < 100,
  `fingerprint cost ${fingerprintMicros.toFixed(2)}us exceeds 100us`,
);
assert.ok(flushMicros < 500, `50-event flush cost ${flushMicros.toFixed(2)}us exceeds 500us`);

const result = {
  fingerprintRuns,
  fingerprintMicros: Number(fingerprintMicros.toFixed(3)),
  flushRuns,
  flush50EventsMicros: Number(flushMicros.toFixed(3)),
  redaction: {
    rawFingerprintValueAbsent: true,
  },
};
fs.mkdirSync(require('path').dirname(out), { recursive: true });
fs.writeFileSync(out, `${JSON.stringify(result, null, 2)}\n`);
process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
