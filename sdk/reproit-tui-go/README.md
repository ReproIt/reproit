# ReproIt TUI SDK for Go

This SDK connects a Go terminal application to ReproIt. It records structural screen transitions and
crash paths so a production failure can be replayed by the local TUI runner.

## Capture frames

Pass the rendered cell grid or text after each settled frame:

```go
r := reproittui.New(reproittui.Config{
    AppID:    "my-tui",
    Endpoint: "https://ingest.reproit.com/v1/events",
})
defer r.InstallCrashHandler()()
defer r.Flush()

r.Observe(reproittui.ScreenContents{
    Grid: grid,
    CursorRow: row,
    CursorCol: col,
}, "key:Down")
```

Use `ScreenContents.Grid` with a cell-buffer renderer. If the application already has the rendered
text, call `ObserveContents(text, row, col, action)`. The SDK does not read from the terminal.

An edge is recorded only when the structural signature changes. Text labels are not part of the
signature, so translated interfaces retain the same structural identity. The Go implementation is
checked against the same golden vectors as the Rust runner.

## Report invariants

```go
r.Invariant("cart-total-nonnegative", func() error {
    if cart.Total() < 0 {
        return fmt.Errorf("total is %d", cart.Total())
    }
    return nil
})
```

An invariant becomes a finding only when it returns an error or panics and the same violation
reproduces.

## Events and crashes

The reporter sends `{appId, sentAt, ctx?, events}` batches. Without an endpoint, events go to the
configured development hook or are dropped. The crash handler records the current signature and
path, flushes the batch, and preserves the application's panic or signal behavior.

## Validate

```sh
cd sdk/reproit-tui-go
go test ./...
```

The tests check signatures, fingerprints, value classes, and cell rendering against the shared
golden vectors.
