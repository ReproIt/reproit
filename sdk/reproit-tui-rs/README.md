# ReproIt TUI SDK for Rust

This SDK connects a Rust terminal application to ReproIt. It records structural screen transitions
and crash paths so a production failure can be replayed by the local TUI runner.

## Capture frames

Pass the rendered text or cell rows after each settled frame:

```rust
use reproit_tui::{Config, ReproIt, ScreenContents, SpoolTransport};

let sdk = ReproIt::new(
    Config { app_id: "myapp".into(), ctx: None },
    Box::new(SpoolTransport::stderr()),
);
sdk.install_crash_handler();

let screen = ScreenContents::from_text(rendered_text, cursor_row, cursor_col);
sdk.observe(&screen, "key:Down");
sdk.flush();
```

For ratatui, convert each `ratatui::buffer::Buffer` row into a `String` and call
`ScreenContents::from_rows`. For crossterm or a custom renderer, pass the text already written to
the terminal through `ScreenContents::from_text`.

An edge is recorded only when the structural signature changes. Text labels are not part of the
signature, so translated interfaces retain the same structural identity. The Rust SDK and the local
runner share `reproit-tui-sig`, which keeps their signatures identical.

## Report invariants

Register exact application rules that ReproIt may confirm during a run:

```rust
sdk.invariant("cart-total-nonnegative", move || {
    if cart.total() < 0 {
        return Err(format!("total is {}", cart.total()));
    }
    Ok(())
});
```

An invariant becomes a finding only when its predicate returns an error or panics and the same
violation reproduces.

## Transport and crashes

`SpoolTransport` writes newline-delimited JSON to a file or stderr. Implement the `Transport` trait
to send batches to an HTTP endpoint. HTTP is not included as a runtime dependency.

`install_crash_handler` records the current structural signature and path, flushes the batch, and
then runs the previous panic hook.

## Validate

```sh
cargo test -p reproit-tui-sdk
```

The tests check the SDK against the shared golden TUI signature vectors.
