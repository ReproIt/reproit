/*!
 * CI capture mode for reproit-backend-node: the flaky-CI wedge.
 *
 * `suite(name)` returns a node:test-compatible `test(name, fn)` whose trigger
 * identity is the TEST (suite + test id), not an inbound HTTP request. With
 * `REPROIT_CI_CAPTURE=1` every test runs inside its own trace, so the
 * instrumented outbound clients (instrument.js) record dependency exchanges
 * and the determinism envelope exactly as production capture does; a FAILING
 * test emits a version-2 `reproit-backend-capture` capsule to a bounded
 * on-disk spool. With `REPROIT_REPLAY` set the SAME wrapper re-runs only the
 * capsule's named test while the SDK serves the recorded exchanges in
 * process, and reports the observed result as a structured stderr marker for
 * `reproit check`. Without either env the wrapper is `node:test` untouched.
 *
 * The wire is the existing capture payload: the test identity rides in the
 * `operation` field as `test:<suite>#<test>`, and the failed assertion is
 * the existing `backend-authored-invariant` registry oracle (a test IS an
 * authored invariant). No new protocol fields, no new oracle ids.
 *
 * Honest limit: replay pins the envelope and the recorded exchanges, which
 * is the whole boundary this SDK can see. A race the boundary cannot see
 * (scheduling, shared memory) is not reproduced by this capsule; `reproit
 * check` reports such runs Inconclusive, never a fake reproduction.
 */
'use strict';

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const nodeTest = require('node:test');

const sdk = require('./index.js');
const instrument = require('./instrument.js');

// Test-trigger identity prefix inside the existing `operation` field.
const TEST_TRIGGER_PREFIX = 'test:';
// The registry oracle a failed test capsule carries: an authored invariant
// (the test's own assertion) was violated. Existing id, not a new one.
const TEST_FAILURE_ORACLE = 'backend-authored-invariant';
// Structured stderr markers `reproit check` parses, like REPROIT:DIVERGENCE.
const RESULT_MARKER = 'REPROIT:CI-TEST ';
const SPOOL_MARKER = 'REPROIT:CI-CAPSULE ';

// Spool bounds. The cap covers the TOTAL bytes on disk, spilled capsules
// beyond it are dropped and counted (in-process stats plus the on-disk
// `dropped.count`), never silently.
const DEFAULT_SPOOL_DIR = '.reproit/ci-spool';
const DEFAULT_SPOOL_MAX_BYTES = 16 * 1024 * 1024;
const SPOOL_MAX_FLOOR_BYTES = 4 * 1024;
const SPOOL_MAX_CEIL_BYTES = 64 * 1024 * 1024;
// Suite and test names share the operation field's 256-code-point bound.
const MAX_NAME = 120;
const MAX_ERROR_CHARS = 2048;

const state = {
  traceSeq: 1,
  stats: { spooledCapsules: 0, droppedCapsules: 0, failedCaptures: 0 },
};

function replayPath() {
  const value = process.env.REPROIT_REPLAY;
  return typeof value === 'string' && value.length > 0 ? value : null;
}

function mode() {
  if (replayPath() !== null) return 'replay';
  if (process.env.REPROIT_CI_CAPTURE === '1') return 'capture';
  return 'off';
}

function boundedName(value) {
  return String(value).trim().slice(0, MAX_NAME);
}

function operationFor(suiteName, testName) {
  return TEST_TRIGGER_PREFIX + boundedName(suiteName) + '#' + boundedName(testName);
}

function boundedError(error) {
  return String((error && error.message) ?? error).slice(0, MAX_ERROR_CHARS);
}

// Synthesized trace context: the CI job stands where production stood.
function ciContext() {
  const commit = [process.env.REPROIT_COMMIT, process.env.GITHUB_SHA].find(
    (value) => typeof value === 'string' && /^[A-Za-z0-9._:-]{1,128}$/.test(value),
  );
  return {
    traceId: 'ci-' + Date.now() + '-' + state.traceSeq++,
    actor: null,
    actionIndex: 0,
    build: commit ?? null,
    configContract: null,
    captureEnvelope: true,
  };
}

// Same envelope shape production capture records; the seed pins the REPLAY
// run's randomness, it does not reproduce the test run's.
function envelopeFor(trace) {
  const first = trace.events()[0] ?? {};
  return {
    observedAtMs: Number.isFinite(first.at) ? first.at : Date.now(),
    tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
    node: process.version,
    os: process.platform,
    arch: process.arch,
    replaySeed: crypto.randomBytes(8).toString('hex'),
  };
}

function spoolDir() {
  const dir = process.env.REPROIT_CI_SPOOL;
  return typeof dir === 'string' && dir.length > 0 ? dir : DEFAULT_SPOOL_DIR;
}

function spoolMaxBytes() {
  const parsed = Number(process.env.REPROIT_CI_SPOOL_MAX);
  if (!Number.isInteger(parsed)) return DEFAULT_SPOOL_MAX_BYTES;
  return Math.min(SPOOL_MAX_CEIL_BYTES, Math.max(SPOOL_MAX_FLOOR_BYTES, parsed));
}

function recordDrop(dir) {
  const counter = path.join(dir, 'dropped.count');
  let dropped = 0;
  try {
    dropped = parseInt(fs.readFileSync(counter, 'utf8'), 10) || 0;
  } catch (ignored) {
    // First drop: the counter does not exist yet.
  }
  fs.writeFileSync(counter, String(dropped + 1) + '\n');
}

// Write one capsule inside the byte cap; over-cap capsules are dropped and
// counted. Returns the file path or null.
function spool(payload) {
  const body = sdk.canonicalJson(payload);
  const bytes = Buffer.byteLength(body);
  const dir = spoolDir();
  fs.mkdirSync(dir, { recursive: true });
  let used = 0;
  for (const entry of fs.readdirSync(dir)) {
    if (!entry.endsWith('.json')) continue;
    try {
      used += fs.statSync(path.join(dir, entry)).size;
    } catch (ignored) {
      // A concurrently removed entry counts as zero.
    }
  }
  if (used + bytes > spoolMaxBytes()) {
    state.stats.droppedCapsules += 1;
    recordDrop(dir);
    return null;
  }
  const digest = crypto.createHash('sha256').update(body).digest('hex').slice(0, 12);
  const file = path.join(dir, 'capsule-' + digest + '.json');
  fs.writeFileSync(file, body);
  state.stats.spooledCapsules += 1;
  process.stderr.write(
    SPOOL_MARKER + JSON.stringify({ file, operation: payload.operation }) + '\n',
  );
  return file;
}

function finishAndSpool(trace, operation, error) {
  try {
    trace.finish({ error: boundedError(error) }, undefined, false, false);
    spool({
      format: sdk.CAPTURE_FORMAT,
      version: 2,
      operation,
      oracle: TEST_FAILURE_ORACLE,
      envelope: envelopeFor(trace),
      events: trace.events(),
    });
  } catch (ignored) {
    // Capture must never mask the test's own failure.
    state.stats.failedCaptures += 1;
  }
}

function captureTest(suiteName) {
  instrument.install();
  return function ciTest(testName, fn) {
    const operation = operationFor(suiteName, testName);
    return nodeTest.test(testName, async (t) => {
      const trace = sdk.BackendTrace.begin(ciContext(), operation, {
        input: { suite: boundedName(suiteName), test: boundedName(testName) },
      });
      try {
        await sdk.traceStorage.run(trace, () => fn(t));
      } catch (error) {
        finishAndSpool(trace, operation, error);
        throw error;
      }
      try {
        trace.finish(null, undefined, true, false);
      } catch (ignored) {
        // An over-long passing trace has nothing to spool anyway.
      }
    });
  };
}

// The capsule names exactly one test; everything else is skipped so the
// process exit code speaks for the named test alone.
function replayTarget() {
  const payload = JSON.parse(fs.readFileSync(replayPath(), 'utf8'));
  const operation = payload.operation;
  if (typeof operation !== 'string' || !operation.startsWith(TEST_TRIGGER_PREFIX)) {
    throw new TypeError('REPROIT_REPLAY capsule does not carry a test trigger identity');
  }
  return operation;
}

function reportResult(operation, status, error) {
  const detail = { operation, status };
  if (error !== null) detail.failure = boundedError(error);
  process.stderr.write(RESULT_MARKER + JSON.stringify(detail) + '\n');
}

function replayTest(suiteName) {
  instrument.install();
  const target = replayTarget();
  return function ciTest(testName, fn) {
    const operation = operationFor(suiteName, testName);
    if (operation !== target) {
      return nodeTest.test(testName, { skip: 'reproit replay targets ' + target }, () => {});
    }
    return nodeTest.test(testName, async (t) => {
      try {
        await fn(t);
      } catch (error) {
        reportResult(operation, 'failed', error);
        throw error;
      }
      reportResult(operation, 'passed', null);
    });
  };
}

// options: reserved; there are none yet and unknown keys are rejected so a
// typo cannot silently change capture behavior.
function suite(suiteName, options = {}) {
  const unknown = Object.keys(options);
  if (unknown.length > 0) {
    throw new TypeError('reproit ci.suite: unknown option ' + unknown[0]);
  }
  const active = mode();
  if (active === 'capture') return captureTest(suiteName);
  if (active === 'replay') return replayTest(suiteName);
  return (testName, fn) => nodeTest.test(testName, fn);
}

module.exports = {
  suite,
  stats: () => ({ ...state.stats }),
  TEST_TRIGGER_PREFIX,
  TEST_FAILURE_ORACLE,
  RESULT_MARKER,
  SPOOL_MARKER,
  DEFAULT_SPOOL_DIR,
  DEFAULT_SPOOL_MAX_BYTES,
};
