# reproit-backend

Rust adapter for [ReproIt](https://github.com/ReproIt/reproit). Mount it and the
backend oracles move from the **black-box tier** (status, response shape,
round-trip identity) to the **effect-grounded tier**: what the handler actually
read and wrote, not just what the response claimed.

That difference is what the oracles for tenant isolation, lost updates,
transaction atomicity and idempotency need. Without an adapter they abstain,
because the evidence to judge them does not exist in the response.

```toml
[dependencies]
reproit-backend = { version = "0.1", features = ["axum"] }
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

## Status: preview

This crate is **0.x and outside the ReproIt 1.x compatibility promise**. Its
contract may change before the backend pillar is promoted to the stable surface,
which requires field evidence from at least two independent uses. See
`docs/stability.md` in the main repository.

Capture mode is bounded by construction: a 64-item drop-oldest queue, 16
operations per batch, a 48 KB payload cap, a 100 ms flush floor, and one worker
thread. `record()` never blocks and never panics.

## License

Apache-2.0
