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
reproit debug replay-capture capture.json
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), 16 operations per batch,
48 KB capture payload (trailing effect events dropped first, `captureDroppedEffects` counts
them), bounded flush interval (floor 100 ms), per-request timeout, and at most `RetryLimit`
(cap 5) retries; 4xx responses are never retried. Redaction runs in `Begin`/`Effect`/`Finish`,
before anything is queued. Uploads run on one background goroutine, off the request path.
Canonical wire bytes (compact, recursively sorted keys) are pinned against the Node adapter by
a golden test. `sdk/test/oracle_contract_test.js` pins the `backend-server-error` tagging
contract.

## Tests

```sh
cd sdk/reproit-backend-go
go test ./...        # unit + net/http e2e against a stub ingest, zero dependencies
cd fiber && go test ./...  # Fiber v2 adapter (separate module)
```
