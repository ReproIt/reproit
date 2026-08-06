// Functional end-to-end test: real Express and Fastify servers with a planted
// 500, real HTTP requests, and a local stub ingest server. Asserts the finding
// source-neutral batch arrives with the causal sequence, and that a
// scan-time request round-trips the x-reproit-events header.
//
// Run explicitly (needs devDependencies): npm install && npm run test:e2e
'use strict';

const assert = require('node:assert');
const http = require('node:http');
const test = require('node:test');

const { Capture } = require('../index.js');

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
      const url = 'http://127.0.0.1:' + server.address().port + '/v1/capture-batches';
      resolve({ received, server, url });
    });
  });
}

function assertServerErrorBatch(received) {
  assert.strictEqual(received.length, 1);
  const { authorization, batch } = received[0];
  assert.strictEqual(authorization, 'Bearer sk_live_test');
  assert.strictEqual(batch.version, 1);
  assert.strictEqual(batch.projectId, 'app-e2e');
  assert.strictEqual(batch.deployment.version, '9.9.9');
  assert.strictEqual(batch.emitter.id, 'backend-node');
  const events = batch.events.map((captured) => captured.event);
  assert.deepStrictEqual(
    events.map((event) => event.kind),
    [
      'operation-start',
      'trigger',
      'checkpoint',
      'state-access',
      'effect',
      'operation-end',
      'observation',
    ],
  );
  // The determinism envelope rides as a named checkpoint.
  assert.strictEqual(events[2].name, 'determinism-envelope');
  assert.strictEqual(typeof events[2].attributes.observedAtMs, 'number');
  assert.strictEqual(typeof events[2].attributes.replaySeed, 'string');
  assert.strictEqual(events[3].subject, 'orders');
  // The raw return event ships as the operation-return effect carrier.
  assert.strictEqual(events[4].subject, 'operation-return');
  assert.strictEqual(events[4].value.representation, 'replayable');
  assert.strictEqual(events[4].value.value.kind, 'return');
  assert.strictEqual(events[4].value.value.status, 500);
  assert.strictEqual(events[4].value.value.success, false);
  assert.strictEqual(
    events.at(-1).failure.signature,
    'backend-server-error:POST /boom',
  );
  // The secret-shaped input field was structurally redacted before upload.
  assert.strictEqual(events[1].value.representation, 'replayable');
  assert.strictEqual(events[1].value.value.body.apiKey.$reproit.redacted, true);
  assert.strictEqual(events[1].value.value.body.item, 'widget');
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
  app.use(reproitExpress({ capture, effectsComplete: true }));
  app.get('/ok', (req, res) => res.json({ ok: true }));
  app.post('/boom', (req, res) => {
    req.reproit?.effect('write', {
      resource: 'orders',
      key: '1',
      exchange: { request: { key: '1' }, response: { written: true } },
    });
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
    assertServerErrorBatch(ingest.received);
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
  await app.register(reproitFastify, { capture, effectsComplete: true });
  app.get('/ok', async () => ({ ok: true }));
  app.post('/boom', async (request, reply) => {
    request.reproit?.effect('write', {
      resource: 'orders',
      key: '1',
      exchange: { request: { key: '1' }, response: { written: true } },
    });
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
    assertServerErrorBatch(ingest.received);
    await assertScanHeader(baseUrl);
    assert.strictEqual(capture.stats().capturedOperations, 1);
  } finally {
    await app.close();
    ingest.server.close();
  }
});

test('express: outbound exchanges ship with responses in the capture batch', async () => {
  const express = require('express');
  const reproitExpress = require('../express.js');
  const instrument = require('../instrument.js');
  instrument.install();
  const ingest = await startStubIngest();
  // Planted upstream dependency: returns a payload the handler mishandles.
  const upstream = http.createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ prices: null }));
  });
  await new Promise((resolve) => upstream.listen(0, '127.0.0.1', resolve));
  const upstreamUrl = 'http://127.0.0.1:' + upstream.address().port;
  const fakePg = {
    Client: class Client {
      query() {
        return Promise.resolve({ command: 'SELECT', rowCount: 1, rows: [{ id: 7 }] });
      }
    },
  };
  instrument.wrapPg(fakePg);
  const pgClient = new fakePg.Client();
  const capture = Capture.create({
    endpoint: ingest.url,
    apiKey: 'sk_live_test',
    appId: 'app-e2e',
    build: '9.9.9',
    flushIntervalMs: 100,
  });
  const app = express();
  app.use(express.json());
  app.use(reproitExpress({ capture, effectsComplete: true }));
  app.get('/quote', async (req, res) => {
    try {
      await pgClient.query('SELECT id FROM issuers WHERE symbol = $1', ['ACME']);
      const upstreamRes = await fetch(upstreamUrl + '/prices');
      const body = await upstreamRes.json();
      res.json({ first: body.prices[0] });
    } catch (err) {
      res.status(500).json({ error: 'internal' });
    }
  });
  const server = await new Promise((resolve) => {
    const listening = app.listen(0, '127.0.0.1', () => resolve(listening));
  });
  const baseUrl = 'http://127.0.0.1:' + server.address().port;
  try {
    const failing = await fetch(baseUrl + '/quote?symbol=ACME');
    assert.strictEqual(failing.status, 500);
    assert.strictEqual(await capture.flush(5000), true);
    assert.strictEqual(ingest.received.length, 1);
    const { batch } = ingest.received[0];
    // Both dependency calls ship raw exchange events nested replayable.
    const raws = batch.events
      .map((captured) => captured.event)
      .filter((event) => event.value && event.value.representation === 'replayable')
      .map((event) => event.value.value)
      .filter((raw) => raw && raw.exchange);
    assert.strictEqual(raws.length, 2);
    const pg = raws.find((raw) => raw.exchange.protocol === 'pg');
    assert.deepStrictEqual(pg.exchange.response.rows, [{ id: 7 }]);
    const httpExchange = raws.find((raw) => raw.exchange.protocol === 'http');
    assert.deepStrictEqual(httpExchange.exchange.response.body, { prices: null });
    assert.strictEqual(httpExchange.exchange.response.status, 200);
    const network = batch.capabilities.find((entry) => entry.capability === 'network');
    assert.strictEqual(network.completeness, 'complete');
  } finally {
    server.close();
    upstream.close();
    ingest.server.close();
  }
});
