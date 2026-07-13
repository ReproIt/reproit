# reproit-tui (TypeScript SDK)

The production SDK a **JS/TS terminal-UI (TUI)** application embeds to report
sessions, coverage edges and crash signatures to the reproit cloud, and to compute
the SAME canonical TUI screen signature the fuzz runner computes. A production crash
reported by this SDK carries the exact state signature the
[`reproit __tui`](../../crates/reproit/src/backends/tui.rs) runner can replay locally.

Designed for [**Ink**](https://github.com/vadimdemedes/ink) (React for terminals)
and any other JS/TS TUI framework (blessed, neo-blessed, terminal-kit, or a
hand-rolled raw-mode dashboard).

## The signature namespace (read this)

TUI signatures are a **separate namespace** from the accessibility-tree (a11y)
signatures. A terminal has no a11y role tree, so the descriptor SOURCE is the
rendered terminal screen (the VT cell grid), normalized to a locale-invariant layout
skeleton, **not** a `Node` role tree. Per
[`docs/signature.md`](../../docs/signature.md) ("Terminal and instrumented surfaces"),
**TUI signatures are NOT expected to match the a11y golden vectors in
`signature_vectors.json`**. The parity target for this SDK is the canonical Rust
crate [`crates/tui-sig/src/lib.rs`](../../crates/tui-sig/src/lib.rs) (the runner
shares that crate directly), pinned by the repo-root golden vectors
[`tui_signature_vectors.json`](../../tui_signature_vectors.json).

What **is** shared with every other surface: the **hash family**, FNV-1a 32-bit
(offset basis `0x811c9dc5`, prime `0x01000193`, 8-char zero-padded lowercase hex),
and the `value_class` buckets (`EMPTY/ZERO/NEG/POS1/POS2/POS3/POSL`, `NONEMPTY`
fallback) with the strict period-decimal grammar. So a TUI signature is in the same
8-hex namespace as an a11y one; it just hashes a different descriptor.

## How the descriptor was ported

`signature.ts` is a one-to-one port of `crates/tui-sig/src/lib.rs`:

| lib.rs (Rust) | signature.ts | what it does |
|---|---|---|
| `sig_of` | `sigOf` | the FNV-1a 32-bit primitive (over **UTF-8 bytes**) |
| `structural_class` | `structuralClass` | per-cell locale-invariant class: box-drawing -> `#`, digit -> `9`, any word char -> `W`, space/newline/ASCII-punct kept verbatim |
| `skeleton_of` | `skeletonOf` | run-length classed cells; digit/space widths are omitted, other repeated runs append length |
| `numeric_value_classes` | `numericValueClasses` | bounded (cap 8), sorted multiset of the screen's numeric value-classes |
| `value_class` / `is_strict_decimal` | `valueClass` / `isStrictDecimal` | the oracle's buckets + strict grammar |
| `structural_sig` | `structuralSig` | `"{skeleton}\x1ecur={row},{col}\x1eV:{classes}"`, hashed |
| `content_fingerprint` | `contentFingerprint` | raw full-screen text + cursor, hashed (Layer-1 effect token, ephemeral) |
| `labels_of` | `labelsOf` | display-only word set (never the signature) |

### Char-predicate port and its risk

The Rust `char` predicates are reimplemented to match exactly (helpers at the bottom
of `signature.ts`):

- `is_ascii_digit` -> code range `0x30..=0x39` only.
- `is_ascii_punctuation` -> the four ASCII punctuation ranges, pinned by code point.
- `is_whitespace` -> the full Unicode White_Space set, **pinned by code point**, not
  the JS `\s` class (JS `\s` differs: it includes U+FEFF and excludes U+0085 NEL).
- `is_alphanumeric` (== `is_alphabetic || is_numeric`) -> the JS Unicode property
  escapes `\p{Alphabetic}` and `\p{Nd}|\p{Nl}|\p{No}`, which map to the same Unicode
  Alphabetic property and Numeric type (Nd|Nl|No) Rust uses.

**The one load-bearing port choice is the hash input encoding.** The Rust `sig_of`
hashes over the **UTF-8 bytes** of the input (`for b in s.bytes()`). The browser web
SDK (`sdk/reproit-web.js`) hashes over UTF-16 char codes, which is fine there because
its descriptor is pure ASCII, but a TUI screen carries multi-byte content
(box-drawing glyphs at U+2500.., CJK words). So this port hashes **UTF-8 bytes** via
`TextEncoder`, exactly like the Go SDK. The CJK (`cjk_word`) and grouped-number
(`grouped`, `neg_dec`) golden vectors exercise the multi-byte path, so this is pinned.

**Residual risk:** the only theoretical drift is `is_alphanumeric` / `is_whitespace`
on exotic Unicode code points whose Alphabetic/Numeric/White_Space classification
differs between the JS engine's Unicode tables and Rust's. Both collapse to `W` /
space in the skeleton, and the structurally load-bearing branches (digit and
box-drawing) are pinned by exact code-point ranges, so practical terminal content
cannot diverge. All 19 golden vectors, including the multi-byte ones, pass.

## The screen text model

The runner's `vt100::Parser` exposes `screen().contents()`. `ScreenContents`
reproduces that text model:

- **`ScreenContents.fromText(text, cursor)`** wraps an already-rendered contents
  string verbatim. This is the **Ink path** (see below).
- **`ScreenContents.fromRows(rows, cursor)`** renders a row-major cell grid to the
  SAME contents string vt100 produces, byte-for-byte: per row, gaps before a
  non-empty cell become spaces; a wide (CJK) cell emits its contents once and its
  spacer column is skipped; trailing empty cells emit nothing (per-row trailing
  whitespace trimmed); rows joined by `\n`; trailing blank rows trimmed. (These rules
  match the Go SDK's `ScreenContents.Text`, read from vt100-0.15 `grid.rs`/`row.rs`
  `write_contents`.)

`cursor` is `[row, col]`, 0-based, matching `screen().cursor_position()`.

## Ink integration

Ink renders its React component tree to a **string** each frame. You do not need any
Ink internal: capture the frame string and hand it to the reporter.

```tsx
import { Reporter, ScreenContents } from "reproit-tui";

const reporter = new Reporter({
  appId: "my-ink-cli",
  endpoint: process.env.REPROIT_ENDPOINT, // null/undefined -> events go to onEvent / dropped
});
reporter.installCrashHandler(); // flushes a crash event before the process dies

// After each render, hand the SDK the frame text + the action that produced it.
useEffect(() => {
  reporter.observe(ScreenContents.fromText(frame, [0, 7]), "render");
}, [state]);
```

See [`example/ink.tsx`](example/ink.tsx) for a full counter.

**One Ink integration covers the JS/TS TUI population.** Ink is React-for-terminals
and is the dominant JS TUI framework; the same `observe(fromText(...))` call works
for any framework that can produce a frame string (and `fromRows` covers cell-buffer
frameworks). Per the research, instrumenting Ink reaches the bulk of the JS/TS TUI
ecosystem with a single integration.

**Vendored/forked Ink (e.g. Claude Code).** The SDK does **not** hard-depend on any
Ink internal. `Reporter` and `ScreenContents` touch only the frame string you pass
and the global `process` object, so they work unchanged under a bundled, vendored, or
forked Ink. (`reproit-tui` lists no runtime dependency on `ink` at all; the Ink
example imports `ink` only for illustration.)

## Embed API

```ts
const r = new Reporter({
  appId: "myapp",
  endpoint: "https://ingest.reproit.com/v1/events", // null -> events go to onEvent / dropped
  ctx: { release: "1.2.3" },        // optional static context on every batch
  redactLabels: false,              // true -> drop the human label set from edges
  onEvent: (ev) => { /* dev sink */ },
});

r.installCrashHandler();            // uncaughtException/unhandledRejection/SIGINT/SIGTERM
r.observe(screen, "key:Down");      // record an edge iff the structural sig changed
r.observeText(frameStr, [0, 0], "render");  // Ink convenience
r.observeRows(cellGrid, [0, 0], "render");  // cell-buffer convenience
r.recordError(err);                 // emit an error event (current sig + graph path)
r.flush();                          // POST the batch now
```

- An **edge** event is recorded only when the structural signature changes (mirroring
  the runner's coverage edges, and the Go/Rust/web SDKs). A value-only change (same
  skeleton, a counter ticking) updates the ephemeral content fingerprint but does NOT
  open a new edge.
- **`recordError`** emits an `error` event carrying the current signature and the
  graph PATH that led to it (the seed of a deterministic repro), like the web SDK's
  error event. It does not exit.
- **`installCrashHandler`** wires `process.on('uncaughtException')` /
  `'unhandledRejection'` plus `SIGINT`/`SIGTERM`, flushing a `crash` event (current
  signature + path) before the process dies, then re-throwing / re-raising so the
  app's own crash semantics (exit code, default signal disposition) are preserved.
- The wire contract is identical to every other reproit SDK:
  `{appId, sentAt, ctx?, events}`, with `session` / `edge` / `error` / `crash`
  events.

## App invariants

Declare a predicate the app must satisfy in EVERY state the fuzzer reaches (a
running total never negative, exactly one pane focused). Under the fuzzer reproit
evaluates it on each observed screen and reports the failures as `invariant`
findings; in production the registry is inert.

```ts
r.invariant("cart-total-nonneg", () => cart.total >= 0);

// Throw (or return { ok: false, message }) to supply the failure message.
r.invariant("one-pane-focused", () => {
  const n = panes.filter((p) => p.focused).length;
  if (n !== 1) throw new Error(`${n} panes focused`);
  return true;
});
```

The predicate returns truthy when it holds; returning falsy, throwing, or
returning `{ ok: false, message }` marks it violated (the thrown text or the
message becomes the finding message). Registration is idempotent by id. Under the
fuzzer the SDK writes each violation to the marker file the `reproit __tui`
runner provisions (`REPROIT_INVARIANT_FILE`); stderr is not used because a PTY
conflates it with the rendered frames.

## Golden TUI vectors: how they were derived

`tui_signature_vectors.json` (repo root) holds 19 representative screens with their
expected `structural_sig` and `content_fingerprint`. **Every expected value was
produced by the real Rust crate, not a hand-port** (a temporary `#[ignore]`'d
`gen_tui_golden` test called the crate's own `structural_sig` / `content_fingerprint`
on each screen and printed the results; see the JSON's `_derivation` field).

## Parity test

Node v26 strips TypeScript types and runs `.ts` directly, and ships a built-in test
runner + assert, so **no `npm install` is required** to run the parity gate:

```
node --test sdk/reproit-tui-ts/test/parity.test.ts
```

or from this directory:

```
node --test test/parity.test.ts      # also: npm test
```

`test/parity.test.ts` asserts, for all **19** vectors, that the SDK's `structuralSig`
and `contentFingerprint` equal the crate-produced values, plus the cross-vector
relationships the spec promises (locale-invariance, value-class bucketing, the
value-sensitive fingerprint, cursor-as-structure), the canonical FNV-1a known values,
the `value_class` buckets, the cell-grid -> contents renderer against the vt100 model,
and the reporter's edge-only-on-change / batch-contract behavior.

Type-check (needs TypeScript, optional): `npx tsc --noEmit` or `npm run typecheck`.

## Structural authentication inputs

Terminal cells do not expose input semantics. Declare each auth field while it
is present; the declaration is invisible and becomes a no-op outside reproit:

```ts
import { authInput } from "reproit-tui";

authInput("phone", "account-phone");
// On the next screen:
authInput("otp", "verification-code");
```

Reproit uses these stable purposes and screen transitions to generate and verify
the login journey. Visible labels are never inspected, so translated and
non-Latin interfaces follow the identical path.
