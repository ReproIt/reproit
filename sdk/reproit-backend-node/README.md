# ReproIt backend adapter for Node

This package is an internal validation surface, not a published compatibility API. It is inactive
unless a trusted request contains `x-reproit-trace`. It is a port of the Rust reference adapter
(`sdk/reproit-backend-rs`) with the same bounds, redaction, and wire format, in plain modern Node
with zero runtime dependencies.

Framework integrations pass their header lookup into `traceContextFromHeaders`, start an
operation with `BackendTrace.begin`, record only effects actually observed by the adapter, then
call `finish` and return `header()` as `x-reproit-events`. Set `effectsComplete` only when the
adapter observed every persistent effect in the operation. Tenant and resource identifiers must be
non-secret structural identifiers.

The adapter enforces bounded identifiers, 256 events, a 60 KB encoded header, typed effects, one
return, no effects after return, hashed idempotency identity, and recursive structural redaction.
GraphQL callers may attach parser-produced `selection` mappings; never infer selections from
response content.

## Express middleware and Fastify plugin

Both integrations begin the trace from the decoded request (JSON body, decoded query values,
lowercased headers), finish it when the response headers flush, and attach `x-reproit-events` on
scan-time requests. Handlers record observed effects through `req.reproit` / `request.reproit`:

```js
const { Capture } = require('reproit-backend-node');
const capture = Capture.create({
  endpoint: 'https://cloud.example.com/v1/events', // ingest endpoint
  apiKey: 'sk_live_...', // project API key (Authorization: Bearer)
  appId: 'app-id', // Cloud project app id
  build: '1.4.2', // optional deployment identity
});

// Express: mount after express.json(). Pass no capture for scan-time only.
app.use(require('reproit-backend-node/express')({ capture }));
app.post('/orders', (req, res) => {
  req.reproit?.effect('write', { resource: 'orders', key: '1' });
  // ...
});

// Fastify:
fastify.register(require('reproit-backend-node/fastify'), { capture });
```

Every adapter path fails closed: an instrumentation defect never breaks the request.

## Production capture mode (off by default)

Capture mode uploads finished traces to Cloud ingest without requiring `x-reproit-trace`. It is
config-gated: nothing leaves the process unless the host constructs a `Capture`.
`Capture.create(config)` returns `null` (capture disabled, host unaffected) when the config is
unusable. `capture.record(trace)` never blocks, never throws, and never surfaces errors.

Sampling: operations whose return reports `success == false` or HTTP 5xx are always captured;
healthy operations are captured only under `healthySamplePerMille` (default 0, backend frames
only, no finding). A 5xx capture is posted as an event-batch-v1 batch: every trace event as a
`backend` frame plus one `finding` frame tagged with the first-class `backend-server-error`
oracle id, whose `context.reproitCapture` object carries the full redacted start/effects/return
sequence for deterministic local replay:

```sh
# fetch the finding from /v1/errors/:app, save context.reproitCapture as capture.json, then:
reproit debug replay-capture capture.json
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), 16 operations per batch,
48 KB capture payload (trailing effect events dropped first, `captureDroppedEffects` counts
them), bounded flush interval (floor 100 ms), per-request timeout, and at most `retryLimit`
(cap 5) retries; 4xx responses are never retried. Redaction runs in `begin`/`effect`/`finish`,
before anything is queued. Uploads use global fetch on unref'd timers, off the request path.
`sdk/test/oracle_contract_test.js` pins the `backend-server-error` tagging contract.

## Tests

```sh
cd sdk/reproit-backend-node
npm test                    # unit + batch validation, zero dependencies
npm install && npm run test:e2e  # Express + Fastify servers against a stub ingest
```
