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

### Named capability gaps (not silent downgrades)

- Rust has no monkeypatching, so the boundary is explicit: reqwest calls
  not routed through `instrument::http` and statements not routed through
  `pg`/`db::run` are invisible to capture and unavailable at replay.
- The RNG/clock shims pin only reads routed through the SDK
  (`replay_rng`, `now_millis`). Direct `rand::random`, `getrandom`, or
  `SystemTime::now` calls in application code are NOT pinned.
- `TZ` is exported to the environment; the std library does not consult it
  (chrono-style formatters do). Locale is not captured: Rust has no
  process locale, so the envelope omits it (absent, never guessed).
- tokio-postgres coverage: transactions on the raw client, COPY,
  LISTEN/NOTIFY and prepared portals are not recorded; parameters bind for
  JSON scalars only; result columns outside bool/int/float/text/json kinds
  record as null; the `command` tag derives from the statement verb.

## Status: preview, unreleased

This crate is `0.0.0`, unpublished, and **outside the ReproIt 1.x compatibility
promise**. There is no release: the version number is not a claim that one
exists. Its contract may change before the backend pillar is promoted to the
stable surface, which requires field evidence from at least two independent
uses. See `docs/compatibility.md` in the main repository.

Capture mode is bounded by construction: a 64-item drop-oldest queue, 16
operations per batch, a 48 KB payload cap, a 100 ms flush floor, and one worker
thread. `record()` never blocks and never panics.

## License

Apache-2.0
