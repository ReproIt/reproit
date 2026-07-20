# ReproIt TUI SDK for Python

This dependency-free SDK connects a Python terminal application to ReproIt. It records structural
screen transitions and crash paths so a production failure can be replayed by the local TUI runner.

## Capture frames

Pass the rendered text after each settled frame:

```python
from reproit_tui_py import Reporter, ScreenContents

reporter = Reporter(
    app_id="my-tui",
    endpoint="https://ingest.reproit.com",
)
reporter.install_crash_handler()

screen = ScreenContents.from_text(frame_text, cursor=(row, col))
reporter.observe(screen, action="key:Down")
```

Rich and Textual applications can pass exported rendered text. Applications that retain row strings
or cells can use `ScreenContents.from_rows`:

```python
screen = ScreenContents.from_rows(
    ["┌────┐", "│ Hi │", "└────┘"],
    cursor=(1, 2),
)
reporter.observe(screen, action="render")
```

The SDK does not read from the terminal. An edge is recorded only when the structural signature
changes. Text labels are not part of the signature, so translated interfaces retain the same
structural identity. The Python implementation is checked against the same golden vectors as the
Rust runner.

## Report invariants

```python
reporter.invariant("cart-total-nonnegative", lambda: cart.total >= 0)

def one_pane_focused():
    count = sum(1 for pane in panes if pane.focused)
    if count != 1:
        raise ValueError(f"{count} panes focused")
    return True

reporter.invariant("one-pane-focused", one_pane_focused)
```

An invariant becomes a finding only when it returns false, raises, or returns a failed result and
the same violation reproduces.

## Events and crashes

The reporter sends strict version 1 event batches with optional bearer authentication. Set
`redact_labels=True` to omit human labels from edge events. Without an endpoint, batches use the
configured development hook.

`install_crash_handler` records uncaught exceptions and supported fatal signals, flushes the batch,
and then preserves the original traceback or signal exit.

## Validate

```sh
python3 sdk/reproit-tui-py/tests/test_parity.py
```

The tests check signatures, fingerprints, value classes, and frame capture against the shared golden
vectors.
