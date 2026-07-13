# reproit-tui-py (Python SDK)

The production telemetry SDK a **Python terminal-UI (TUI)** application embeds to
report sessions, coverage edges and crash signatures to the reproit cloud, and to
compute the SAME canonical TUI screen signature the fuzz runner computes. A
production crash reported by this SDK carries the exact state signature the
[`reproit __tui`](../../crates/reproit/src/backends/tui.rs) runner can replay
locally.

Target frameworks: **Textual, Rich, urwid, prompt_toolkit**, or any hand-rolled
raw-mode dashboard. Pure stdlib, no third-party dependency.

## The signature namespace (read this)

TUI signatures are a **separate namespace** from the accessibility-tree (a11y)
signatures. A terminal has no a11y role tree, so the descriptor SOURCE is the
rendered terminal screen (the VT cell grid), normalized to a locale-invariant
layout skeleton, NOT a `Node` role tree. Per
[`docs/signature.md`](../../docs/signature.md) ("Terminal and instrumented
surfaces"), **TUI signatures are NOT expected to match the a11y golden vectors in
`signature_vectors.json`**. The canonical source (and the parity target) is the
Rust crate [`crates/tui-sig/src/lib.rs`](../../crates/tui-sig/src/lib.rs), which
the runner [`crates/reproit/src/backends/tui.rs`](../../crates/reproit/src/backends/tui.rs)
shares directly. The golden vectors live at the repo root in
[`tui_signature_vectors.json`](../../tui_signature_vectors.json).

What *is* shared with every other reproit surface: the **hash family**, FNV-1a
32-bit (offset basis `0x811c9dc5`, prime `0x01000193`, 8-char zero-padded
lowercase hex), and the `value_class` buckets
(`EMPTY`/`ZERO`/`NEG`/`POS1`/`POS2`/`POS3`/`POSL`, with `NONEMPTY` as the
locale-safe fallback) under the strict period-decimal grammar. So a TUI signature
is in the same 8-hex namespace as an a11y one; it just hashes a different
descriptor.

## How the descriptor is computed

`signature.py` is a one-to-one Python port of `crates/tui-sig/src/lib.rs` (same
names, same logic, same ordering):

| `signature.py`           | `crates/tui-sig`          | what it does                                   |
| ------------------------ | ------------------------- | ---------------------------------------------- |
| `sig_of`                 | `sig_of`                  | FNV-1a 32-bit over UTF-8 bytes -> 8 hex        |
| `structural_class`       | `structural_class`        | one cell -> locale-invariant class             |
| `skeleton_of`            | `skeleton_of`             | run-length layout skeleton                     |
| `numeric_value_classes`  | `numeric_value_classes`   | bounded, sorted numeric value-class set        |
| `value_class`            | `value_class`             | the value-class bucketer                       |
| `is_strict_decimal`      | `is_strict_decimal`       | the strict period-decimal grammar              |
| `content_fingerprint`    | `content_fingerprint`     | Layer-1 effect token (ephemeral)               |
| `structural_sig`         | `structural_sig`          | THE canonical TUI state signature              |
| `labels_of`              | `labels_of`               | display-only word set (never the signature)    |

The skeleton keeps what is stable across locales and carries the layout
(box-drawing glyphs `U+2500..U+259F` -> `#`, digits -> `9`, ASCII
punctuation/symbols verbatim, spaces/newlines verbatim) and erases the localized
identity of words (any run of natural-language letters, any language including CJK,
-> `W`). Each maximal run is collapsed; volatile digit/space widths omit their
length while stable repeated runs keep it, so
the extents survive. The cursor cell `(row, col)` is appended because which
field/row is focused is structure, not text. The same screen rendered in English
and German produces the same skeleton and therefore the same signature.

### Char-predicate port (the risk, made explicit)

The one porting risk is reproducing Rust's `char` predicates exactly, because
Python's `str` helpers do not match them 1:1. The SDK reproduces them by code
point, in the same style the existing Python a11y ports use
(`runners/linux-atspi.py`, `sdk/reproit-linux/reproit_linux/signature.py`) and the
Go SDK uses (`sdk/reproit-tui-go/signature.go`):

- **`is_ascii_digit`**: `'0'..'9'` only (NOT Python `str.isdigit()`, which accepts
  superscripts and other-script digits).
- **`is_ascii_punctuation`**: the exact ASCII ranges
  `U+21..U+2F`, `U+3A..U+40`, `U+5B..U+60`, `U+7B..U+7E` (NOT `string.punctuation`
  string membership, to keep the boundary explicit).
- **box-drawing / block**: the literal range `U+2500..U+259F`, checked first.
- **`is_whitespace`**: the fixed Unicode `White_Space` set enumerated by code
  point (NOT Python `str.isspace()`, which also treats the FS/GS/RS/US controls and
  some separators as whitespace and would diverge from Rust).
- **`is_alphanumeric`** (Rust `is_alphabetic || is_numeric`): Unicode category
  `L*`, plus numeric categories `Nd`/`Nl`/`No`, plus an `str.isalpha()` fallback
  for the `Other_Alphabetic` code points. For every glyph that actually reaches
  this branch on a rendered screen the result is exact: ASCII digits are handled
  before this is called, and any word glyph collapses to `W` either way, so the
  only distinction that matters (letter-vs-symbol) is one both stdlibs agree on.

This is parity-gated against all 19 golden vectors (see "Verification" below), so
any drift in a predicate is caught immediately.

## Textual / Rich integration

The SDK never touches the terminal. The app hands it the rendered frame each time
a screen settles, via a `ScreenContents` (the capture model in `capture.py`), and
the reporter signs it and records a coverage edge only when the structural
signature changes (exactly like the Go/Rust SDKs and the runner).

**Rich / Textual**, where the framework owns rendering, export the rendered frame
to text and pass it with the cursor cell:

```python
from rich.console import Console
from reproit_tui_py import Reporter, ScreenContents

reporter = Reporter(app_id="my-tui", endpoint="https://ingest.reproit.com")
reporter.install_crash_handler()
reporter.start_timer()  # periodic background flush (daemon timer)

# Rich can record the console output and export it as plain text. Render your
# frame to a recording console, then export the exact text the user sees:
console = Console(record=True)
console.print(my_renderable)           # draw the current screen
frame_text = console.export_text()     # the rendered frame as text

# (row, col) is the on-screen cursor cell; pass (0, 0) if you do not track one.
reporter.observe(ScreenContents.from_text(frame_text, cursor=(row, col)), action="key:Down")
```

For **Textual** specifically, capture the rendered screen text in your `App` (e.g.
after `on_mount` / on each screen transition) using a recording `Console`, or use
`Console.render_lines` to assemble the visible lines, then feed the joined text to
`ScreenContents.from_text`. Call `observe(...)` whenever a screen settles (after a
key, a navigation, or a refresh); the SDK de-dupes by structural signature so
repeat frames of the same screen cost nothing.

**urwid / prompt_toolkit** and hand-rolled dashboards usually hold a cell grid or a
list of row strings. Build the screen with `from_rows`:

```python
from reproit_tui_py import ScreenContents, Cell

# list of row strings (one character per cell):
screen = ScreenContents.from_rows(["┌────┐", "│ Hi │", "└────┘"], cursor=(1, 2))

# or a row-major Cell grid (mark wide CJK/emoji cells so the vt100 spacer column
# is skipped, exactly as the runner's parser does):
grid = [[Cell("欢", wide=True), Cell(""), Cell("x")]]
screen = ScreenContents(grid=grid, cursor=(0, 0))

reporter.observe(screen, action="auto")
```

`ScreenContents.text()` reproduces vt100 `screen().contents()` byte-for-byte (gap
spaces before later non-empty cells, wide-cell spacer-column skip, per-row trailing
trim, trailing-blank-row trim), so the signature the embedded app reports equals
the one the runner computes from the same screen.

## Crash reporting

`reporter.install_crash_handler()` installs:

- `sys.excepthook` for uncaught Python exceptions: it records an `error` event
  carrying the current signature and the graph path that led to it, flushes, then
  chains to the prior excepthook so the app's own logging and the real traceback
  still run;
- `SIGSEGV` / `SIGABRT` / `SIGBUS` / `SIGFPE` for native crashes: records a signal
  error, flushes, then restores the default disposition and re-raises so the crash
  is not swallowed (core dumps and the real exit code are preserved).

You can also call `reporter.record_error(exc)` from your own `except` block.

## App invariants

Declare a predicate the app must satisfy in EVERY state the fuzzer reaches (a
running total never negative, exactly one pane focused). Under the fuzzer reproit
evaluates it on each observed screen and reports the failures as `invariant`
findings; in production the registry is inert.

```python
reporter.invariant("cart-total-nonneg", lambda: cart.total >= 0)

# Raise (or return {"ok": False, "message": ...}) to supply the failure message.
def one_pane_focused():
    n = sum(1 for p in panes if p.focused)
    if n != 1:
        raise ValueError(f"{n} panes focused")
    return True

reporter.invariant("one-pane-focused", one_pane_focused)
```

The predicate returns truthy when it holds; returning falsy, raising, or
returning `{"ok": False, "message": ...}` marks it violated (the raised text or
the message becomes the finding message). Registration is idempotent by id. Under
the fuzzer the SDK writes each violation to the marker file the `reproit __tui`
runner provisions (`REPROIT_INVARIANT_FILE`); stderr is not used because a PTY
conflates it with the rendered frames.

## Event contract

The reporter emits the SAME wire contract every reproit SDK uses, POSTed to
`<endpoint>/v1/events` as one batch via stdlib `urllib` (best-effort, dropped on
failure, with optional `Authorization: Bearer <api_key>`):

```json
{ "appId": "my-tui", "sentAt": 1730000000000, "ctx": { "platform": "linux", "..." : "..." },
  "events": [
    { "kind": "edge",  "from": "<sig>", "action": "key:Down", "to": "<sig>", "labels": ["..."], "t": 0 },
    { "kind": "error", "sig": "<sig>", "path": [{"sig": "<sig>", "action": "..."}], "message": "...", "stack": ["..."], "t": 0 }
  ] }
```

With `redact_labels=True`, the human label set is dropped from edges and only the
locale-invariant signatures leave the process. With no `endpoint`, batches go to
the `on_event` hook (or stderr if there is none), which is the dev/testing path.

## Verification

The parity gate loads `../../tui_signature_vectors.json` and asserts, for all 19
golden vectors, `structural_sig == expected_sig` and `content_fingerprint ==
expected_fp`; it also checks the cross-vector relationships (locale invariance,
POS1 collapse, cursor-as-structure, value-only effect), the value-class buckets,
and the capture-to-text mapping with synthetic screens.

```
python3 sdk/reproit-tui-py/tests/test_parity.py
```

Expected output:

```
PASS: golden vectors 19/19 (sig + fp)
PASS: cross-vector relationships
PASS: value-class buckets + bounded numeric extraction
PASS: capture-to-text mapping (synthetic screens)

All 19 golden TUI vectors pass ...
```

The live Textual/Rich capture against a running app is not exercised in the
headless test (it needs a real TTY); the capture-to-text mapping that feeds the
signature is unit-tested with synthetic screens instead, and the signature/
fingerprint core is parity-gated against the golden vectors.

No em dashes anywhere, per project rules.
