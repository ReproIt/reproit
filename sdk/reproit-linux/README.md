# ReproIt SDK for Linux

This SDK connects native GTK and Qt applications to ReproIt. It captures the accessible widget
structure, records structural transitions and crash paths, and uses the same signature contract as
the Linux runner.

## Install

```sh
pip install \
  'reproit-linux @ git+https://github.com/ReproIt/reproit.git#subdirectory=sdk/reproit-linux'
```

The live widget walk requires PyGObject and the applicable GTK, Qt, ATK, or AT-SPI runtime
libraries.

## Connect an application

GTK applications pass their top-level widget:

```python
from reproit_linux import ReproIt

window = builder.get_object("main_window")
ReproIt.init(
    app_id="example",
    endpoint="https://ingest.reproit.com",
    api_key="pk_live_...",
    build_version="1.4.2",
    build_commit="abc123",
    root_widget=window,
)

button.connect("clicked", lambda *_: ReproIt.observe("tap:roll"))
```

Qt and other AT-SPI applications can pass the top-level accessible object:

```python
ReproIt.init(
    app_id="example",
    atspi_root=top_accessible,
    endpoint="https://ingest.reproit.com",
    api_key="pk_live_...",
)
```

Call `ReproIt.observe(action)` after a settled user action. An edge is recorded only when the
structural signature changes. Accessible labels do not enter the signature, so translated interfaces
retain the same structural identity.

## Capture an exploratory bug

Add `ReproIt.capture_bug()` to a debug action. It sends the rolling structural path and current
state without field values. Run `reproit create` from the application source directory. The CLI
replays and shrinks the path before it creates a confirmed bug.

## Report invariants

```python
ReproIt.invariant("cart-total-nonnegative", lambda: cart.total >= 0)

def one_row_selected():
    count = len(tree.get_selection())
    if count != 1:
        raise ValueError(f"{count} rows selected")
    return True

ReproIt.invariant("one-row-selected", one_row_selected)
```

An invariant becomes a finding only when it returns false, raises, or returns a failed result and
the same violation reproduces.

## Crash handling

Crash handling is enabled by default. It records the current signature and path, flushes the batch,
and then preserves the previous exception or signal behavior.

## Validate

```sh
cd sdk/reproit-linux
python3 tests/test_parity.py
python3 tests/test_capture.py
```

The parity test checks signatures against the shared golden vectors. The capture test checks
synthetic GTK and AT-SPI widget mappings. A live widget walk requires a Linux display and
accessibility bus.
