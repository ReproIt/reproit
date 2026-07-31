// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// These vectors exist because eleven SDKs hand implement one contract, so a
// defect otherwise has to be found eleven times. Every group here was written
// against a real defect that shipped. Node is the reference implementation, so
// if these fail the vectors are wrong, not the SDK.
'use strict';

const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const VECTORS = JSON.parse(
  fs.readFileSync(path.join(__dirname, '../../capture-behavior-v1.json'), 'utf8'),
);

const { redact } = require('../index.js');
const instrument = require('../instrument.js');
const replay = require('../replay.js');

function bodyOf(spec) {
  if (spec.bodyRepeat) return spec.bodyRepeat[0].repeat(spec.bodyRepeat[1]);
  return spec.body;
}

test('constants match the shared vectors', () => {
  assert.strictEqual(
    instrument.MAX_EXCHANGE_BODY_BYTES,
    VECTORS.constants.maxExchangeBodyBytes,
  );
  assert.strictEqual(replay.DIVERGENCE_MARKER, VECTORS.constants.divergenceMarker);
});

test('bounds vectors', () => {
  for (const kase of VECTORS.bounds.cases) {
    const actual = instrument.boundedBody(bodyOf(kase.input), kase.input.contentType);
    const expected = { ...kase.expect };
    // A JS string also has a `repeat` method, so test the shape explicitly
    // rather than truthiness of the property.
    if (expected.body && Array.isArray(expected.body.repeat)) {
      expected.body = expected.body.repeat[0].repeat(expected.body.repeat[1]);
    }
    assert.deepStrictEqual(actual, expected, `bounds case ${kase.name}`);
  }
});

test('redaction type vectors', () => {
  for (const kase of VECTORS.redaction.typeCases) {
    assert.deepStrictEqual(redact(kase.input), kase.expect, JSON.stringify(kase.input));
  }
});

test('redaction key folding vectors', () => {
  for (const kase of VECTORS.redaction.foldingCases) {
    const out = redact({ [kase.field]: 'value' });
    const wasRedacted = Boolean(out[kase.field] && out[kase.field].$reproit);
    assert.strictEqual(
      wasRedacted,
      kase.secret,
      `${kase.field} should ${kase.secret ? '' : 'not '}be treated as secret`,
    );
  }
});

test('redaction nesting vectors', () => {
  for (const kase of VECTORS.redaction.nestingCases) {
    assert.deepStrictEqual(redact(kase.input), kase.expect, JSON.stringify(kase.input));
  }
});

test('matching vectors', () => {
  for (const kase of VECTORS.matching.cases) {
    const actual = replay.httpRequestMatcher(kase.recorded, kase.live);
    assert.strictEqual(actual, kase.expect.matches, `matching case ${kase.name}`);
  }
});

test('pg matching vectors', () => {
  for (const kase of VECTORS.matching.pgCases) {
    const actual = replay.pgRequestMatcher(kase.recorded, kase.live);
    assert.strictEqual(actual, kase.expect.matches, `pg case ${kase.name}`);
  }
});

test('the divergence marker starts the line and carries the required fields', () => {
  const session = new replay.ReplaySession({
    format: 'reproit-backend-capture',
    version: 2,
    operation: 'GET /x',
    oracle: 'backend-server-error',
    events: VECTORS.divergence.cases[0].capsuleExchanges.map((exchange, index) => ({
      kind: 'effect',
      sequence: index + 1,
      exchange,
    })),
  });
  const lines = [];
  const original = process.stderr.write.bind(process.stderr);
  process.stderr.write = (chunk) => {
    lines.push(String(chunk));
    return true;
  };
  try {
    session.match('http', { method: 'GET', url: 'http://svc/unknown' });
  } finally {
    process.stderr.write = original;
  }
  const marker = lines.find((line) => line.startsWith(VECTORS.divergence.markerPrefix));
  assert.ok(marker, 'the marker must START the line, not appear inside it');
  const report = JSON.parse(marker.slice(VECTORS.divergence.markerPrefix.length));
  for (const field of VECTORS.divergence.reportFields.required) {
    assert.ok(field in report, `report is missing required field ${field}`);
  }
  assert.strictEqual(report.consumed, VECTORS.divergence.cases[0].expect.consumed);
  assert.strictEqual(report.total, VECTORS.divergence.cases[0].expect.total);
});

test('the trigger token this SDK emits is in the protocol vocabulary', () => {
  const token = VECTORS.triggerTokens.bySdkKind.backend;
  assert.ok(VECTORS.triggerTokens.allowed.includes(token));
  assert.ok(!VECTORS.triggerTokens.rejected.includes(token));
  // The capture emitter must use exactly that token.
  const source = fs.readFileSync(path.join(__dirname, '../capture.js'), 'utf8');
  assert.ok(
    source.includes(`'${token}'`),
    `capture.js must emit ${token}; iOS and RN both shipped user-action instead`,
  );
  for (const bad of VECTORS.triggerTokens.rejected) {
    assert.ok(!source.includes(`'${bad}'`), `capture.js must not emit ${bad}`);
  }
});
