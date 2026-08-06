'use strict';

const assert = require('node:assert');
const crypto = require('node:crypto');
const test = require('node:test');

const {
  Recorder,
  Transport,
  environmentBound,
  replayable,
  structural,
} = require('../index.js');

function addPortableFailure(capture, artifactIds = []) {
  capture.trigger('http-request', 'POST /orders', replayable({ sku: 'widget' }));
  capture.checkpoint('determinism-envelope', {
    observedAtMs: 1,
    replaySeed: '00ff00ff00ff00ff',
    tz: 'UTC',
  });
  capture.failure({
    observation: 'exception',
    authority: 'runtime-diagnosis',
    summary: 'order creation failed',
    signature: 'orders:create:failed',
    artifactIds,
  });
}

function recorder(maxEvents = 16) {
  return new Recorder({
    batchId: 'cb_test',
    projectId: 'project-test',
    sessionId: 'session-test',
    emitter: {
      id: 'node-test',
      kind: 'runtime-sdk',
      component: 'orders',
      runtime: 'node',
    },
    observedAt: '2026-07-27T12:00:00Z',
    capabilities: [
      { capability: 'http', completeness: 'complete' },
      {
        capability: 'database',
        completeness: 'partial',
        detail: 'writes are observed but seed data is not captured',
      },
    ],
    maxEvents,
  });
}

test('records a source-neutral request failure', () => {
  const capture = recorder();
  const operation = capture.operationStart('create-order', { monotonicNs: 1 });
  capture.trigger(
    'http-request',
    'POST /orders',
    replayable({ body: { sku: 'widget', quantity: 2 } }),
    { monotonicNs: 2, causalParentIds: [operation], traceId: 'trace-1' },
  );
  capture.state(
    'database',
    'write',
    'orders',
    structural({ table: 'orders', columns: ['sku', 'quantity'] }),
    { monotonicNs: 3, traceId: 'trace-1' },
  );
  capture.operationEnd('create-order', 'failed', { monotonicNs: 4 });
  capture.failure({
    observation: 'exception',
    authority: 'runtime-diagnosis',
    summary: 'order creation failed',
    signature: 'orders:create:unique-violation',
    observationPoint: 'orders.create',
    artifactIds: [],
  }, { monotonicNs: 5 });

  const batch = capture.finish();
  assert.strictEqual(batch.version, 1);
  assert.strictEqual(batch.events.length, 5);
  assert.strictEqual(batch.events[1].event.kind, 'trigger');
  assert.strictEqual(batch.events[4].event.kind, 'observation');
  assert.strictEqual(capture.finish(), null);
});

test('reports bounded event overflow without dangling parents', () => {
  const capture = recorder(3);
  const first = capture.operationStart('first', { monotonicNs: 1 });
  const second = capture.operationStart('second', {
    monotonicNs: 2,
    causalParentIds: [first],
  });
  capture.operationEnd('second', 'succeeded', {
    monotonicNs: 3,
    causalParentIds: [second],
  });
  capture.checkpoint('done', {}, { monotonicNs: 4 });
  const batch = capture.finish();
  assert.strictEqual(batch.events.length, 3);
  assert.strictEqual(batch.events[2].event.kind, 'defect');
  assert.ok(batch.events.every((event) => !event.causalParentIds.includes(first)));
});

test('normalizes unsafe external correlation ids without dropping evidence', () => {
  const first = new Recorder({
    batchId: 'cb_first',
    projectId: 'project-test',
    sessionId: 'session/with spaces',
    emitter: {
      id: 'node-test',
      kind: 'runtime-sdk',
      component: 'orders',
      runtime: 'node',
    },
  });
  first.operationStart('POST /orders', {
    actor: 'Ada Lovelace',
    traceId: 'trace/with spaces',
    spanId: 'trace/with spaces:POST /orders',
  });
  const firstBatch = first.finish();
  const firstEvent = firstBatch.events[0];

  const second = recorder();
  second.operationStart('POST /orders', {
    actor: 'Ada Lovelace',
    traceId: 'trace/with spaces',
    spanId: 'trace/with spaces:POST /orders',
  });
  const secondEvent = second.finish().events[0];

  assert.match(firstEvent.actor, /^actor:[a-f0-9]{32}$/);
  assert.match(firstEvent.traceId, /^traceid:[a-f0-9]{32}$/);
  assert.match(firstEvent.spanId, /^spanid:[a-f0-9]{32}$/);
  assert.match(firstBatch.sessionId, /^sessionid:[a-f0-9]{32}$/);
  assert.strictEqual(firstEvent.actor, secondEvent.actor);
  assert.strictEqual(firstEvent.traceId, secondEvent.traceId);
  assert.strictEqual(firstEvent.spanId, secondEvent.spanId);
});

test('value constructors preserve the portability boundary', () => {
  assert.strictEqual(structural({ type: 'string' }).representation, 'structural');
  assert.strictEqual(replayable('safe').representation, 'replayable');
  assert.strictEqual(environmentBound('customer-db').representation, 'environment-bound');
  assert.throws(() => replayable('secret', 'unredacted-restricted'));
});

test('transport rejects oversized batches and bad config without throwing', () => {
  assert.strictEqual(Transport.create({ endpoint: '', apiKey: 'key' }), null);
  const transport = Transport.create({
    endpoint: 'https://cloud.example/v1/capture-batches',
    apiKey: 'pk_test',
  });
  assert.ok(transport);
  assert.strictEqual(transport.submit({ data: 'x'.repeat(4 * 1024 * 1024) }), false);
});

test('transport requires digest verified bytes for exportable artifacts', () => {
  const bytes = Buffer.from('safe evidence');
  const digest = 'sha256:' + crypto.createHash('sha256').update(bytes).digest('hex');
  const capture = recorder();
  capture.addArtifact({
    id: digest,
    kind: 'structured-log',
    mediaType: 'application/json',
    bytes: bytes.length,
    policy: 'exportable',
    redaction: 'redacted-at-source',
    collection: 'flight-recorder',
  });
  addPortableFailure(capture, [digest]);
  const batch = capture.finish();
  const transport = Transport.create({
    endpoint: 'https://cloud.example/v1/capture-batches',
    apiKey: 'pk_test',
  });
  assert.strictEqual(transport.submit(batch), false);
  assert.strictEqual(transport.submit(batch, { [digest]: Buffer.from('wrong') }), false);
  assert.strictEqual(transport.submit(batch, { [digest]: bytes }), true);
});

test('transport rejects incomplete captures before any network request', async () => {
  const capture = recorder();
  capture.failure({
    observation: 'exception',
    authority: 'runtime-diagnosis',
    summary: 'failure without a trigger or envelope',
    signature: 'failure:incomplete',
    artifactIds: [],
  });
  const transport = Transport.create({
    endpoint: 'https://cloud.example/v1/capture-batches',
    apiKey: 'pk_test',
  });
  const originalFetch = global.fetch;
  let requests = 0;
  global.fetch = async () => {
    requests += 1;
    throw new Error('unexpected request');
  };
  try {
    assert.strictEqual(transport.submit(capture.finish()), false);
    assert.strictEqual(await transport.flush(100), true);
    assert.strictEqual(requests, 0);
  } finally {
    global.fetch = originalFetch;
  }
});

test('transport rejects local-only oracle artifacts before upload', () => {
  const bytes = Buffer.from('local evidence');
  const digest = 'sha256:' + crypto.createHash('sha256').update(bytes).digest('hex');
  const capture = recorder();
  capture.addArtifact({
    id: digest,
    kind: 'structured-log',
    mediaType: 'application/json',
    bytes: bytes.length,
    policy: 'local-only',
    redaction: 'redacted-at-source',
    collection: 'flight-recorder',
  });
  addPortableFailure(capture, [digest]);
  const transport = Transport.create({
    endpoint: 'https://cloud.example/v1/capture-batches',
    apiKey: 'pk_test',
  });
  assert.strictEqual(transport.submit(capture.finish()), false);
});
