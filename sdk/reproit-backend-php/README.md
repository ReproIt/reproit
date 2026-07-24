# ReproIt backend adapter for PHP

This package is an internal validation surface, not a published compatibility API. It is inactive
unless a trusted request contains `x-reproit-trace`. It is a port of the Rust reference adapter
(`sdk/reproit-backend-rs`) with the same bounds, redaction, and wire format, in plain modern PHP
with zero Composer runtime dependencies (curl when the extension is loaded, stream contexts
otherwise).

Framework integrations pass their header lookup into `trace_context_from_headers`, start an
operation with `BackendTrace::begin`, record only effects actually observed by the adapter, then
call `finish` and return `header()` as `x-reproit-events`. Set `effectsComplete` only when the
adapter observed every persistent effect in the operation. Tenant and resource identifiers must be
non-secret structural identifiers.

The adapter enforces bounded identifiers, 256 events, a 60 KB encoded header, typed effects, one
return, no effects after return, hashed idempotency identity, and recursive structural redaction.
GraphQL callers may attach parser-produced `selection` mappings; never infer selections from
response content.

## PSR-15 middleware and vanilla PHP wrapper

Both integrations begin the trace from the decoded request (JSON body up to 64 KB, decoded query
values, lowercased headers), finish it from the response, and attach `x-reproit-events` on
scan-time requests. The PSR interfaces are vendored as minimal guarded declarations, so no
`psr/*` package is required; real installs win when present.

```php
use ReproitBackend\Capture;
use ReproitBackend\ReproitMiddleware;

$capture = Capture::create([
    'endpoint' => 'https://cloud.example.com/v1/events', // ingest endpoint
    'apiKey' => 'sk_live_...', // project API key (Authorization: Bearer)
    'appId' => 'app-id', // Cloud project app id
    'build' => '1.4.2', // optional deployment identity
]);

// PSR-15 (Slim, Mezzio, any PSR-15 pipeline). Pass no capture for scan-time
// only. Handlers record observed effects via the `reproit` request attribute:
$app->add(new ReproitMiddleware($capture));
// in a handler:
$request->getAttribute('reproit')?->effect('write', ['resource' => 'orders', 'key' => '1']);

// Laravel: no direct dependency; wrap the middleware with a PSR-7 bridge
// (symfony/psr-http-message-bridge + a PSR-17 factory) and register the
// bridged middleware in the HTTP kernel, or use the vanilla wrapper below in
// a route closure.

// Vanilla PHP (front controllers, `php -S` routers): the handler returns
// [$status, $output] and the wrapper emits the JSON response.
\ReproitBackend\handle_request($capture, function (?\ReproitBackend\BackendTrace $trace) {
    $trace?->effect('write', ['resource' => 'orders', 'key' => '1']);
    return [201, ['id' => 1]];
});
```

Every adapter path fails closed: an instrumentation defect never breaks the request.

## Production capture mode (off by default)

Capture mode uploads finished traces to Cloud ingest without requiring `x-reproit-trace`. It is
config-gated: nothing leaves the process unless the host constructs a `Capture`.
`Capture::create($config)` returns `null` (capture disabled, host unaffected) when the config is
unusable. `$capture->record($trace)` never blocks, never throws, and never surfaces errors.

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

### The PHP flush model

PHP's request-per-process model has no long-lived background worker, so this port replaces the
reference SDKs' worker thread / unref'd timers with the PHP-model equivalent: `record` only
queues, and the queue drains in one bounded synchronous pass at request end, inside a shutdown
function registered by the `Capture` constructor. Where the SAPI supports it
(`fastcgi_finish_request` under FPM), the response is released to the client before anything is
sent; otherwise the connection is held for at most `shutdownTimeoutMs` (default 2000, cap 10000),
and whatever cannot ship inside that budget is dropped and counted in `droppedOperations`. The
response is never delayed beyond that documented timeout. `flushIntervalMs` keeps its reference
validation (floor 100 ms) but degenerates in this model: there is exactly one flush per process,
at request end. Long-running CLI workers can call `$capture->flush($timeoutMs)` on their own
schedule instead.

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), 16 operations per batch,
48 KB capture payload (trailing effect events dropped first, `captureDroppedEffects` counts
them), per-request timeout, at most `retryLimit` (cap 5) retries with 4xx never retried, and the
hard shutdown budget above. Redaction runs in `begin`/`effect`/`finish`, before anything is
queued. `sdk/test/oracle_contract_test.js` pins the `backend-server-error` tagging contract.

## Tests

```sh
cd sdk/reproit-backend-php
php test/trace_test.php && php test/capture_test.php && php test/psr15_test.php  # unit
php test/e2e_test.php  # vanilla `php -S` app + stub ingest server, real requests
```

Batch shape is validated through `test/event_batch_v1.php`, a PHP mirror of
`sdk/test/event_batch_v1.js`, and the canonical encoding is byte-compared against the Node SDK's
`canonicalJson` when `node` is available.
