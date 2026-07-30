// Outbound-exchange capture tests: the instrumented http/fetch/pg clients
// must attach request AND response to the ambient trace, bounded and
// redacted, and exchange-bearing batches must nest the raw events so the
// protocol projection can round-trip them.
'use strict';

const assert = require('node:assert');
const http = require('node:http');
const test = require('node:test');

const { BackendTrace, Capture, traceStorage } = require('../index.js');
const instrument = require('../instrument.js');

instrument.install();

function startUpstream(handler) {
  const server = http.createServer(handler);
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

function beginTrace() {
  return BackendTrace.begin(
    { traceId: 'cap-x-1', actor: null, actionIndex: 0, build: null, configContract: null },
    'GET /quote',
    { input: { query: { symbol: 'ACME' } } },
  );
}

test('http.get exchanges record request and response on the ambient trace', async () => {
  const upstream = await startUpstream((req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ prices: [1, 2], apiKey: 'sk-live-secret' }));
  });
  const port = upstream.address().port;
  const trace = beginTrace();
  await traceStorage.run(trace, () => {
    return new Promise((resolve, reject) => {
      http
        .get('http://127.0.0.1:' + port + '/prices', (res) => {
          res.on('data', () => {});
          res.on('end', resolve);
        })
        .on('error', reject);
    });
  });
  // The exchange lands asynchronously on response 'end'; one tick suffices.
  await new Promise((resolve) => setImmediate(resolve));
  upstream.close();
  const exchange = trace.events().find((event) => event.exchange)?.exchange;
  assert.ok(exchange, 'exchange event recorded');
  assert.strictEqual(exchange.protocol, 'http');
  assert.strictEqual(exchange.request.method, 'GET');
  assert.strictEqual(exchange.response.status, 200);
  assert.deepStrictEqual(exchange.response.body.prices, [1, 2]);
  // Structural redaction applies INSIDE captured exchange bodies.
  assert.strictEqual(exchange.response.body.apiKey.$reproit.redacted, true);
});

test('fetch exchanges are recorded with bodies', async () => {
  const upstream = await startUpstream((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      res.writeHead(502, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ error: 'upstream down', echoed: JSON.parse(body || '{}') }));
    });
  });
  const port = upstream.address().port;
  const trace = beginTrace();
  await traceStorage.run(trace, async () => {
    await fetch('http://127.0.0.1:' + port + '/convert', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ amount: 5 }),
    });
  });
  upstream.close();
  const exchange = trace.events().find((event) => event.exchange)?.exchange;
  assert.ok(exchange, 'exchange event recorded');
  assert.strictEqual(exchange.request.method, 'POST');
  assert.deepStrictEqual(exchange.request.body, { amount: 5 });
  assert.strictEqual(exchange.response.status, 502);
  assert.strictEqual(exchange.response.body.error, 'upstream down');
});

test('oversized bodies keep provable identity only', async () => {
  const big = 'x'.repeat(instrument.MAX_EXCHANGE_BODY_BYTES + 1);
  const upstream = await startUpstream((req, res) => {
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end(big);
  });
  const port = upstream.address().port;
  const trace = beginTrace();
  await traceStorage.run(trace, async () => {
    await fetch('http://127.0.0.1:' + port + '/blob');
  });
  upstream.close();
  const exchange = trace.events().find((event) => event.exchange)?.exchange;
  assert.strictEqual(exchange.response.truncated, true);
  assert.strictEqual(exchange.response.bodyBytes, big.length);
  assert.match(exchange.response.bodySha256, /^[0-9a-f]{64}$/);
  assert.strictEqual(exchange.response.body, undefined);
});

test('pg queries record rows and errors as exchanges', async () => {
  const fakePg = {
    Client: class Client {
      query(config, values, callback) {
        const text = typeof config === 'string' ? config : config.text;
        if (text.includes('boom')) return Promise.reject(new Error('relation missing'));
        const result = { command: 'SELECT', rowCount: 1, rows: [{ id: 7, name: 'ACME' }] };
        if (typeof callback === 'function') return callback(null, result);
        if (typeof values === 'function') return values(null, result);
        return Promise.resolve(result);
      }
    },
  };
  instrument.wrapPg(fakePg);
  const client = new fakePg.Client();
  const trace = beginTrace();
  await traceStorage.run(trace, async () => {
    await client.query('SELECT id, name FROM issuers WHERE symbol = $1', ['ACME']);
    await client.query('SELECT boom').catch(() => {});
  });
  await new Promise((resolve) => setImmediate(resolve));
  const exchanges = trace
    .events()
    .filter((event) => event.exchange)
    .map((event) => event.exchange);
  assert.strictEqual(exchanges.length, 2);
  assert.strictEqual(exchanges[0].protocol, 'pg');
  assert.deepStrictEqual(exchanges[0].request.values, ['ACME']);
  assert.deepStrictEqual(exchanges[0].response.rows, [{ id: 7, name: 'ACME' }]);
  assert.strictEqual(exchanges[1].response.error.message, 'relation missing');
});

test('exchange-bearing batches nest the raw event for projection', () => {
  const capture = Capture.create({
    endpoint: 'http://c/v1/capture-batches',
    apiKey: 'sk',
    appId: 'app-demo',
  });
  const trace = beginTrace();
  trace.effect('call', {
    resource: 'pricing',
    key: 'GET /prices',
    exchange: {
      protocol: 'http',
      request: { method: 'GET', url: 'http://pricing/prices' },
      response: { status: 200, body: { prices: null } },
    },
  });
  trace.effect('read', { resource: 'inventory', key: 'widget' });
  trace.finish({ error: 'boom' }, 500, false, true);
  const batch = capture._buildBatch([
    { operation: 'GET /quote', status: 500, events: trace.events().slice() },
  ]);
  const carriers = batch.events.filter(
    (event) => event.event.value && event.event.value.representation === 'replayable',
  );
  // Trigger input, the exchange effect, and the return carrier are
  // replayable; the plain read effect stays a structural summary.
  const exchangeCarrier = carriers.find(
    (event) => event.event.value.value && event.event.value.value.exchange,
  );
  assert.ok(exchangeCarrier, 'raw exchange event nested replayable');
  assert.strictEqual(
    exchangeCarrier.event.value.value.exchange.response.status,
    200,
  );
  const capability = batch.capabilities.find((entry) => entry.capability === 'network');
  assert.strictEqual(capability.completeness, 'complete');
  assert.match(capability.detail, /exchanges recorded with responses/);
});
