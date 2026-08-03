# reproit-backend

Rust adapter for [ReproIt](https://github.com/ReproIt/reproit). Mount it and the
backend oracles move from the **black-box tier** (status, response shape,
round-trip identity) to the **effect-grounded tier**: what the handler actually
read and wrote, not just what the response claimed.

That difference is what the oracles for tenant isolation, lost updates,
transaction atomicity and idempotency need. Without an adapter they abstain,
because the evidence to judge them does not exist in the response.

**Not published, and not blocked on being published.** The crate is `0.0.0` and
not on crates.io, but `publish = false` only stops it being uploaded: a git
dependency resolves and builds today, which is all a service needs to move from
the black-box tier to effect-grounded verdicts.

```toml
[dependencies]
reproit-backend = { git = "https://github.com/ReproIt/reproit", features = ["axum"] }
```

```rust
use reproit_backend::{MiddlewareConfig, ReproitLayer};

let app = Router::new()
    .route("/users", get(list_users))
    .layer(ReproitLayer::new(MiddlewareConfig {
        capture,
        ..MiddlewareConfig::default()
    }));
```

`actix` is the other feature-gated framework; the core adapter is
dependency-light and framework-agnostic.

## Capsule parity (features `instrument`, `pg`)

Full capsule parity with the Node reference (`reproit-backend-node`), wire
version 2, parity-pinned byte for byte by
`sdk/test/backend_replay_parity_test.js`:

- **Outbound exchange capture**: route reqwest calls through
  `instrument::http::send` (buffered) or `instrument::http::send_stream`
  (a tee: chunks reach the app live, the exchange records at end of body,
  an abandoned stream records nothing). Request line+headers+body and
  response status+headers+body are recorded with the Node bounds: 8 KiB
  inline body (over-cap keeps byte count + sha256 over every byte +
  truncation marker), 32 headers capped over name-sorted order, 64 db rows,
  128 stream chunk boundaries.
- **tokio-postgres** (`pg` feature): `pg::connect` + `Client::query` /
  `Client::execute` emit the Node `pg` wire shape; in replay, `connect` is
  a stub (the app boots with the database down) and statements are served
  from the capture. `instrument::db::run` remains the generic boundary.
- **Redaction at source**: the same keyword + `$reproit` placeholder pass,
  byte-compatible placeholders, applied inside exchange bodies.
- **Determinism envelope**: `determinism_envelope()` stamps
  observedAtMs/tz/runtime/os/arch/replaySeed; replay pins `TZ`, offsets the
  clock (`instrument::now_millis`), and seeds `instrument::replay_rng`.
- **Replay** (`REPROIT_REPLAY`): strict per-operation ordinals, exact
  method+origin-path, bodies modulo placeholders; the first unmatched call
  fails closed with the `REPROIT:DIVERGENCE` marker (byte-identical to
  Node's, `bodyDelta` naming the first differing message index for
  chat-shaped bodies, byte offset otherwise, with an ABSENT-vs-null
  distinction); truncated bodies and stream shapes serve a hard 599.

## Agent oracle API

The Node reference's agent oracle vocabulary, verbatim: three registry ids
(`agent-response-content`, `agent-guardrail-violation`,
`agent-loop-bound-exceeded`, exported as `AGENT_ORACLES`) an LLM/agent
operation marks on its own trace as authored assertions.

```rust
// In a handler (via the middleware's request-extension Recorder), or on a
// hand-begun BackendTrace:
recorder.oracle(reproit_backend::AGENT_GUARDRAIL_ORACLE,
    Some(json!({"tool": "delete_order"})))?;
```

Same semantics as Node's `trace.oracle(id, detail)`:

- Unknown ids are rejected (`TraceError::InvalidOperation`) against the same
  vocabulary, so a typo cannot mint an oracle category.
- The marker rides as an `emit` effect on the existing `reproit-oracle`
  resource; the wire vocabulary is unchanged.
- A marked operation is ALWAYS captured by production capture mode, like a
  5xx, even when it returns 200/success.
- The capture batch's failure observation carries the marked id in its
  signature and reports `contract-violation` (an authored assertion), not
  `exception` (the bare-5xx default). `marked_oracle(events)` is public.

## CI capture mode (module `ci`, feature `instrument`)

The flaky-CI wedge, Node's `ci.js` ported to cargo test's process model.
Wrap a test body in `ci::run(suite, test, body)`:

```rust
#[tokio::test]
async fn b_order_total_applies_the_configured_tax_rate() {
    reproit_backend::ci::run("checkout", "order total applies the configured tax rate", async {
        assert_eq!(order_total(100.0, &config_url()).await, 125.0);
    })
    .await;
}
```

- Trigger identity is the TEST: the capsule's `operation` field carries
  `test:<suite>#<test>` and the oracle is the existing
  `backend-authored-invariant` registry id. No new wire fields, no new
  oracle ids.
- `REPROIT_CI_CAPTURE=1`: every wrapped test runs inside its own trace, the
  instrument boundaries record dependency exchanges and the determinism
  envelope, and a FAILING test spools a version-2 capsule to the bounded
  spool (`REPROIT_CI_SPOOL`, default `.reproit/ci-spool`; total-bytes cap
  `REPROIT_CI_SPOOL_MAX`, default 16 MiB, clamped 4 KiB..64 MiB; over-cap
  capsules are dropped and counted in `dropped.count`, never silently). The
  failure identity is the panic payload, recorded as the return event's
  `output.error`.
- `REPROIT_REPLAY=<capsule>`: the SAME wrapper re-runs only the capsule's
  named test with every recorded exchange served in process and the envelope
  pinned, and reports the observed result as the `REPROIT:CI-TEST` stderr
  marker `reproit check` parses. `reproit check <capsule> --exec "cargo test
  ... -- --test-threads=1"` maps it to the four-way verdict (reproduced /
  fixed / diverged / inconclusive); a plain rerun passing OUTSIDE the
  capsule is flaky evidence and never reads as Fixed.
- Without either env the wrapper runs the body untouched.

Named deviations from the Node reference, forced by cargo test's model (all
also documented in the `ci` module header):

- **Adoption is per test body**, not a runner-level `test()` replacement: a
  Rust library cannot intercept `#[test]` functions, so each test wraps its
  body in `ci::run` the way an app adopts a logger. An unwrapped test is
  invisible to capture, exactly like an unwrapped client.
- **Order-dependent suites need `-- --test-threads=1`** (sequential,
  name-sorted); libtest's default parallel run cannot be sequenced from
  library code.
- **Failure identity is the panic payload**; tests failing by process abort
  and `#[should_panic]` inversions are not capturable at this layer.
- **Replay skips are silent**: non-target tests do not run their bodies and
  libtest reports them as passed, where node:test marks them skipped. The
  `REPROIT:CI-TEST` marker still speaks for the named test alone.
- Markers (`REPROIT:CI-TEST`, `REPROIT:CI-CAPSULE`, `REPROIT:DIVERGENCE`)
  are written straight to fd 2 so libtest's output capture cannot swallow
  them; `--nocapture` is not required.

Fixture and gate: `examples/rs-flaky-ci-fixture` (planted order-dependent
failure, invisible in a plain run) and `validation/backend/rs-flaky-ci-e2e/
run.sh` (six legs, cloned from the Node gate).

## Level matrix against the Node reference

Founder rule: every backend SDK sits at the same level as
`reproit-backend-node` in all ways; anything genuinely impossible is a NAMED
gap here, never silent.

| Node surface | Rust | Notes |
| --- | --- | --- |
| Scan-time trace + events header | yes | `BackendTrace`, byte-identical wire |
| Context / selection / redaction | yes | same bounds, same `$reproit` placeholders |
| Ambient trace (AsyncLocalStorage) | yes | tokio task-local, `instrument::scope` |
| Framework adapters (express, fastify) | yes | axum layer, actix middleware |
| Production capture mode | yes | same queue/batch/retry bounds |
| Agent oracle API + marked capture | yes | `BackendTrace::oracle`, `Recorder::oracle` |
| Outbound HTTP capture | partial, NAMED | explicit boundary only, gap 1 below |
| DB capture (pg) | partial, NAMED | wrapper/`db::run` only, gaps 1 and 4 |
| Replay + `REPROIT:DIVERGENCE` | yes | byte-identical marker, parity-pinned |
| Envelope seed/clock/TZ pins | partial, NAMED | shim-routed reads only, gaps 2 and 3 |
| CI capture mode (spool, markers) | yes, NAMED deltas | `ci::run` per test, deviation list above |
| Replay skip semantics | partial, NAMED | silent skip vs node:test's marked skip |

### Named capability gaps (not silent downgrades)

1. Rust has no monkeypatching, so the boundary is explicit: reqwest calls
   not routed through `instrument::http` and statements not routed through
   `pg`/`db::run` are invisible to capture and unavailable at replay.
2. The RNG/clock shims pin only reads routed through the SDK
   (`replay_rng`, `now_millis`). Direct `rand::random`, `getrandom`, or
   `SystemTime::now` calls in application code are NOT pinned.
3. `TZ` is exported to the environment; the std library does not consult it
   (chrono-style formatters do). Locale is not captured: Rust has no
   process locale, so the envelope omits it (absent, never guessed).
4. tokio-postgres coverage: transactions on the raw client, COPY,
   LISTEN/NOTIFY and prepared portals are not recorded; parameters bind for
   JSON scalars only; result columns outside bool/int/float/text/json kinds
   record as null; the `command` tag derives from the statement verb.

## Publication

This crate is `0.0.0` and is not published to crates.io: depend on it by git,
as shown above. The version number is not a claim that a release exists.

Capture mode is bounded by construction: a 64-item drop-oldest queue, 16
operations per batch, a 48 KB payload cap, a 100 ms flush floor, and one worker
thread. `record()` never blocks and never panics.

## License

Apache-2.0
