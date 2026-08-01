// Hermetic replay tests: with REPROIT_REPLAY set, the wrapped clients serve
// recorded exchanges in process (no sockets, no database), divergence fails
// closed with the structured marker, and the envelope pins clock/RNG/TZ.
'use strict';

const assert = require('node:assert');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');

const CAPTURE = {
  format: 'reproit-backend-capture',
  version: 2,
  operation: 'GET /quote',
  oracle: 'backend-server-error',
  envelope: {
    observedAtMs: 1753747200000,
    tz: 'Europe/Berlin',
    node: 'v26.5.0',
    replaySeed: '00ff00ff00ff00ff',
  },
  events: [
    {
      traceId: 'cap-r-1',
      spanId: 'cap-r-1:GET /quote',
      actionIndex: 0,
      operation: 'GET /quote',
      sequence: 1,
      kind: 'start',
      input: { query: { symbol: 'ACME' } },
      at: 1753747200000,
      monoNs: 0,
    },
    {
      traceId: 'cap-r-1',
      spanId: 'cap-r-1:GET /quote',
      actionIndex: 0,
      operation: 'GET /quote',
      sequence: 2,
      kind: 'effect',
      effect: 'read',
      resource: 'pg',
      key: 'SELECT id FROM issuers WHERE symbol = $1',
      exchange: {
        protocol: 'pg',
        request: { text: 'SELECT id FROM issuers WHERE symbol = $1', values: ['ACME'] },
        response: { command: 'SELECT', rowCount: 1, rows: [{ id: 7 }] },
      },
      at: 1753747200004,
      monoNs: 4000000,
    },
    {
      traceId: 'cap-r-1',
      spanId: 'cap-r-1:GET /quote',
      actionIndex: 0,
      operation: 'GET /quote',
      sequence: 3,
      kind: 'effect',
      effect: 'call',
      resource: 'pricing',
      key: 'GET /prices',
      exchange: {
        protocol: 'http',
        request: { method: 'GET', url: 'http://pricing.internal/prices?tier=gold' },
        response: {
          status: 200,
          headers: { 'content-type': 'application/json' },
          body: { prices: null },
        },
      },
      at: 1753747200009,
      monoNs: 9000000,
    },
    // LLM-shaped traffic: a streamed SSE completion, a tool-call loop whose
    // recorded order interleaves another operation, and a chat exchange the
    // prompt-drift test tampers against. Minimal event fields: replay only
    // reads kind/exchange.
    {
      operation: 'GET /quote',
      sequence: 4,
      kind: 'effect',
      effect: 'call',
      exchange: {
        protocol: 'http',
        request: { method: 'GET', url: 'http://llm.internal/stream' },
        response: {
          status: 200,
          headers: { 'content-type': 'text/event-stream' },
          body: 'data: a\n\ndata: b\n\ndata: c\n\n',
          stream: { chunks: [9, 9, 9] },
        },
      },
    },
    {
      operation: 'GET /quote',
      sequence: 5,
      kind: 'effect',
      effect: 'call',
      exchange: {
        protocol: 'http',
        request: {
          method: 'POST',
          url: 'http://llm.internal/v1/messages',
          body: { model: 'm', messages: [{ role: 'user', content: 'q' }] },
        },
        response: {
          status: 200,
          headers: { 'content-type': 'application/json' },
          body: { reply: 'r0' },
        },
      },
    },
    {
      operation: 'GET /quote',
      sequence: 6,
      kind: 'effect',
      effect: 'call',
      exchange: {
        protocol: 'http',
        request: { method: 'POST', url: 'http://tools.internal/run', body: { tool: 'x' } },
        response: {
          status: 200,
          headers: { 'content-type': 'application/json' },
          body: { ok: true },
        },
      },
    },
    {
      operation: 'GET /quote',
      sequence: 7,
      kind: 'effect',
      effect: 'call',
      exchange: {
        protocol: 'http',
        request: {
          method: 'POST',
          url: 'http://llm.internal/v1/messages',
          body: {
            model: 'm',
            messages: [
              { role: 'user', content: 'q' },
              { role: 'assistant', content: 'r0' },
              { role: 'user', content: 'tool: ok' },
            ],
          },
        },
        response: {
          status: 200,
          headers: { 'content-type': 'application/json' },
          body: { reply: 'r1' },
        },
      },
    },
    {
      operation: 'GET /quote',
      sequence: 8,
      kind: 'effect',
      effect: 'call',
      exchange: {
        protocol: 'http',
        request: {
          method: 'POST',
          url: 'http://llm.internal/v1/chat',
          body: {
            messages: [
              { role: 'user', content: 'hello' },
              { role: 'assistant', content: 'hi' },
              { role: 'user', content: 'weather?' },
            ],
          },
        },
        response: {
          status: 200,
          headers: { 'content-type': 'application/json' },
          body: { reply: 'sunny' },
        },
      },
    },
    {
      traceId: 'cap-r-1',
      spanId: 'cap-r-1:GET /quote',
      actionIndex: 0,
      operation: 'GET /quote',
      sequence: 9,
      kind: 'return',
      output: { error: 'internal' },
      status: 500,
      success: false,
      effectsComplete: true,
      at: 1753747200012,
      monoNs: 12000000,
    },
  ],
};

const capturePath = path.join(os.tmpdir(), 'reproit-replay-test-' + process.pid + '.json');
fs.writeFileSync(capturePath, JSON.stringify(CAPTURE));
process.env.REPROIT_REPLAY = capturePath;
const instrument = require('../instrument.js');
instrument.install();

function stderrCapture() {
  const lines = [];
  const original = process.stderr.write.bind(process.stderr);
  process.stderr.write = (chunk, ...rest) => {
    lines.push(String(chunk));
    return original(chunk, ...rest);
  };
  return {
    lines,
    restore() {
      process.stderr.write = original;
    },
  };
}

test('envelope pins TZ, clock, and RNG', () => {
  assert.strictEqual(process.env.TZ, 'Europe/Berlin');
  assert.ok(Math.abs(Date.now() - CAPTURE.envelope.observedAtMs) < 5000);
  const draws = [Math.random(), Math.random()];
  draws.forEach((draw) => assert.ok(draw >= 0 && draw < 1));
});

test('pg queries serve recorded rows without a database', async () => {
  const fakePg = {
    Client: class Client {
      query() {
        throw new Error('replay must never reach the real driver');
      }
    },
  };
  instrument.wrapPg(fakePg);
  const result = await new fakePg.Client().query(
    'SELECT id FROM issuers WHERE symbol = $1',
    ['ACME'],
  );
  assert.deepStrictEqual(result.rows, [{ id: 7 }]);
});

test('http.get serves the recorded response in process', async () => {
  const body = await new Promise((resolve, reject) => {
    http
      .get('http://pricing.internal/prices?tier=gold', (res) => {
        assert.strictEqual(res.statusCode, 200);
        let data = '';
        res.on('data', (chunk) => (data += chunk));
        res.on('end', () => resolve(data));
      })
      .on('error', reject);
  });
  assert.deepStrictEqual(JSON.parse(body), { prices: null });
});

test('recorded SSE streams re-serve chunk for chunk', async () => {
  const chunks = [];
  await new Promise((resolve, reject) => {
    http
      .get('http://llm.internal/stream', (res) => {
        assert.strictEqual(res.statusCode, 200);
        res.on('data', (chunk) => chunks.push(String(chunk)));
        res.on('end', resolve);
      })
      .on('error', reject);
  });
  assert.deepStrictEqual(chunks, ['data: a\n\n', 'data: b\n\n', 'data: c\n\n']);
});

test('tool-call loops match per-operation ordinals across interleaving', async () => {
  // Recorded order is messages[0], tool, messages[1]; the live code asks for
  // both messages calls FIRST. Per-operation ordinals serve each operation
  // in its own recorded order without a cross-operation divergence.
  const first = await fetch('http://llm.internal/v1/messages', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'm', messages: [{ role: 'user', content: 'q' }] }),
  });
  assert.deepStrictEqual(await first.json(), { reply: 'r0' });
  const second = await fetch('http://llm.internal/v1/messages', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      model: 'm',
      messages: [
        { role: 'user', content: 'q' },
        { role: 'assistant', content: 'r0' },
        { role: 'user', content: 'tool: ok' },
      ],
    }),
  });
  assert.deepStrictEqual(await second.json(), { reply: 'r1' });
  const tool = await fetch('http://tools.internal/run', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ tool: 'x' }),
  });
  assert.deepStrictEqual(await tool.json(), { ok: true });
});

test('prompt drift names the first differing message index', async () => {
  const captured = stderrCapture();
  let response;
  try {
    response = await fetch('http://llm.internal/v1/chat', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        messages: [
          { role: 'user', content: 'hello' },
          { role: 'assistant', content: 'hi' },
          { role: 'user', content: 'DIFFERENT QUESTION' },
        ],
      }),
    });
  } finally {
    captured.restore();
  }
  assert.strictEqual(response.status, 599);
  const marker = captured.lines.find((line) => line.startsWith('REPROIT:DIVERGENCE '));
  assert.ok(marker, 'structured divergence marker emitted');
  const report = JSON.parse(marker.slice('REPROIT:DIVERGENCE '.length));
  assert.deepStrictEqual(report.bodyDelta, {
    kind: 'message',
    firstDifferingMessage: 2,
    recordedMessages: 3,
    liveMessages: 3,
  });
});

test('unknown body shapes fall back to the first differing byte offset', () => {
  const replay = require('../replay.js');
  const delta = replay.bodyDelta({ prompt: 'summarize A' }, { prompt: 'summarize B' });
  assert.strictEqual(delta.kind, 'byte');
  assert.strictEqual(delta.offset, JSON.stringify({ prompt: 'summarize ' }).length - 2);
  assert.strictEqual(replay.bodyDelta({ a: 1 }, { a: 1 }), null);
});

test('an unmatched call is a divergence: 599 and the structured marker', async () => {
  const captured = stderrCapture();
  try {
    const response = await fetch('http://pricing.internal/unknown-endpoint');
    assert.strictEqual(response.status, 599);
    assert.deepStrictEqual(await response.json(), { reproit: 'diverged' });
  } finally {
    captured.restore();
  }
  const marker = captured.lines.find((line) => line.startsWith('REPROIT:DIVERGENCE '));
  assert.ok(marker, 'structured divergence marker emitted');
  const report = JSON.parse(marker.slice('REPROIT:DIVERGENCE '.length));
  assert.strictEqual(report.protocol, 'http');
  assert.strictEqual(report.got.method, 'GET');
});
