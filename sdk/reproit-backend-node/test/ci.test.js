// CI capture mode (ci.js): a failing test spools a test-trigger capsule, a
// replay run re-executes only the named test and reports the structured
// result marker, and the spool cap drops loudly. Each scenario runs the ci
// wrapper in a child process because capture/replay mode is decided by env
// at suite() time and instrument.install() rewires process-wide clients.
'use strict';

const test = require('node:test');
const assert = require('node:assert');
const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const SDK = path.join(__dirname, '..');
const ci = require('../ci.js');

// One upstream call, one assertion that fails unless FIXED=1. The upstream
// stub only boots outside replay, exactly like a real suite's dependencies.
const FIXTURE = `
const http = require('http');
const assert = require('assert');
const ci = require(process.env.REPROIT_SDK + '/ci.js');
if (!process.env.REPROIT_REPLAY) {
  const server = http.createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ n: 7 }));
  });
  server.listen(19993);
  server.unref();
}
const t = ci.suite('unit');
t('asserts the upstream answer', async () => {
  const response = await fetch('http://127.0.0.1:19993/n');
  const body = await response.json();
  assert.strictEqual(body.n, process.env.FIXED === '1' ? 7 : 8);
});
`;

function run(env) {
  return spawnSync(process.execPath, ['-e', FIXTURE], {
    env: { ...process.env, REPROIT_SDK: SDK, ...env },
    encoding: 'utf8',
  });
}

function tempSpool(label) {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'reproit-ci-' + label + '-'));
}

test('a failing test spools a test-trigger capsule with the exchange', () => {
  const spool = tempSpool('spool');
  const result = run({ REPROIT_CI_CAPTURE: '1', REPROIT_CI_SPOOL: spool });
  assert.notStrictEqual(result.status, 0);
  assert.ok(result.stderr.includes(ci.SPOOL_MARKER), result.stderr);
  const files = fs.readdirSync(spool).filter((name) => name.startsWith('capsule-'));
  assert.strictEqual(files.length, 1);
  const capsule = JSON.parse(fs.readFileSync(path.join(spool, files[0]), 'utf8'));
  assert.strictEqual(capsule.format, 'reproit-backend-capture');
  assert.strictEqual(capsule.version, 2);
  assert.strictEqual(capsule.operation, 'test:unit#asserts the upstream answer');
  assert.strictEqual(capsule.oracle, ci.TEST_FAILURE_ORACLE);
  assert.ok(typeof capsule.envelope.replaySeed === 'string');
  const exchanges = capsule.events.filter((event) => event.exchange);
  assert.strictEqual(exchanges.length, 1);
  assert.strictEqual(exchanges[0].exchange.response.body.n, 7);
  const returned = capsule.events[capsule.events.length - 1];
  assert.strictEqual(returned.success, false);
  assert.ok(String(returned.output.error).includes('7 !== 8'), returned.output.error);
  fs.rmSync(spool, { recursive: true, force: true });
});

test('replay re-runs the named test and reports failed, then passed', () => {
  const spool = tempSpool('replay');
  const captured = run({ REPROIT_CI_CAPTURE: '1', REPROIT_CI_SPOOL: spool });
  assert.notStrictEqual(captured.status, 0);
  const file = fs
    .readdirSync(spool)
    .filter((name) => name.startsWith('capsule-'))
    .map((name) => path.join(spool, name))[0];
  assert.ok(file);
  // No upstream exists in either replay run; the SDK serves the recording.
  const failed = run({ REPROIT_REPLAY: file });
  assert.notStrictEqual(failed.status, 0);
  assert.ok(failed.stderr.includes(ci.RESULT_MARKER), failed.stderr);
  const failedLine = failed.stderr
    .split('\n')
    .find((line) => line.startsWith(ci.RESULT_MARKER));
  const failedReport = JSON.parse(failedLine.slice(ci.RESULT_MARKER.length));
  assert.strictEqual(failedReport.status, 'failed');
  assert.strictEqual(failedReport.operation, 'test:unit#asserts the upstream answer');
  assert.ok(String(failedReport.failure).includes('7 !== 8'));
  const passed = run({ REPROIT_REPLAY: file, FIXED: '1' });
  assert.strictEqual(passed.status, 0, passed.stderr);
  assert.ok(passed.stderr.includes('"status":"passed"'), passed.stderr);
  fs.rmSync(spool, { recursive: true, force: true });
});

test('a full spool drops the capsule and counts the drop', () => {
  const spool = tempSpool('full');
  // Pre-fill the spool to the floor cap so the next capsule cannot fit.
  fs.writeFileSync(path.join(spool, 'existing.json'), 'x'.repeat(4 * 1024));
  const result = run({
    REPROIT_CI_CAPTURE: '1',
    REPROIT_CI_SPOOL: spool,
    REPROIT_CI_SPOOL_MAX: String(4 * 1024),
  });
  assert.notStrictEqual(result.status, 0);
  const capsules = fs.readdirSync(spool).filter((name) => name.startsWith('capsule-'));
  assert.strictEqual(capsules.length, 0);
  const dropped = fs.readFileSync(path.join(spool, 'dropped.count'), 'utf8');
  assert.strictEqual(parseInt(dropped, 10), 1);
  fs.rmSync(spool, { recursive: true, force: true });
});

test('without capture or replay env the wrapper is inert node:test', () => {
  const result = run({});
  assert.strictEqual(result.status, 1);
  assert.ok(!result.stderr.includes(ci.SPOOL_MARKER));
  assert.ok(!result.stderr.includes(ci.RESULT_MARKER));
});

test('unknown suite options are rejected, not ignored', () => {
  assert.throws(() => ci.suite('s', { retries: 2 }), /unknown option/);
});
