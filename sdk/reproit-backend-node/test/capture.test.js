// Capture-mode parity tests against sdk/reproit-backend-rs/src/capture.rs.
// Capture-mode tests for the universal source-neutral capture contract.
'use strict';

const assert = require('node:assert');
const test = require('node:test');

const { BackendTrace, Capture, CAPTURE_FORMAT, SERVER_ERROR_ORACLE } = require('../index.js');

function finishedTrace(status, success) {
  const capture = Capture.create({
    endpoint: 'http://c/v1/capture-batches',
    apiKey: 'sk',
    appId: 'app',
  });
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
    endpoint: 'http://c/v1/capture-batches',
    apiKey: 'sk',
    appId: 'app-demo',
    build: '1.2.3',
  });
  const trace = finishedTrace(status, success);
  return capture._buildBatch([
    { operation: 'createOrder', status, events: trace.events().slice() },
  ]);
}

test('server error batch is a source-neutral causal capture', () => {
  const batch = batchFor(500, false);
  assert.strictEqual(batch.version, 1);
  assert.strictEqual(batch.projectId, 'app-demo');
  assert.strictEqual(batch.emitter.kind, 'runtime-sdk');
  assert.strictEqual(batch.events[0].event.kind, 'operation-start');
  assert.strictEqual(batch.events[1].event.kind, 'trigger');
  assert.strictEqual(batch.events.at(-1).event.kind, 'observation');
  assert.strictEqual(batch.events.at(-1).event.failure.signature, 'backend:createOrder');
  assert.strictEqual(
    batch.events[1].event.value.value.body.item,
    'widget',
  );
  assert.strictEqual(batch.deployment.version, '1.2.3');
});

test('healthy operations ship causal facts without a failure observation', () => {
  const batch = batchFor(201, true);
  assert.strictEqual(
    batch.events.some((event) => event.event.kind === 'observation'),
    false,
  );
});

test('unrelated operations cannot share an occurrence batch', () => {
  const capture = Capture.create({
    endpoint: 'http://c/v1/capture-batches',
    apiKey: 'sk',
    appId: 'app',
  });
  const operation = {
    operation: 'createOrder',
    status: 500,
    events: finishedTrace(500, false).events(),
  };
  assert.throws(() => capture._buildBatch([operation, operation]));
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
  const capture = Capture.create({
    endpoint: 'http://c/v1/capture-batches',
    apiKey: 'sk',
    appId: 'app',
  });
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
  const capture = Capture.create({
    endpoint: 'http://c/v1/capture-batches',
    apiKey: 'sk',
    appId: 'app',
  });
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
