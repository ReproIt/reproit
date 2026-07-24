// Capture-mode parity tests against sdk/reproit-backend-rs/src/capture.rs.
// The batch round-trip mirrors the Rust test that validates through
// reproit_protocol::EventBatch::validate, using the JS protocol mirror in
// sdk/test/event_batch_v1.js.
'use strict';

const assert = require('node:assert');
const test = require('node:test');

const { BackendTrace, Capture, CAPTURE_FORMAT, SERVER_ERROR_ORACLE } = require('../index.js');
const { validateEventBatch } = require('../../test/event_batch_v1.js');

function finishedTrace(status, success) {
  const capture = Capture.create({ endpoint: 'http://c/v1/events', apiKey: 'sk', appId: 'app' });
  const context = { ...capture.context(), build: '1.2.3' };
  const trace = BackendTrace.begin(context, 'createOrder', {
    input: { body: { item: 'widget', qty: 2 } },
  });
  trace.effect('read', { resource: 'inventory', key: 'widget' });
  trace.finish({ error: 'boom' }, status, success, true);
  return trace;
}

function batchFor(status, success) {
  const capture = Capture.create({
    endpoint: 'http://c/v1/events',
    apiKey: 'sk',
    appId: 'app-demo',
    build: '1.2.3',
  });
  const trace = finishedTrace(status, success);
  return capture._buildBatch([
    { operation: 'createOrder', status, events: trace.events().slice() },
  ]);
}

test('server error batch is a valid tagged event batch', () => {
  const batch = batchFor(500, false);
  validateEventBatch(batch);
  assert.strictEqual(batch.frames.length, 4);
  const finding = batch.frames[3].event;
  assert.strictEqual(finding.kind, 'finding');
  assert.strictEqual(finding.identity.oracle, SERVER_ERROR_ORACLE);
  const capture = finding.context.reproitCapture;
  assert.strictEqual(capture.format, CAPTURE_FORMAT);
  assert.strictEqual(capture.operation, 'createOrder');
  assert.strictEqual(capture.events.length, 3);
  // Redaction happened before anything left the process boundary.
  assert.strictEqual(capture.events[0].input.body.item, 'widget');
  assert.strictEqual(batch.deployment.version, '1.2.3');
});

test('healthy operations ship backend frames without a finding', () => {
  const batch = batchFor(201, true);
  validateEventBatch(batch);
  assert.strictEqual(batch.frames.length, 3);
  assert.ok(batch.frames.every((frame) => frame.event.kind === 'backend'));
});

test('oversized captures drop trailing effects first', () => {
  const events = finishedTrace(500, false).events().slice();
  events.splice(2, 0, { kind: 'effect', effect: 'write', resource: 'x'.repeat(48 * 1024) });
  const batch = Capture.create({
    endpoint: 'http://c/v1/events',
    apiKey: 'sk',
    appId: 'app',
  })._buildBatch([{ operation: 'createOrder', status: 500, events }]);
  validateEventBatch(batch);
  const finding = batch.frames[batch.frames.length - 1].event;
  assert.strictEqual(finding.context.captureDroppedEffects, 1);
  const kept = finding.context.reproitCapture.events;
  assert.strictEqual(kept.length, 3);
  assert.strictEqual(kept[1].kind, 'effect');
  assert.strictEqual(kept[1].resource, 'inventory');
});

test('a capture that cannot fit start plus return is omitted', () => {
  const events = [
    { kind: 'start', operation: 'op', input: { blob: 'x'.repeat(48 * 1024) } },
    { kind: 'return', status: 500, success: false },
  ];
  const batch = Capture.create({
    endpoint: 'http://c/v1/events',
    apiKey: 'sk',
    appId: 'app',
  })._buildBatch([{ operation: 'op', status: 500, events }]);
  const finding = batch.frames[batch.frames.length - 1].event;
  assert.strictEqual(finding.context.captureOmitted, true);
  assert.strictEqual('reproitCapture' in finding.context, false);
});

test('unusable configs disable capture instead of failing', () => {
  assert.strictEqual(Capture.create({ endpoint: '', apiKey: 'sk', appId: 'app' }), null);
  assert.strictEqual(Capture.create({ endpoint: 'http://c', apiKey: '', appId: 'app' }), null);
  assert.strictEqual(
    Capture.create({ endpoint: 'http://c', apiKey: 'sk', appId: 'bad app' }),
    null,
  );
  assert.strictEqual(
    Capture.create({ endpoint: 'http://c', apiKey: 'sk', appId: 'app', build: 'bad build' }),
    null,
  );
});

test('record ignores unfinished traces and healthy traces when sampling is off', () => {
  const capture = Capture.create({ endpoint: 'http://c/v1/events', apiKey: 'sk', appId: 'app' });
  const open = BackendTrace.begin(capture.context(), 'op', { input: null });
  capture.record(open);
  const healthy = BackendTrace.begin(capture.context(), 'op', { input: null });
  healthy.finish(null, 200, true, true);
  capture.record(healthy);
  assert.strictEqual(capture.stats().capturedOperations, 0);
  const failed = BackendTrace.begin(capture.context(), 'op', { input: null });
  failed.finish(null, 200, false, true);
  capture.record(failed);
  assert.strictEqual(capture.stats().capturedOperations, 1);
});

test('queue overflow drops the oldest operation', () => {
  const capture = Capture.create({ endpoint: 'http://c/v1/events', apiKey: 'sk', appId: 'app' });
  for (let i = 0; i < 65; i++) {
    const trace = BackendTrace.begin(capture.context(), 'op-' + i, { input: null });
    trace.finish(null, 500, false, true);
    capture.record(trace);
  }
  const stats = capture.stats();
  assert.strictEqual(stats.capturedOperations, 65);
  assert.strictEqual(stats.droppedOperations, 1);
  assert.strictEqual(capture._queue[0].operation, 'op-1');
});
