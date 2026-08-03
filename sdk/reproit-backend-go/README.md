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

## Installing

The module path `github.com/ReproIt/reproit/sdk/reproit-backend-go` is not a published
repository: the source lives in the main repository under
`sdk/reproit-backend-go`. Vendor it or point at a checkout with a replace
directive until it is published:

```
require github.com/ReproIt/reproit/sdk/reproit-backend-go v0.0.0
replace github.com/ReproIt/reproit/sdk/reproit-backend-go => /path/to/reproit/sdk/reproit-backend-go
```

## net/http middleware and Fiber v2 adapter

The `net/http` middleware begins the trace from the decoded request (JSON body, decoded query
values, lowercased headers), finishes it when the response is complete, and attaches
`x-reproit-events` on scan-time requests. Handlers record observed effects through the recorder
carried on the request context:

```go
import reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"

config := reproit.NewCaptureConfig(
    "https://cloud.example.com/v1/capture-batches", // ingest endpoint
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

Fiber v2 is a separate Go module (`github.com/ReproIt/reproit/sdk/reproit-backend-go/fiber`) so the core stays
dependency-free. It is the same adapter behind Fiber's buffered request/response model; handlers
fetch the recorder with `reproitfiber.From(c)`:

```go
import reproitfiber "github.com/ReproIt/reproit/sdk/reproit-backend-go/fiber"

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
only, no finding). A 5xx capture is posted as one universal capture-batch-v1 containing exactly that
operation, carrying the full redacted start/effects/return sequence for deterministic local
replay:

```sh
# pull the occurrence the capture became, and re-execute it locally:
reproit occ_<id>
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), one operation per batch,
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

## Agent oracle API

LLM/agent operations can mark authored failure assertions on their own trace with
`trace.Oracle(id, detail)`. The id must be one of the registry agent oracle ids
(`agent-response-content`, `agent-guardrail-violation`, `agent-loop-bound-exceeded`,
exported as `AgentResponseOracle` / `AgentGuardrailOracle` / `AgentLoopBoundOracle`); unknown
ids return `ErrInvalidOperation` so a typo cannot mint an oracle category. The marker rides as
an `emit` effect on the resource `reproit-oracle`, so the scan-time wire vocabulary is
unchanged. Semantics match the Node reference exactly: a marked operation is ALWAYS captured
by capture mode (like a 5xx, even when the return reports success), and the capture batch's
failure observation carries the marked id as a `contract-violation` (an authored assertion),
where a bare 5xx stays the `exception` it always was. `MarkedOracle(events)` returns the first
marked id on a finished trace's events.

```go
if err := trace.Oracle(reproit.AgentGuardrailOracle, map[string]any{
    "tool": "delete_order",
}); err != nil {
    // unknown oracle id: fix the constant, do not invent one
}
```

## CI capture mode (the flaky-CI wedge)

`reproitci` (same module, `github.com/ReproIt/reproit/sdk/reproit-backend-go/reproitci`) binds a `testing.T`
test to a trigger identity that is the TEST, not an inbound HTTP request. The wire is the
existing capture payload: the identity rides in the existing `operation` field as
`test:<suite>#<test>`, the oracle is the existing `backend-authored-invariant` registry id (a
test IS an authored invariant), and the markers are the existing structured stderr lines
(`REPROIT:CI-TEST`, `REPROIT:CI-CAPSULE`, `REPROIT:DIVERGENCE`). No new protocol fields, no
new oracle ids.

```go
func TestOrderTotal(t *testing.T) {
    ct := reproitci.Wrap(t, "checkout")
    total, err := OrderTotal(ct.Context(), reproit.WrapClient(nil), configURL, 100)
    if err != nil {
        ct.Fatalf("order total: %v", err)
    }
    if total != 125 {
        ct.Fatalf("order total = %v, want 125", total)
    }
}
```

- `REPROIT_CI_CAPTURE=1`: each wrapped test runs under its own capture-envelope trace;
  outbound calls carry `ct.Context()` so the SDK boundaries record every dependency exchange. A
  FAILING test spools a version-2 capsule to a bounded on-disk spool (`REPROIT_CI_SPOOL`,
  default `.reproit/ci-spool`; total-bytes cap `REPROIT_CI_SPOOL_MAX`, default 16 MiB, clamped
  to [4 KiB, 64 MiB]; over-cap capsules are dropped and counted in `dropped.count`, never
  silently) and announces it with a `REPROIT:CI-CAPSULE` stderr line.
- `REPROIT_REPLAY=<capsule>`: the SAME wrapper skips every test but the capsule's named one,
  the SDK serves the recorded exchanges in process (no upstream, no database), and the observed
  result is reported as a `REPROIT:CI-TEST` marker for `reproit check`.
- Neither env set: `Wrap` is inert.

`reproit check <capsule> --exec "<test command>"` re-runs the single named test directly:

```sh
reproit check capsule.json --exec \
  "go -C <dir> test -count=1 -run '^TestOrderTotal\$' 1>&2"
```

Failure identity: assertions made through the wrapper (`ct.Errorf` / `ct.Fatalf` / ...) record
the bounded message that `reproit check` compares between the recorded run and a replay; a
failure raised on the bare `*testing.T` still fails and spools, but with an empty identity
(check then treats any replayed failure as the recorded one). Two Go mechanics are explicit
where Node hides them: outbound calls must carry `Context()` (no ambient async storage), and
the replay command must redirect `1>&2` in local directory mode, because `go test` merges the
test binary's stderr into stdout and package-list mode buffers a passing binary's output away.
Honest limit, same as every SDK: replay pins the envelope and the recorded exchanges; a race
the boundary cannot see is reported Inconclusive, never a fake reproduction.

## Level matrix against the Node reference

Same level in ALL surfaces the Node SDK has; genuinely impossible surfaces are named rows,
never silent downgrades.

| Node surface | Go | Level |
| --- | --- | --- |
| Scan-time trace (`BackendTrace`, bounds, redaction, canonical wire) | `Begin`/`Effect`/`Finish`/`Header` | Level (byte-parity golden tests) |
| Framework adapters (Express, Fastify) | `net/http` middleware, Fiber v2 module | Level (per-ecosystem frameworks) |
| Production capture mode (`Capture`) | `NewCapture`/`Record`/`Flush` | Level |
| Agent oracle API (`trace.oracle`, marked capture, contract-violation) | `trace.Oracle`, `MarkedOracle` | Level |
| Exchange capture (http/db, stream chunk boundaries) | `Transport`/`WrapClient`, `RunDB`, `SQLDriver` | Level, but opt-in (below) |
| Hermetic replay (ordinal match, `$reproit` wildcards, `REPROIT:DIVERGENCE`) | same, byte-identical marker | Level |
| Envelope pinning (TZ, clock, seeded RNG) | TZ + `ReplayNow` + `math/rand` v1 + `NewReplayRNG` | Level, named gaps (below) |
| CI capture mode (`ci.suite`, spool caps, `REPROIT:CI-TEST`) | `reproitci.Wrap`, same caps and markers | Level |

Named impossible surfaces (Go the language, not this port):

- Automatic instrumentation: Node monkeypatches `http`/`fetch`/`pg` process wide; Go cannot.
  The boundary is explicit and opt-in; a client not routed through it is invisible to capture
  and unavailable at replay.
- Ambient trace propagation: Node's `AsyncLocalStorage` finds the trace implicitly; Go threads
  `context.Context` (middleware does it for handlers, `reproitci` hands it to tests).
- `time.Now` cannot be patched process wide; code must read `reproit.ReplayNow()`.
- `math/rand/v2`'s global source cannot be reseeded; only the v1 global source and
  `NewReplayRNG` pin. `crypto/rand` is unpinnable by design.
- The `database/sql` driver API exposes no server command tag; the recorded `command` is
  derived from the statement's leading verb. Context-less driver calls carry no ambient trace
  and pass through unrecorded.

## Tests

```sh
cd sdk/reproit-backend-go
go test ./...        # unit + net/http e2e + reproitci (child-process suite), zero dependencies
cd fiber && go test ./...  # Fiber v2 adapter (separate module)
node ../test/backend_replay_parity_test.js  # byte parity against the Node reference
../../validation/backend/go-hermetic-e2e/run.sh  # money test under PORTABILITY
../../validation/backend/go-flaky-ci-e2e/run.sh  # flaky-CI wedge, six legs
```
