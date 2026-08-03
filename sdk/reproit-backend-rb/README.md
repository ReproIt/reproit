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
  endpoint: "https://cloud.example.com/v1/capture-batches", # ingest endpoint
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
only, no finding). A 5xx capture is posted as one universal capture-batch-v1 containing exactly that
operation, carrying the full redacted start/effects/return sequence for deterministic local
replay:

```sh
# pull the occurrence the capture became, and re-execute it locally:
reproit occ_<id>
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), one operation per batch,
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

## Agent oracles (authored assertions on LLM/agent operations)

An agent operation that "succeeds" with a wrong answer never trips the 5xx sampler. The agent
oracle API lets the operation mark its own contract violation on the trace:

```ruby
trace.oracle(ReproitBackendRb::AGENT_RESPONSE_ORACLE, { "expected" => "json" })
# ids: agent-response-content | agent-guardrail-violation | agent-loop-bound-exceeded
```

Semantics, identical to the Node reference and the Python port: the marker rides as an `emit`
effect (resource `reproit-oracle`) so the wire vocabulary is unchanged; unknown ids raise
`TraceError` so a typo cannot mint an oracle category; a marked operation is ALWAYS captured,
like a 5xx; the batch's failure observation is a `contract-violation` (an authored assertion,
not a runtime exception) whose signature carries the marked id; and `capture_payload` stamps the
marked id in the capsule's `oracle` field instead of `backend-server-error`. The three ids are
existing entries in `crates/reproit/oracle-registry.json`; no new ids exist here.

## CI capture mode (the flaky-CI wedge)

`ReproitBackendRb::CI.suite(name)` returns a Minitest-backed test declarator whose trigger
identity is the TEST, not an inbound HTTP request (it is autoloaded, and pulls minitest/autorun;
requiring the SDK alone never does):

```ruby
require "reproit_backend_rb"

t = ReproitBackendRb::CI.suite("checkout")
t.call("order total applies the configured tax rate") do
  assert_equal 125, order_total(100, CONFIG_URL)
end
```

- Without env, the wrapper is plain Minitest, except that tests run in DECLARATION order (never
  shuffled), like the Node reference's node:test: the wedge exists to capture order-dependent
  state, so capture and replay must walk the suite identically.
- `REPROIT_CI_CAPTURE=1`: every test runs inside its own trace with the Net::HTTP hook and db
  helper live, so dependency exchanges and the determinism envelope record exactly as production
  capture does. A FAILING test spools a version-2 `reproit-backend-capture` capsule to a bounded
  on-disk spool (`REPROIT_CI_SPOOL`, default `.reproit/ci-spool`; total-byte cap
  `REPROIT_CI_SPOOL_MAX`, default 16 MiB, clamped to [4 KiB, 64 MiB]; over-cap capsules are
  dropped and counted in `dropped.count`, never silently) and announces it with the
  `REPROIT:CI-CAPSULE` stderr marker. The test identity rides in the existing `operation` field
  as `test:<suite>#<test>` and the oracle is the existing `backend-authored-invariant` id: no
  new protocol fields, no new oracle ids.
- `REPROIT_REPLAY=<capsule>`: the SAME wrapper re-runs ONLY the capsule's named test (everything
  else skips, so the exit code speaks for the named test alone) while the SDK serves the
  recorded exchanges in process, and reports the observed result as the structured
  `REPROIT:CI-TEST` stderr marker `reproit check` parses. `reproit check <capsule> --exec "ruby
  tests/checkout_test.rb"` maps it to the four-way verdict (reproduced / fixed / diverged /
  inconclusive), proven end to end by `validation/backend/rb-flaky-ci-e2e/run.sh` against
  `fixtures/rb-flaky-ci-fixture`.

Honest limit (same as Node): replay pins the envelope and the recorded exchanges, which is the
whole boundary this SDK can see. A race the boundary cannot see (thread scheduling, shared
memory) is not reproduced by this capsule; `reproit check` reports such runs Inconclusive.

## Level matrix against the Node reference

The founder rule is one level for all backend SDKs: every Node surface is either ported at the
same semantics or a NAMED impossible row here, never a silent gap.

| Node surface (sdk/reproit-backend-node) | Ruby | Notes |
| --- | --- | --- |
| Scan-time trace adapter (bounds, redaction, header wire) | Level | byte-identical canonical JSON |
| Framework adapters (Express/Fastify) | Level | Rack middleware (Rails, Sinatra, any Rack 2/3) |
| Production capture mode (sampler, batch, bounds) | Level | stdlib net/http worker thread |
| Agent oracle API (`trace.oracle`, marked capture) | Level | this document, section above |
| CI capture mode (`ci.suite`, spool, replay markers) | Level | Minitest instead of node:test |
| Outbound HTTP capture (http/https/fetch wrap) | Level | one `Net::HTTP#request` prepend |
| Database capture (`wrapPg`) | Level | `wrap_pg` on the pg gem plus `Instrument.db` |
| Hermetic replay (ordinals, 599/divergence, stream shape) | Level | pinned by shared vectors |
| Envelope pinning: TZ, wall clock, seeded rand | Level | Timecop-pattern prepend, `srand` |
| Envelope pinning: `SecureRandom` | Named gap | reads OS entropy directly; CANNOT be pinned |
| HTTP clients off `Net::HTTP` | Named gap | libcurl (typhoeus/curb), raw sockets, async-http |
| pg edge shapes | Named gap | COPY, prepared statements, pipeline; no mysql2/sqlite3/Sequel |
| Non-`Time` clock reads | Named gap | `DateTime` via `Date`, C extensions calling `gettimeofday` |

The named rows are genuinely impossible or out of scope at this boundary, stated here so they
are never a silent downgrade; everything else is pinned by the shared behavior vectors,
`sdk/test/backend_replay_parity_test.js`, the SDK suite, and the two acceptance gates
(`validation/hermetic-e2e.sh`, `validation/backend/rb-flaky-ci-e2e/run.sh`).

## Tests

```sh
cd sdk/reproit-backend-rb
ruby test/trace_test.rb && ruby test/capture_test.rb   # unit, stdlib only (node for the mirror)
ruby test/ci_test.rb                                   # CI capture mode, stdlib only
gem install --user-install webrick rack                # e2e prerequisites, user-local
ruby test/e2e_test.rb                                  # WEBrick + Rack::Lint e2e
validation/backend/rb-flaky-ci-e2e/run.sh              # flaky-CI wedge acceptance (from repo root)
```
