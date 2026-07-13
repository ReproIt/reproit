# reproit-tui (Go SDK)

The production SDK a **terminal-UI (TUI)** application embeds to report sessions,
coverage edges and crash signatures to the reproit cloud, and to compute the SAME
canonical TUI screen signature the fuzz runner computes. A production crash reported
by this SDK carries the exact state signature the
[`reproit __tui`](../../crates/reproit/src/backends/tui.rs) runner can replay locally.

## Why Go

Go 1.26 is the best embed + test story here:

- The richest production TUI ecosystem (bubbletea, tview, tcell, gocui) is Go, so
  "drop this into a real TUI app" is a one-import reality, not a hypothetical.
- A single `go test ./...` runs the parity gate in this environment with no extra
  toolchain. (Rust could share code with `tui.rs` directly, but the embed target
  audience for TUIs is overwhelmingly Go, and the parity guarantee is enforced by the
  golden vectors regardless of language.)
- `runtime/debug` + `os/signal` give a clean panic + signal crash-flush path.

## The signature namespace (read this)

TUI signatures are a **separate namespace** from the accessibility-tree (a11y)
signatures. A terminal has no a11y role tree, so the descriptor SOURCE is the rendered
terminal screen (the VT cell grid), normalized to a locale-invariant layout skeleton,
not a `Node` role tree. Per [`docs/signature.md`](../../docs/signature.md) ("Terminal
and instrumented surfaces", around lines 332-341), **TUI signatures are NOT expected to
match the a11y golden vectors in `signature_vectors.json`**. The parity target for this
SDK is [`crates/reproit/src/backends/tui.rs`](../../crates/reproit/src/backends/tui.rs).

What *is* shared with every other surface: the **hash family**, FNV-1a 32-bit (offset
basis `0x811c9dc5`, prime `0x01000193`, 8-char zero-padded lowercase hex), and the
`value_class` buckets (`EMPTY/ZERO/NEG/POS1/POS2/POS3/POSL`, `NONEMPTY` fallback) with
the strict period-decimal grammar. So a TUI signature is in the same 8-hex namespace as
an a11y one; it just hashes a different descriptor.

## How the descriptor was ported

`signature.go` is a one-to-one port of `tui.rs`:

| tui.rs | signature.go | what it does |
|---|---|---|
| `sig_of` | `SigOf` | the FNV-1a 32-bit primitive (over UTF-8 bytes) |
| `structural_class` | `structuralClass` | per-cell locale-invariant class: box-drawing -> `#`, digit -> `9`, any word char -> `W`, space/newline/ASCII-punct kept verbatim |
| `skeleton_of` | `skeletonOf` | run-length classed cells; digit/space widths are omitted, other repeated runs append length |
| `numeric_value_classes` | `numericValueClasses` | bounded (cap 8), sorted multiset of the screen's numeric value-classes |
| `value_class` / `is_strict_decimal` | `valueClass` / `isStrictDecimal` | the oracle's buckets + strict grammar |
| `structural_sig` | `StructuralSig` | `"{skeleton}\x1ecur={row},{col}\x1eV:{classes}"`, hashed |
| `content_fingerprint` | `ContentFingerprint` | raw full-screen text + cursor, hashed (Layer-1 effect token, ephemeral) |
| `labels_of` | `LabelsOf` | display-only word set (never the signature) |

The Rust `char` predicates (`is_ascii_digit`, `is_ascii_punctuation`,
`is_alphanumeric`, `is_whitespace`) are reimplemented to match exactly (see the helpers
at the bottom of `signature.go`); the `is_whitespace` set is pinned by code point.

### The screen text model

The runner's `vt100::Parser` exposes `screen().contents()`. `ScreenContents.Text()`
reproduces that text byte-for-byte from a cell grid:

- per row, gaps before a non-empty cell become spaces; a wide (CJK) cell emits its
  contents once and its spacer column is skipped; trailing empty cells emit nothing
  (per-row trailing whitespace trimmed);
- rows joined by `\n`; trailing blank rows trimmed.

(These rules were read directly from `vt100-0.15` `grid.rs` / `row.rs`
`write_contents`.)

## Embed API

```go
r := reproittui.New(reproittui.Config{
    AppID:    "myapp",
    Endpoint: "https://ingest.reproit.com/v1/events", // "" -> events go to OnEvent / dropped
})
defer r.InstallCrashHandler()() // installs SIGINT/TERM/SEGV/ABRT handler; returned fn recovers panics
defer r.Flush()

// Each rendered frame: hand the SDK your cell grid (or a raw contents string).
r.Observe(reproittui.ScreenContents{Grid: grid, CursorRow: row, CursorCol: col}, "key:Down")
```

- **(a) cell grid**: fill `ScreenContents.Grid` from your framework's buffer
  (bubbletea view, tview/tcell cells). The SDK never touches the terminal.
- **(b) raw contents**: set `ScreenContents.Raw` (or call
  `ObserveContents(text, row, col, action)`) if you already hold the rendered text.

An **edge** event is recorded only when the structural signature changes (mirroring the
runner's coverage edges). On crash, a `crash` event carrying the current signature is
flushed before exit. The wire contract is identical to every other reproit SDK:
`{appId, sentAt, ctx?, events}`.

## App invariants

Declare a predicate the app must satisfy in EVERY state the fuzzer reaches (a
running total never negative, exactly one pane focused). Under the fuzzer reproit
evaluates it on each observed screen and reports the failures as `invariant`
findings; in production the registry is inert.

```go
r.Invariant("cart-total-nonneg", func() error {
    if cart.Total() < 0 {
        return fmt.Errorf("total is %d", cart.Total())
    }
    return nil
})
```

The predicate returns `nil` when it holds; a non-nil error (or a panic) marks it
violated, with that text as the finding message. Registration is idempotent by
id. Under the fuzzer the SDK writes each violation to the marker file the
`reproit __tui` runner provisions (`REPROIT_INVARIANT_FILE`); stderr is not used
because a PTY conflates it with the rendered frames.

## Golden TUI vectors: how they were derived

`tui_signature_vectors.json` (at the repo root, shared by the Rust/Go/TS/Python
TUI SDKs) holds 19 representative screens with their expected `structural_sig` and
`content_fingerprint`. **Every expected value was produced by the real runner code
(the shared `reproit-tui-sig` crate), not a hand-port.** Derivation:

1. A temporary `#[ignore]`'d test `gen_tui_golden` was added to the `tests` module in
   `crates/reproit/src/backends/tui.rs`. It calls the crate's own
   `structural_sig(contents, cursor)` and `content_fingerprint(contents, cursor)` on
   each screen and prints them.
2. Run: `cargo test -p reproit gen_tui_golden -- --nocapture --ignored`.
3. The output was transcribed into the JSON, and the temporary test removed (so
   `tui.rs` is unchanged from HEAD).

To regenerate, re-add that test and re-run.

## Parity test

```
go test ./...
```

`signature_test.go` asserts, for all 19 vectors, that the SDK's `StructuralSig` and
`ContentFingerprint` equal the `tui.rs`-produced values, plus the cross-vector
relationships the spec promises (locale-invariance, value-class bucketing, the
value-sensitive fingerprint, cursor-as-structure), the canonical FNV-1a known values,
the `value_class` buckets, and the cell-grid -> contents-string renderer against the
vt100 model.

## Divergence risk vs tui.rs

- **Hash + descriptor**: byte-exact, pinned by the golden vectors (FNV over UTF-8
  bytes; identical serialization including the `\x1e` separators and `V:` section).
- **`char` predicates**: the only theoretical drift is `is_alphanumeric` /
  `is_whitespace` on exotic Unicode. Both collapse to `W` / space respectively in the
  skeleton, and the digit and box-drawing branches (the structurally load-bearing ones)
  are pinned by exact ASCII / code-point ranges, so practical terminal content cannot
  diverge. The CJK and grouped-number golden vectors exercise the multi-byte path.
- **Cursor**: the SDK takes a 0-based `(row, col)` exactly as `tui.rs` reads from
  `screen().cursor_position()`. The app must pass the same convention.
