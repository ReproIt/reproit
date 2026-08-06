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
    'endpoint' => 'https://cloud.example.com/v1/capture-batches', // ingest endpoint
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
only, no finding). A 5xx capture is posted as one universal capture-batch-v1 containing exactly that
operation, carrying the full redacted start/effects/return sequence for deterministic local
replay:

```sh
# pull the occurrence the capture became, and re-execute it locally:
reproit occ_<id>
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

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), one operation per batch,
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

### Automatic http(s) stream capture (auto_prepend_file)

One outbound path needs no per-call change. PHP lets userland replace a stream wrapper, so
`autocapture.php` registers a capturing wrapper over `http://` and `https://`. Once installed,
every stream-based outbound request (`file_get_contents`, `fopen`, `SimpleXML`,
`DOMDocument::load` on an http(s) URL) is captured AUTOMATICALLY through the SAME path as
`Instrument::http`: the wrapper reads the method, headers, and body from the caller's stream
context and delegates to `Instrument::http`, so the recorded exchange shape and redaction are
identical. In replay mode (`REPROIT_REPLAY` set) the wrapper serves the recorded exchange with no
socket and fails closed on divergence with a thrown `DivergenceError` plus the `REPROIT:DIVERGENCE `
marker, the stream form of the boundary's 599.

Turn it on with the `auto_prepend_file` bootstrap, which requires the SDK and installs the wrapper
before the app runs:

```sh
php -d auto_prepend_file=/path/to/reproit-backend-php/bootstrap.php app.php
; or in php.ini / an FPM pool config:
auto_prepend_file = /path/to/reproit-backend-php/bootstrap.php
```

Installation is EXPLICIT. Requiring `reproit.php` loads the wrapper class but does not touch the
wrapper table, so an app opts in through the bootstrap or a direct `\ReproitBackend\install()`
call. Tests register and unregister deterministically with `install()` / `uninstall()`. Install is
idempotent and fails closed toward the host: a registration or capture defect leaves the builtin
wrapper serving and never breaks the host request. This covers STREAM traffic only. `curl_exec`
and PDO remain OPT-IN for the reason below.

Named capability gaps, stated rather than papered over:

- **curl-direct and PDO traffic are not interceptable.** PHP has no process-wide HTTP
  chokepoint, so `curl_exec` called directly and ORMs with their own transports are invisible to
  capture and unavailable at replay. curl and the PDO drivers are C-level functions, and PHP
  cannot redefine or intercept a C function at runtime without the uopz or runkit extension,
  which are not present (nor an acceptable production dependency). These stay OPT-IN: route curl
  calls through `RecordingClient` (or `Instrument::http`) and database statements through
  `RecordingPdo`, or they are outside the capsule. One outbound path IS automatic, see below.
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
upstream call plus PDO query) on `fixtures/php-backend-fixture/app.php` and re-executes it from
a copied checkout with every dependency stopped, asserting all four verdicts (reproduced /
fixed / reproduced / diverged-naming-the-call).

## Agent oracle API (LLM/agent capsule flavor)

`$trace->oracle($id, $detail)` marks an authored assertion on the trace: this operation
violated its own contract (response content/shape, guardrail, loop bound). Semantics are
identical to the Node reference and the Python port:

- Registry ids only, lowest confidence tier: `agent-response-content`,
  `agent-guardrail-violation`, `agent-loop-bound-exceeded` (`AGENT_ORACLES`). An unknown id
  throws `TraceError(InvalidOperation)`, so a typo cannot mint an oracle category.
- The marker rides as an `emit` effect on the resource `reproit-oracle`, so the scan-time wire
  vocabulary is unchanged and old readers keep working.
- A marked operation is ALWAYS captured, like a 5xx, even when it returns 200/success.
- The capture's failure observation carries the marked id in its signature and reports
  `contract-violation` (an authored assertion) instead of the 5xx default `exception`; the
  replayable capture payload's `oracle` field carries the marked id too.

```php
$trace->oracle(\ReproitBackend\AGENT_GUARDRAIL_ORACLE, ['tool' => 'delete_order']);
```

Prompt-drift divergence naming (`bodyDelta` with the first differing message index) already
ships in `replay.php`; the oracle API completes the agent flavor on this SDK.

## CI capture mode (the flaky-CI wedge)

`Ci::suite($name)` (in `ci.php`, required separately like Node's lazy `ci.js`) returns a
`$test($name, $fn)` callable for the plain test scripts this SDK itself uses, run directly as
`php test/x_test.php`. The trigger identity is the TEST, riding the existing `operation` field
as `test:<suite>#<test>`; a failed test carries the existing `backend-authored-invariant`
registry oracle. No new protocol fields, no new oracle ids.

- `REPROIT_CI_CAPTURE=1`: every test runs inside its own capture-envelope trace, installed as
  the ambient `Instrument` trace, so the explicit outbound boundary records dependency
  exchanges exactly as production capture does. A FAILING test spools a version-2
  `reproit-backend-capture` capsule to a bounded on-disk spool (`REPROIT_CI_SPOOL`, default
  `.reproit/ci-spool`; total-bytes cap `REPROIT_CI_SPOOL_MAX`, default 16 MiB, floor 4 KiB,
  ceil 64 MiB; over-cap capsules are dropped and counted in the on-disk `dropped.count`,
  never silently) and prints the `REPROIT:CI-CAPSULE ` stderr marker.
- `REPROIT_REPLAY=<capsule>`: the SAME wrapper re-runs ONLY the capsule's named test with the
  recorded exchanges served in process and the envelope pinned, and reports the observed
  result as the `REPROIT:CI-TEST ` structured stderr marker `reproit check` parses. The
  recorded exec re-runs the single named test file directly: `reproit check <capsule> --exec
  "php tests/checkout_test.php"`.
- Neither env: plain execution; the wrapper only keeps the script's exit code honest.

Process model, same seam as capture mode: the spool write happens synchronously when the test
throws, and a `register_shutdown_function` safety net spools the in-flight test when a fatal
error kills the request-scoped process before the catch can run.

Named follow-up gap: the wrapper targets the plain-script runner this repo's PHP tests use;
a PHPUnit listener is a follow-up, not shipped, so PHPUnit suites are not yet wired (adding
PHPUnit as a dependency was deliberately avoided).

Acceptance: `validation/backend/php-flaky-ci-e2e/run.sh` on
`fixtures/php-flaky-ci-fixture/` (a planted order-dependent failure invisible in a plain
run), cloned leg for leg from `validation/backend/flaky-ci-e2e`: plain run passes, the
simulated CI run spools, a plain rerun from the copy passes (flaky evidence, never Fixed),
then reproduced (1) / fixed (0) / reproduced again (1) / deleted exchange diverges (3)
naming the call, all under the PORTABILITY bar.

## Level matrix against the Node reference

Founder rule: all backend SDKs sit at the same level as the Node reference in all ways;
genuinely-impossible surfaces are NAMED gaps, never silent. The full Node surface, row by row:

| Node surface | PHP status |
| --- | --- |
| Scan-time trace adapter (`x-reproit-trace` -> `x-reproit-events`) | Level (`trace.php`) |
| Canonical JSON wire + byte parity pins | Level (golden checks vs Node) |
| Framework adapters (express/fastify) | Level shape: PSR-15 + vanilla wrapper |
| Production capture mode + ingest upload | Level (`capture.php`; PHP flush model documented) |
| Outbound HTTP capture | Level; http(s) STREAMS automatic (auto_prepend wrapper), PSR-18 + `Instrument::http` opt-in; curl-direct + PDO NAMED gaps |
| DB driver exchange capture (`pg`) | Level shape: PDO wrap (`pdo.php`) |
| Streaming (SSE/chunked) exchange boundaries | Level (teed PSR-7 stream, chunk boundaries) |
| Envelope: TZ pin, seeded RNG | Level for `mt_rand`; `random_bytes`/`random_int` NAMED gap |
| Envelope: clock pin | NAMED gap: unpinnable sans extension; `Instrument::clock()` is the seam |
| Hermetic replay + `REPROIT:DIVERGENCE ` marker | Level (`replay.php`) |
| LLM flavor: prompt-drift `bodyDelta` | Level (first differing message index) |
| Agent oracle API (marked-op capture) | Level (this document, section above) |
| CI capture (test trigger, spool, `REPROIT:CI-TEST`) | Level for plain scripts; PHPUnit NAMED gap |
| Runner integration (node:test wrapper) | Level shape: plain-script `Ci::suite`, the SDK's idiom |
| Fixture + validation gate (hermetic + flaky-CI) | Level (`php-hermetic-e2e`, `php-flaky-ci-e2e`) |

## Tests

```sh
cd sdk/reproit-backend-php
php test/trace_test.php && php test/capture_test.php && php test/psr15_test.php  # unit
php test/e2e_test.php  # vanilla `php -S` app + stub ingest server, real requests
```

Batch shape is validated through `test/event_batch_v1.php`, a PHP mirror of
`sdk/test/event_batch_v1.js`, and the canonical encoding is byte-compared against the Node SDK's
`canonicalJson` when `node` is available.
