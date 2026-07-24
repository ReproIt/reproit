// Functional end-to-end test: real Express and Fastify servers with a planted
// 500, real HTTP requests, and a local stub ingest server. Asserts the finding
// batch arrives correctly tagged with the reproitCapture sequence, and that a
// scan-time request round-trips the x-reproit-events header.
//
// Run explicitly (needs devDependencies): npm install && npm run test:e2e
'use strict';

const assert = require('node:assert');
const http = require('node:http');
const test = require('node:test');

const { Capture, SERVER_ERROR_ORACLE, CAPTURE_FORMAT } = require('../index.js');
const { validateEventBatch } = require('../../test/event_batch_v1.js');

function startStubIngest() {
  const received = [];
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      received.push({ authorization: req.headers.authorization, batch: JSON.parse(body) });
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end('{"accepted":true}');
    });
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const url = 'http://127.0.0.1:' + server.address().port + '/v1/events';
      resolve({ received, server, url });
    });
  });
}

function assertServerErrorBatch(received, sdkName) {
  assert.strictEqual(received.length, 1);
  const { authorization, batch } = received[0];
  assert.strictEqual(authorization, 'Bearer sk_live_test');
  validateEventBatch(batch);
  assert.strictEqual(batch.appId, 'app-e2e');
  assert.strictEqual(batch.deployment.version, '9.9.9');
  const findings = batch.frames.filter((frame) => frame.event.kind === 'finding');
  assert.strictEqual(findings.length, 1);
  const finding = findings[0].event;
  assert.strictEqual(finding.identity.oracle, SERVER_ERROR_ORACLE);
  assert.strictEqual(finding.context.capture, sdkName);
  const capture = finding.context.reproitCapture;
  assert.strictEqual(capture.format, CAPTURE_FORMAT);
  assert.strictEqual(capture.oracle, SERVER_ERROR_ORACLE);
  const kinds = capture.events.map((event) => event.kind);
  assert.deepStrictEqual(kinds, ['start', 'effect', 'return']);
  assert.strictEqual(capture.events[1].resource, 'orders');
  assert.strictEqual(capture.events[2].status, 500);
  assert.strictEqual(capture.events[2].success, false);
  // The secret-shaped input field was structurally redacted before upload.
  assert.strictEqual(capture.events[0].input.body.apiKey.$reproit.redacted, true);
  assert.strictEqual(capture.events[0].input.body.item, 'widget');
}

async function assertScanHeader(baseUrl) {
  const response = await fetch(baseUrl + '/ok', {
    headers: { 'x-reproit-trace': 'trace-e2e', 'x-reproit-actor': 'alice' },
  });
  assert.strictEqual(response.status, 200);
  const header = response.headers.get('x-reproit-events');
  assert.ok(header, 'expected an x-reproit-events response header');
  const events = JSON.parse(Buffer.from(header, 'base64url').toString('utf8'));
  assert.strictEqual(events[0].traceId, 'trace-e2e');
  assert.strictEqual(events[0].actor, 'alice');
  assert.strictEqual(events.at(-1).kind, 'return');
  assert.strictEqual(events.at(-1).status, 200);
}

test('express: planted 500 ships a tagged finding batch to the stub ingest', async () => {
  const express = require('express');
  const reproitExpress = require('../express.js');
  const ingest = await startStubIngest();
  const capture = Capture.create({
    endpoint: ingest.url,
    apiKey: 'sk_live_test',
    appId: 'app-e2e',
    build: '9.9.9',
    flushIntervalMs: 100,
  });
  const app = express();
  app.use(express.json());
  app.use(reproitExpress({ capture }));
  app.get('/ok', (req, res) => res.json({ ok: true }));
  app.post('/boom', (req, res) => {
    req.reproit?.effect('write', { resource: 'orders', key: '1' });
    res.status(500).json({ error: 'boom' });
  });
  const server = await new Promise((resolve) => {
    const listening = app.listen(0, '127.0.0.1', () => resolve(listening));
  });
  const baseUrl = 'http://127.0.0.1:' + server.address().port;
  try {
    const boom = await fetch(baseUrl + '/boom', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ item: 'widget', apiKey: 'sk_live_leak' }),
    });
    assert.strictEqual(boom.status, 500);
    assert.strictEqual(await capture.flush(5000), true);
    assertServerErrorBatch(ingest.received, 'reproit-backend-node');
    await assertScanHeader(baseUrl);
    // The healthy scan-time request must not have been captured.
    assert.strictEqual(capture.stats().capturedOperations, 1);
  } finally {
    server.close();
    ingest.server.close();
  }
});

test('fastify: planted 500 ships a tagged finding batch to the stub ingest', async () => {
  const fastify = require('fastify');
  const reproitFastify = require('../fastify.js');
  const ingest = await startStubIngest();
  const capture = Capture.create({
    endpoint: ingest.url,
    apiKey: 'sk_live_test',
    appId: 'app-e2e',
    build: '9.9.9',
    flushIntervalMs: 100,
  });
  const app = fastify();
  await app.register(reproitFastify, { capture });
  app.get('/ok', async () => ({ ok: true }));
  app.post('/boom', async (request, reply) => {
    request.reproit?.effect('write', { resource: 'orders', key: '1' });
    reply.code(500);
    return { error: 'boom' };
  });
  await app.listen({ port: 0, host: '127.0.0.1' });
  const baseUrl = 'http://127.0.0.1:' + app.server.address().port;
  try {
    const boom = await fetch(baseUrl + '/boom', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ item: 'widget', apiKey: 'sk_live_leak' }),
    });
    assert.strictEqual(boom.status, 500);
    assert.strictEqual(await capture.flush(5000), true);
    assertServerErrorBatch(ingest.received, 'reproit-backend-node');
    await assertScanHeader(baseUrl);
    assert.strictEqual(capture.stats().capturedOperations, 1);
  } finally {
    await app.close();
    ingest.server.close();
  }
});
