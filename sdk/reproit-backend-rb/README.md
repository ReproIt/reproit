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
reproit internal debug replay-capture capture.json
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), 16 operations per batch,
48 KB capture payload (trailing effect events dropped first, `captureDroppedEffects` counts
them), bounded flush interval, per-request timeout, and at most `retry_limit` (cap 5) retries;
4xx responses are never retried. Redaction runs in `begin`/`effect`/`finish`, before anything is
queued. Uploads use stdlib net/http on one background thread. `sdk/test/oracle_contract_test.js`
pins the `backend-server-error` tagging contract.

## Capsule parity (outbound capture + hermetic replay)

This SDK is at full capsule parity with the Node reference (`sdk/reproit-backend-node`), pinned
byte-for-byte by `sdk/test/backend_replay_parity_test.js` and the shared behavior vectors:

- Outbound exchange capture at the library layer: a `Module#prepend` on `Net::HTTP#request`
  (covers HTTParty, Faraday's default adapter, Octokit, and everything else built on Net::HTTP);
  streaming responses consumed via `read_body { |chunk| ... }` (SSE/chunked, the LLM shape) are
  TEED, so the observed chunk boundaries record in `response.stream` while the app still
  consumes the live stream, and the exchange lands at EOF (an abandoned body records only what
  Net::HTTP itself drains).
- `ReproitBackendRb.wrap_pg(pg)` wraps the pg gem's `PG::Connection` statement surface:
  statements and results record as `pg`-protocol exchanges in the Node wire shape; in replay
  every connect constructor returns an in-process stub, so the app boots with the database down.
- `REPROIT_REPLAY=<capture.json>` flips every hook from recorder to stub: strict per-operation
  ordinal matching, bodies modulo recorded `$reproit` placeholders, first unmatched call fails
  closed (599 / DivergenceError) with the structured `REPROIT:DIVERGENCE` marker; prompt drift
  names the first differing message index for chat-shaped bodies. TZ, the wall clock
  (`Time.now` and `Process.clock_gettime(CLOCK_REALTIME)`, offset via singleton prepend, the
  Timecop pattern, replay mode only), and `Kernel#rand` (via `srand`) pin from the capture
  envelope.

Named capability gaps (recorded here so they are never a silent downgrade):

- `SecureRandom` reads OS entropy directly and CANNOT be pinned by the envelope; only the
  default `Kernel#rand` stream is seeded.
- The wall-clock prepend's blast radius is deliberate and bounded: it runs ONLY under
  `REPROIT_REPLAY`, offsets rather than freezes, and leaves every monotonic clock alone so
  duration math and timeout loops keep working. Code reading time through other channels
  (`DateTime.now` via `Date`, C extensions calling `gettimeofday`) is not pinned.
- Clients that bypass `Net::HTTP` (raw TCP sockets, libcurl bindings such as typhoeus/curb,
  async-http) are invisible to the prepend; they record through explicit `Instrument.db`-style
  calls or not at all.
- The pg wrap covers string statements with positional parameters on the `exec`/`exec_params`/
  `query`/`async_exec` surface; COPY, named prepared statements, and pipeline mode pass through
  unrecorded. mysql2/sqlite3/Sequel adapters are not wrapped.
- The non-block `request` path drains the response in one read, so its recorded SSE stream
  boundaries are coarse (whole-body); fine-grained boundaries come from the `read_body` tee.
- Replayed JSON bodies re-serialize from the canonically stored capture (sorted keys, compact
  separators, identical to Node): an app comparing raw response TEXT against later raw request
  text can observe the reordering; structural matching is unaffected.

## Tests

```sh
cd sdk/reproit-backend-rb
ruby test/trace_test.rb && ruby test/capture_test.rb   # unit, stdlib only (node for the mirror)
gem install --user-install webrick rack                # e2e prerequisites, user-local
ruby test/e2e_test.rb                                  # WEBrick + Rack::Lint e2e
```
