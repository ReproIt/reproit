# reproit-linux

Production telemetry SDK for native Linux GUI apps (GTK and Qt). The in-app twin
of `runners/linux-atspi.py`: that runner drives an app from OUTSIDE through the
AT-SPI bus; this SDK runs INSIDE the app, captures the widget tree through the
accessibility interface, computes the CANONICAL structural signature
(`docs/signature.md`) byte-for-byte identical to the Rust oracle and every other
SDK, and reports the state graph plus crash signatures to the reproit cloud so a
production crash replays as a deterministic test.

## Why Python

The Linux runner already drives AT-SPI via PyGObject (`gi` / `Atspi`), so a
Python SDK reuses the exact accessibility binding and the exact signature core
that is already proven against the golden vectors. PyGObject is the natural
in-app GTK binding (a GTK Python app already imports `gi.repository.Gtk`, so the
SDK attaches to the live widget tree with no FFI), and Qt apps expose AT-SPI,
which the same accessibility walk reads. The parity test runs here with
`python3`. A Go or C SDK would need a separate FNV / descriptor reimplementation
and a build step, and a clumsier embed story for a GTK/Qt app.

## Install

```
pip install reproit-linux            # core + reporter (pure stdlib)
pip install 'reproit-linux[capture]' # adds PyGObject for the live walk
```

A GTK/Qt app already ships PyGObject and the GObject-Introspection typelibs, so
the live walk works out of the box in-app.

## Usage

GTK (in-process ATK walk):

```python
from reproit_linux import ReproIt

win = builder.get_object("main_window")        # a top-level GtkWindow
ReproIt.init(
    app_id="example",
    endpoint="https://ingest.reproit.example",
    api_key="sk_...",
    root_widget=win,                            # the SDK walks win.get_accessible()
)

# Call after each user action (or wire to your signal handlers):
button.connect("clicked", lambda *_: ReproIt.observe("tap:roll"))
```

Qt (or any AT-SPI toolkit) - pass the AT-SPI root accessible:

```python
ReproIt.init(app_id="example", atspi_root=top_accessible, endpoint=..., api_key=...)
```

A fatal crash flushes the session automatically: `install_crash_handler` (on by
default) hooks `sys.excepthook` and the fatal native signals
(SIGSEGV/SIGABRT/SIGBUS/SIGFPE), records an error event carrying the graph path,
flushes, then chains to the prior handler / re-raises so the crash is not
swallowed.

## How a widget folds into the descriptor

Both capture paths funnel every accessible through `capture.node_from_attrs`,
which produces the SAME `signature.Node` the other SDKs use:

| accessible field                         | Node field | notes |
|------------------------------------------|------------|-------|
| ATK / AT-SPI Role name (e.g. `PUSH_BUTTON`) | `role`  | mapped via `ATSPI_ROLE_TO_ROLE` (shared with the runner), unknown roles -> `node` |
| `accessible-id` / buildable id           | `id`       | empty -> omitted |
| `PASSWORD_TEXT` / `SPIN_BUTTON`          | `type`     | `password` / `number` refinement on a `textfield` |
| Value / Text interface, or live name     | `value`    | bucketed by `value_class` into the V: section, only for value-bearing roles |
| accessible name / label text            | (excluded) | localized text NEVER enters the hash (rule 1); kept only as a display-only label list |

Promotions match the runner: `STATUS_BAR` and an active `live` / `container-live`
region become the value-role `status`; a `PROGRESS_BAR` that exposes a value
becomes `progressbar` (otherwise it is the transient `progress`, dropped).
Transient roles (toast/snackbar/spinner/progress/tooltip/badge) and the explicit
`transient` flag drop the node and its subtree before hashing.

## Tests

```
python3 tests/test_parity.py    # all 25 golden vectors reproduce byte-for-byte
python3 tests/test_capture.py   # widget-tree -> descriptor mapping (synthetic trees)
```

The parity test is the cross-language gate (mirrors the Rust oracle's
`golden_vectors_match` and `runners/test_signature.py`). The capture test
exercises the mapping with synthetic ATK / AT-SPI accessibles; the LIVE
GTK/AT-SPI walk against a running app needs a Linux display and a11y bus and is
not exercised headless.
