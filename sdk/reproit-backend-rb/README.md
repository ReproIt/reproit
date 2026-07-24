# ReproIt backend adapter for Ruby

This package is an internal validation surface, not a published compatibility API. It is inactive
unless a trusted request contains `x-reproit-trace`. It is a port of the Rust reference adapter
(`sdk/reproit-backend-rs`) with the same bounds, redaction, and wire format.

Framework integrations pass their header lookup into
`ReproitBackendRb.trace_context_from_headers`, start an operation with `BackendTrace.begin`,
record only effects actually observed by the adapter, then call `finish` and return `header` as
`x-reproit-events`. Set `effects_complete` only when the adapter observed every persistent effect
in the operation. Tenant and resource identifiers must be non-secret structural identifiers.

The adapter enforces bounded identifiers, 256 events, a 60 KB encoded header, typed effects, one
return, no effects after return, hashed idempotency identity, and recursive structural redaction.
GraphQL callers may attach parser-produced `selection` mappings; never infer selections from
response content.

## Rack middleware (Rails, Sinatra, any Rack 2/3 app)

`ReproitBackendRb::Middleware` is a pure Rack middleware with no dependency on the rack gem: it
builds the canonical decoded input (JSON body up to 64 KB, decoded query values, lowercased
headers), begins the trace, and finishes it around the downstream response, attaching
`x-reproit-events` on scan-time requests. Handlers record observed effects through
`env["reproit.trace"]`:

```ruby
require "reproit_backend_rb"

capture = ReproitBackendRb::Capture.create(
  endpoint: "https://cloud.example.com/v1/events", # ingest endpoint
  api_key: "sk_live_...",                          # project API key (Authorization: Bearer)
  app_id: "app-id",                                # Cloud project app id
  build: "1.4.2"                                   # optional deployment identity
)

# Rails (config/application.rb):
config.middleware.use ReproitBackendRb::Middleware, capture: capture

# Sinatra (or any Rack builder):     capture: nil keeps scan-time only.
use ReproitBackendRb::Middleware, capture: capture

post "/orders" do
  trace = env["reproit.trace"]
  trace&.effect("write", resource: "orders", key: "1")
  # ...
end
```

Every adapter path fails closed: an instrumentation defect never breaks the request.

## Production capture mode (off by default)

Capture mode uploads finished traces to Cloud ingest without requiring `x-reproit-trace`. It is
config-gated: nothing leaves the process unless the host constructs a `Capture`.
`Capture.create(...)` returns `nil` (capture disabled, host unaffected) when the config is
unusable. `capture.record(trace)` never blocks, never raises, and never surfaces errors.

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
queued. Uploads use stdlib net/http on one background thread. `sdk/test/oracle_contract_test.js`
pins the `backend-server-error` tagging contract.

## Tests

```sh
cd sdk/reproit-backend-rb
ruby test/trace_test.rb && ruby test/capture_test.rb   # unit, stdlib only (node for the mirror)
gem install --user-install webrick rack                # e2e prerequisites, user-local
ruby test/e2e_test.rb                                  # WEBrick + Rack::Lint e2e
```
