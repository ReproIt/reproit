// Refresh fixture: the same /quote operation as the hermetic fixture, but
// able to RE-RECORD in server mode, which is what `keep --refresh` drives.
//
// Modes:
//   MODE=capture              boot upstream + app, fire the request once, write
//                             the capture to CAPTURE_OUT, exit. (Guard birth.)
//   REPROIT_REPLAY=<capsule>  the SDK serves recorded exchanges; no upstream,
//                             no database. (What `check` does.)
//   REPROIT_CAPTURE_OUT=<f>   server mode WITH recording: talk to the real
//                             local upstream and write what happened to <f>.
//                             (What `keep --refresh` does.)
//
// DRIFT=1 adds an inventory call before pricing, so the code stops making the
// calls the original capture recorded. That is the drift a refresh re-records.
import { createRequire } from 'node:module';
import http from 'node:http';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const SDK = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../sdk/reproit-backend-node',
);
const require = createRequire(SDK + '/index.js');
const sdk = require(SDK + '/index.js');
const reproitExpress = require(SDK + '/express.js');
const instrument = require(SDK + '/instrument.js');
const express = require(SDK + '/node_modules/express');

instrument.install();

const UPSTREAM_PORT = 19973;
const upstreamBase = `http://127.0.0.1:${UPSTREAM_PORT}`;

// Stands in for a database. Never reached under replay: the SDK serves the
// recorded exchange first, and a live hit here is a hermeticity failure.
const fakePg = {
  Client: class Client {
    query() {
      if (process.env.REPROIT_REPLAY) {
        throw new Error('live database reached during hermetic replay');
      }
      return Promise.resolve({ command: 'SELECT', rowCount: 1, rows: [{ id: 7 }] });
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
      await pgClient.query('SELECT id FROM issuers WHERE symbol = $1', [
        String(req.query.symbol ?? ''),
      ]);
      // DRIFT: a call the original capture never recorded.
      if (process.env.DRIFT === '1') {
        const inventory = await fetch(`${upstreamBase}/inventory`);
        await inventory.json();
      }
      const upstream = await fetch(`${upstreamBase}/prices?tier=gold`);
      const body = await upstream.json();
      res.json({ first: body.prices[0] });
    } catch (err) {
      res.status(500).json({ error: 'internal' });
    }
  });
  return app;
}

function startUpstream() {
  const upstream = http.createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(
      req.url.startsWith('/inventory')
        ? JSON.stringify({ inStock: true })
        : JSON.stringify({ prices: null }),
    );
  });
  return new Promise((resolve) => upstream.listen(UPSTREAM_PORT, () => resolve(upstream)));
}

// The capture sink both birth and refresh use, so a refreshed capture is the
// same shape as the original.
function fileCapture(out) {
  return {
    context() {
      return {
        traceId: 'cap-refresh-1',
        actor: null,
        actionIndex: 0,
        build: 'refresh-fixture',
        configContract: null,
        captureEnvelope: true,
      };
    },
    record(trace) {
      fs.writeFileSync(
        out,
        sdk.canonicalJson({
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
        }),
      );
    },
  };
}

if (process.env.MODE === 'capture') {
  const upstream = await startUpstream();
  const app = buildApp(fileCapture(process.env.CAPTURE_OUT));
  const server = app.listen(19972, async () => {
    const res = await fetch('http://127.0.0.1:19972/quote?symbol=ACME');
    console.log('capture fixture status', res.status);
    server.close();
    upstream.close();
  });
} else {
  // Server mode. With REPROIT_CAPTURE_OUT the app also records what it does
  // against the REAL local upstream, which is the re-recording `--refresh`
  // reads back.
  const recording = process.env.REPROIT_CAPTURE_OUT;
  let upstream = null;
  if (recording) upstream = await startUpstream();
  const app = buildApp(recording ? fileCapture(recording) : null);
  const port = Number(process.env.PORT ?? 19972);
  app.listen(port, () => console.log('serving on', port));
  const shutdown = () => {
    if (upstream) upstream.close();
    process.exit(0);
  };
  process.on('SIGTERM', shutdown);
  process.on('SIGINT', shutdown);
}
