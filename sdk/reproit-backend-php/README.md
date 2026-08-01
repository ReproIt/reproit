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
reproit internal debug replay-capture capture.json
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

## Capsule parity (outbound exchanges, determinism envelope, hermetic replay)

The PHP SDK carries the full capsule feature set of the Node reference, parity-pinned byte for
byte by `sdk/test/backend_replay_parity_test.js` (served exchange, 599 divergence body, and the
`REPROIT:DIVERGENCE ` marker line):

- **PSR-18 decoration** (`psr18.php`): wrap any PSR-18 client in
  `new \ReproitBackend\Psr18\RecordingClient($inner)`. Request line, headers, and body plus the
  response are recorded as bounded `http` exchanges on the ambient trace. The response body is
  TEED, not drained: chunks are recorded as the app consumes the PSR-7 stream and the exchange
  lands at EOF with the observed chunk boundaries (SSE / LLM streaming); an abandoned body
  records nothing. With `REPROIT_REPLAY` set the decorator serves the recorded exchange in
  process and the inner client is never called.
- **PDO wrap** (`pdo.php`): `new \ReproitBackend\RecordingPdo($dsn, ...)` records statements in
  the Node `pg` wire shape (`{text, values}` / `{command, rowCount, rows}`). In replay the
  constructor is a **connect stub**: the parent PDO is never initialized and no server is
  dialed, so the app boots with the database down; an unseen statement fails closed with the
  structured divergence marker.
- **Bounds**, identical to every SDK: 8 KiB inline body budget (identity kept as byte count +
  sha256 over every byte past it), 32 headers capped over name-sorted order, 64 database rows,
  128 stream chunk boundaries. Truncated bodies and truncated boundary lists fail replay closed
  with a named reason.
- **Determinism envelope**: capture stamps `observedAtMs`, `tz`, runtime, `replaySeed`, and the
  deployment identity (config `build`/`commit`, then `REPROIT_COMMIT`, then `GITHUB_SHA`).
  Replay pins the timezone, seeds `mt_srand` from `replaySeed`, and exposes the pinned clock as
  `Instrument::clock()` (a `PinnedClock` offset to the capture instant).

Named capability gaps, stated rather than papered over:

- **curl-direct traffic is not interceptable.** PHP has no process-wide HTTP chokepoint, so
  `curl_exec` called directly, `file_get_contents` with an `http://` URL, and ORMs with their
  own transports are invisible to capture and unavailable at replay. Route outbound calls
  through `RecordingClient` (or `Instrument::http`) or they are outside the capsule.
- **The wall clock cannot be pinned.** Redeclaring `time()`/`microtime()` is a fatal error and
  the namespaced fallback cannot reach application code (measured; see `pin_envelope`), and the
  extensions that could (uopz/runkit7) are not acceptable production dependencies. The SDK's
  `Clock` interface (`Instrument::clock()`) is the seam: apps that need anchored time read it.
- **`random_bytes`/`random_int` are unpinnable.** They are CSPRNG by design and accept no seed;
  only the `mt_rand` stream is seeded. Code drawing crypto randomness stays nondeterministic in
  replay.
- **The request-scoped process model.** PHP tears the world down per request (FPM, `php -S`),
  so a trace, its capsule state, and the replay session all live inside ONE request lifecycle:
  the capture spools before process end through a shutdown function (`register_shutdown_function`,
  the `Capture` design above), and per-operation replay ordinals are stable because they start
  from zero at every request boundary by construction. A background job that outlives its
  request is not covered by request-scoped capture; trace it explicitly as its own operation.

Acceptance: `validation/backend/php-hermetic-e2e/run.sh` captures a planted 5xx (PSR-18
upstream call plus PDO query) on `examples/php-backend-fixture/app.php` and re-executes it from
a copied checkout with every dependency stopped, asserting all four verdicts (reproduced /
fixed / reproduced / diverged-naming-the-call).

## Tests

```sh
cd sdk/reproit-backend-php
php test/trace_test.php && php test/capture_test.php && php test/psr15_test.php  # unit
php test/e2e_test.php  # vanilla `php -S` app + stub ingest server, real requests
```

Batch shape is validated through `test/event_batch_v1.php`, a PHP mirror of
`sdk/test/event_batch_v1.js`, and the canonical encoding is byte-compared against the Node SDK's
`canonicalJson` when `node` is available.
