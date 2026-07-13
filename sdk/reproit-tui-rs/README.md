# reproit-tui (Rust SDK)

Production telemetry SDK a **Rust terminal-UI app** embeds to report sessions,
coverage edges, and crash signatures, computing the SAME canonical TUI screen
signature the [`reproit __tui`](../../crates/reproit/src/backends/tui.rs) runner
computes. A production crash reported here carries a signature the runner can
replay locally.

## Parity by construction (not a port)

This SDK depends on the shared `reproit-tui-sig` crate, which the runner itself
uses. So the signature is byte-identical to the runner **at compile time**, with
no parallel port to keep in sync. The Go/TS/Python TUI SDKs port that logic and
are pinned to the same golden vectors (`tui_signature_vectors.json` at the repo
root); this crate just links it. The parity test (`tests/parity.rs`) still asserts
all golden vectors as a guard against the shared crate drifting.

## The signature namespace

TUI signatures are a **separate namespace** from the accessibility-tree (a11y)
signatures: a terminal has no a11y role tree, so the descriptor source is the
rendered VT cell grid normalized to a locale-invariant layout skeleton, not a
`Node` role tree. They are NOT expected to match the a11y vectors in
`signature_vectors.json`. What IS shared is the hash family (FNV-1a 32-bit) and
the value-class buckets. See `docs/signature.md`, "Terminal and instrumented
surfaces".

## Capture

A TUI app renders its own cells, so you hand the SDK the rendered screen:

```rust
use reproit_tui::{ReproIt, Config, ScreenContents, SpoolTransport};

let sdk = ReproIt::new(
    Config { app_id: "myapp".into(), ctx: None },
    Box::new(SpoolTransport::stderr()), // or your own Transport (HTTP)
);
sdk.install_crash_handler(); // flush a panic with the path that led to it

// each rendered frame:
let screen = ScreenContents::from_text(rendered_text, cursor_row, cursor_col);
sdk.observe(&screen, "key:Down");
// ...
sdk.flush();
```

- **ratatui** apps: read each `ratatui::buffer::Buffer` row's symbols into a
  `String` and use `ScreenContents::from_rows(&rows, row, col)`. ratatui is the
  dominant Rust TUI framework and `Buffer` is the natural capture point.
- **crossterm-direct** apps (and helix/yazi-style custom forks): pass the text you
  drew via `ScreenContents::from_text(...)`. crossterm is the common substrate
  under ratatui and those forks, so one capture model covers them all.

An **edge** is recorded only when the structural signature changes (mirroring the
runner's coverage edges); unchanged frames are no-ops. The wire contract is
identical to every other reproit SDK: `{appId, sentAt, ctx?, events}`.

## Transport

`Transport` is a trait. `SpoolTransport` (the default) writes newline-delimited
JSON to a file or stderr, so an app runs with no networking wired. To POST to the
ingest endpoint, implement `Transport::send` over any blocking HTTP client:

```rust
struct Http { url: String }
impl reproit_tui::Transport for Http {
    fn send(&self, batch_json: &str) {
        // ureq::post(&self.url).send_string(batch_json) ... (best-effort, drop on error)
    }
}
```

HTTP is intentionally not a hard dependency so the default build stays embed-light.

## App invariants

Declare a predicate the app must satisfy in EVERY state the fuzzer reaches (a
running total never negative, exactly one pane focused). Under the fuzzer reproit
evaluates it on each observed screen and reports the failures as `invariant`
findings; in production the registry is inert (zero-overhead until a run
reproduces it).

```rust
sdk.invariant("cart-total-nonneg", move || {
    if cart.total() < 0 {
        return Err(format!("total is {}", cart.total()));
    }
    Ok(())
});
```

The predicate returns `Ok(())` when it holds; `Err(message)` (or a panic) marks
it violated, with that text as the finding message. Registration is idempotent by
id. Under the fuzzer the SDK writes each violation to the marker file the
`reproit __tui` runner provisions (`REPROIT_INVARIANT_FILE`); stderr is not used
because a PTY conflates it with the rendered frames.

## Crash handling

`install_crash_handler()` sets a panic hook that records the panic as an `error`
event (current signature + path) and flushes before the previous hook runs. This
is the idiomatic Rust crash path; true fatal-signal (SIGSEGV) capture can be added
with a signal crate if an app needs it.

## Test

```sh
cargo test -p reproit-tui-sdk
```

This crate is a workspace member, so `cargo test --workspace` (run in CI) already
exercises the parity gate and the reporter unit tests.
