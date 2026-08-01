# ReproIt backend adapter for Go

This module is an internal validation surface, not a published compatibility API. It is inactive
unless a trusted request contains `x-reproit-trace`. It is a port of the Rust reference adapter
(`sdk/reproit-backend-rs`) with the same bounds, redaction, and wire format, in plain Go with
zero third-party dependencies in the core (`net/http`, `encoding/json`, `sync`).

Framework integrations pass their header lookup into `TraceContextFromHeaders`, start an
operation with `Begin`, record only effects actually observed by the adapter, then call `Finish`
and return `Header()` as `x-reproit-events`. Set `EffectsComplete` only when the adapter observed
every persistent effect in the operation. Tenant and resource identifiers must be non-secret
structural identifiers.

The adapter enforces bounded identifiers, 256 events, a 60 KB encoded header, typed effects, one
return, no effects after return, hashed idempotency identity, and recursive structural redaction.
GraphQL callers may attach parser-produced `Selection` mappings; never infer selections from
response content.

## net/http middleware and Fiber v2 adapter

The `net/http` middleware begins the trace from the decoded request (JSON body, decoded query
values, lowercased headers), finishes it when the response is complete, and attaches
`x-reproit-events` on scan-time requests. Handlers record observed effects through the recorder
carried on the request context:

```go
import reproit "github.com/reproit/reproit-backend"

config := reproit.NewCaptureConfig(
    "https://cloud.example.com/v1/events", // ingest endpoint
    "sk_live_...",                         // project API key (Authorization: Bearer)
    "app-id",                              // Cloud project app id
)
config.Build = "1.4.2"                     // optional deployment identity
capture := reproit.NewCapture(config)      // nil = disabled, host unaffected

mux := http.NewServeMux()
mux.HandleFunc("POST /orders", func(w http.ResponseWriter, r *http.Request) {
    if trace := reproit.FromRequest(r); trace != nil {
        _ = trace.Effect(reproit.EffectWrite, reproit.EffectOptions{
            Resource: "orders", Key: "1",
        })
    }
    // ...
})
handler := reproit.Middleware(reproit.MiddlewareOptions{Capture: capture})(mux)
```

Fiber v2 is a separate Go module (`github.com/reproit/reproit-backend/fiber`) so the core stays
dependency-free. It is the same adapter behind Fiber's buffered request/response model; handlers
fetch the recorder with `reproitfiber.From(c)`:

```go
import reproitfiber "github.com/reproit/reproit-backend/fiber"

app := fiber.New()
app.Use(reproitfiber.New(reproitfiber.Options{Capture: capture}))
```

Every adapter path fails closed: an instrumentation defect never breaks the request.

## Production capture mode (off by default)

Capture mode uploads finished traces to Cloud ingest without requiring `x-reproit-trace`. It is
config-gated: nothing leaves the process unless the host constructs a `Capture`.
`NewCapture(config)` returns nil (capture disabled, host unaffected) when the config is
unusable. `capture.Record(trace)` never blocks, never panics, and never surfaces errors.

Sampling: operations whose return reports `success == false` or HTTP 5xx are always captured;
healthy operations are captured only under `HealthySamplePerMille` (default 0, backend frames
only, no finding). A 5xx capture is posted as an event-batch-v1 batch: every trace event as a
`backend` frame plus one `finding` frame tagged with the first-class `backend-server-error`
oracle id, whose `context.reproitCapture` object carries the full redacted start/effects/return
sequence for deterministic local replay:

```sh
# fetch the finding from /v1/errors/:app, save context.reproitCapture as capture.json, then:
reproit internal debug replay-capture capture.json
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), 16 operations per batch,
48 KB capture payload (trailing effect events dropped first, `captureDroppedEffects` counts
them), bounded flush interval (floor 100 ms), per-request timeout, and at most `RetryLimit`
(cap 5) retries; 4xx responses are never retried. Redaction runs in `Begin`/`Effect`/`Finish`,
before anything is queued. Uploads run on one background goroutine, off the request path.
Canonical wire bytes (compact, recursively sorted keys) are pinned against the Node adapter by
a golden test. `sdk/test/oracle_contract_test.js` pins the `backend-server-error` tagging
contract.

## Capsule parity: exchange capture and hermetic replay

The boundary is explicit and opt-in (Go has no monkeypatching): route outbound HTTP through
`Transport` (or the client `WrapClient` returns) and database traffic through the
`SQLDriver` wrap of any `database/sql` driver, or the explicit `RunDB` closure. Every
dependency exchange (request AND response) is recorded on the ambient trace, bounded
(8 KiB body budget with full-byte sha256 identity beyond it, 32 name-sorted headers, 64
rows, 128 stream chunk boundaries) and redacted at source. Streaming responses (SSE /
chunked) record their observed chunk boundaries as the app consumes the body; an
abandoned body records nothing.

```go
sql.Register("reproit-pg", &reproit.SQLDriver{Base: pqDriver})
db, _ := sql.Open("reproit-pg", dsn)
client := reproit.WrapClient(nil)
```

With `REPROIT_REPLAY` naming a capture payload, the SAME boundaries serve the recorded
exchanges: no socket is opened and `SQLDriver.Open` returns a connect stub, so the app
boots with every dependency down. Matching is strict per-operation ordinals; recorded
`$reproit` placeholders wildcard; the first unmatched call emits a `REPROIT:DIVERGENCE`
stderr line (byte-identical to the Node reference, `bodyDelta` naming the first differing
chat message or byte offset) and answers 599 (HTTP) or an error (db). The envelope pins
TZ, the `ReplayNow` clock offset, and the seeded RNG (`math/rand`'s global source plus
`NewReplayRNG`).

Named capability gaps, recorded rather than papered over:

- `time.Now` cannot be patched process wide; code must read `reproit.ReplayNow()` to see
  the capture moment.
- `math/rand/v2`'s global source cannot be reseeded; only the v1 global source and
  `NewReplayRNG` pin. `crypto/rand` is unpinnable by design.
- The `database/sql` driver API exposes no server command tag, so the recorded `command`
  is derived from the statement's leading verb.
- Context-less driver calls (`driver.Stmt.Exec/Query` without context) carry no ambient
  trace and pass through unrecorded.

## Tests

```sh
cd sdk/reproit-backend-go
go test ./...        # unit + net/http e2e against a stub ingest, zero dependencies
cd fiber && go test ./...  # Fiber v2 adapter (separate module)
node ../test/backend_replay_parity_test.js  # byte parity against the Node reference
../../validation/backend/go-hermetic-e2e/run.sh  # money test under PORTABILITY
```
