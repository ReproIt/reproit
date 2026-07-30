// Money-test fixture: an Express app with the reproit SDK whose /quote
// operation 500s because an upstream pricing service returns {prices: null}.
//
// MODE=capture: boots upstream + app, fires the failing request, writes a
// version-2 reproit-backend-capture (exchanges + envelope) to CAPTURE_OUT.
// Default (server) mode: boots ONLY the app on $PORT; with REPROIT_REPLAY
// set the SDK serves the recorded exchanges, so no upstream and no database
// exist. FIXED=1 applies the fix.
import { createRequire } from 'node:module';
import http from 'node:http';
import fs from 'node:fs';

import { fileURLToPath } from 'node:url';
import path from 'node:path';
const SDK = path.join(path.dirname(fileURLToPath(import.meta.url)), '../../../sdk/reproit-backend-node');
const require = createRequire(SDK + '/index.js');
const sdk = require(SDK + '/index.js');
const reproitExpress = require(SDK + '/express.js');
const instrument = require(SDK + '/instrument.js');
const express = require(SDK + '/node_modules/express');

instrument.install();

// A pg-shaped driver that MUST never be reached for real: in capture mode a
// canned result stands in for a live database; in replay mode the SDK serves
// the recorded exchange before this throws.
const fakePg = {
  Client: class Client {
    query(config) {
      if (process.env.MODE !== 'capture') {
        throw new Error('live database reached during hermetic replay');
      }
      return Promise.resolve({ command: 'SELECT', rowCount: 1, rows: [{ id: 7, symbol: 'ACME' }] });
    }
  },
};
instrument.wrapPg(fakePg);
const pgClient = new fakePg.Client();

function buildApp(capture) {
  const app = express();
  app.use(express.json());
  app.use(reproitExpress({ capture }));
  app.get('/quote', async (req, res) => {
    try {
      await pgClient.query('SELECT id, symbol FROM issuers WHERE symbol = $1', [
        String(req.query.symbol ?? ''),
      ]);
      const upstream = await fetch('http://127.0.0.1:19971/prices?tier=gold');
      const body = await upstream.json();
      if (process.env.FIXED === '1' && !Array.isArray(body.prices)) {
        return res.json({ first: null, note: 'no prices available' });
      }
      res.json({ first: body.prices[0] });
    } catch (err) {
      res.status(500).json({ error: 'internal' });
    }
  });
  return app;
}

if (process.env.MODE === 'capture') {
  const upstream = http.createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ prices: null }));
  });
  await new Promise((r) => upstream.listen(19971, r));
  const fileCapture = {
    context() {
      return {
        traceId: 'cap-money-1',
        actor: null,
        actionIndex: 0,
        build: 'money-fixture',
        configContract: null,
        captureEnvelope: true,
      };
    },
    record(trace) {
      const payload = {
        format: 'reproit-backend-capture',
        version: 2,
        operation: trace.events()[0].operation,
        oracle: 'backend-server-error',
        envelope: {
          observedAtMs: Date.now(),
          tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
          node: process.version,
          os: process.platform,
          arch: process.arch,
          replaySeed: 'c0ffee00c0ffee00',
        },
        events: trace.events(),
      };
      fs.writeFileSync(process.env.CAPTURE_OUT, sdk.canonicalJson(payload));
    },
  };
  const app = buildApp(fileCapture);
  const server = app.listen(19970, async () => {
    const res = await fetch('http://127.0.0.1:19970/quote?symbol=ACME');
    console.log('capture fixture status', res.status);
    server.close();
    upstream.close();
  });
} else {
  const app = buildApp(null);
  const port = Number(process.env.PORT ?? 19970);
  app.listen(port, () => console.log('serving on', port));
}
