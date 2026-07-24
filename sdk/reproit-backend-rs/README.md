# ReproIt backend adapter for Rust

This crate is an internal validation surface, not a published compatibility API. It is inactive
unless a trusted request contains `x-reproit-trace`.

Framework integrations pass their header lookup into `TraceContext::from_header_fn`, start an
operation with `BackendTrace::begin`, record only effects actually observed by the adapter, then
call `finish` and return `header()` as `x-reproit-events`. Set `effects_complete` only when the
adapter observed every persistent effect in the operation. Tenant and resource identifiers must be
non-secret structural identifiers.

The adapter enforces bounded identifiers, 256 events, a 60 KB encoded header, typed effects, one
return, no effects after return, hashed idempotency identity, and recursive structural redaction.
GraphQL callers may attach parser-produced `Selection` mappings; never infer selections from
response content.

## Production capture mode (off by default)

Capture mode uploads finished traces to Cloud ingest without requiring `x-reproit-trace`. It is
config-gated: nothing leaves the process unless the host constructs a `Capture`.

```rust
let mut config = reproit_backend::CaptureConfig::new(
    "https://cloud.example.com/v1/events", // ingest endpoint
    "sk_live_...",                          // project API key (Authorization: Bearer)
    "app-id",                               // Cloud project app id
);
config.build = Some("1.4.2".into());        // optional deployment identity
let capture = reproit_backend::Capture::new(config); // None = disabled, host unaffected

// Per request: trace with the normal machinery, then hand the finished trace over.
let mut trace = BackendTrace::begin(capture.context(), "createOrder", ..)?;
// .. effect() calls, then finish(output, status, success, effects_complete)
capture.record(&trace); // never blocks, never panics, never surfaces errors
```

Sampling: operations whose return reports `success == false` or HTTP 5xx are always captured;
healthy operations are captured only under `healthy_sample_per_mille` (default 0, backend frames
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
them), bounded flush interval, per-request timeout, and at most `retry_limit` (cap 5) retries;
4xx responses are never retried. Redaction runs in `begin`/`effect`/`finish`, before anything is
queued. `sdk/test/oracle_contract_test.js` pins the `backend-server-error` tagging contract.
