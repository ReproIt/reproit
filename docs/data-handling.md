# Data handling and privacy (SDK)

This is for anyone deciding whether to put the reproit SDK in production. The
short answer: the SDK captures the **structure of what happened** (which screens,
which controls, in what order) so a crash can be reproduced, and it is designed
so that **user input values and personal data never leave your app**.

If you only read one section, read the next one.

## What leaves your app, and what never does

**Sent** to whatever ingest endpoint you configure:

- **Structural signatures**: a short hash (FNV-1a, 8 hex chars) of each screen's
  *shape* (roles, developer keys, nesting), with all visible text stripped out
  before hashing. The same screen in any language produces the same hash. See
  [signature.md](signature.md).
- **The action sequence**: which controls were operated, addressed by their
  stable selector (a developer key or a structural index), and the transitions
  between screens.
- **Control labels** (the visible UI text of controls, e.g. a button reading
  "Submit"), by default, to make the graph readable. You can turn this off
  (`redactLabels: true`) so only hashes leave, never any UI text.
- **An oracle tag on findings.** Each error event carries an `oracle` field: a
  genuine uncaught error / native crash / fatal signal is tagged `oracle:
  "crash"` (the crash oracle firing). The cloud gates ingest on this tag so only
  oracle-grade findings ship, identical across every SDK.

**Never sent:**

- **The values users type.** Text-field contents are not transmitted. On an
  error, the SDK attaches *derived features* of a field (length, charset, "has
  emoji", and so on, listed below), never the value itself.
- **Password and hidden fields.** These are never read at all, not even to
  fingerprint them.

Confirmed causal replay can additionally retain already-redacted JSON request
and response structure. Credential/identity keys and secret headers are replaced
before persistence, non-JSON bodies retain length only, and the complete capsule
is AES-256-GCM encrypted locally. See [causal-capsules.md](causal-capsules.md).
Referenced findings and kept repros pin their encrypted capsule. Only abandoned
candidate capsules are automatically bounded by age and count.

So a crash report can say "a 312-character name with mixed Arabic and Latin
script broke the checkout screen" without anyone ever seeing the name.

## The PII-safe input fingerprint

When an error fires, the SDK records *features* of the on-screen field values so a
replay fixture can be synthesized that triggers the same bug (a long value, an
emoji, an empty field, a right-to-left string) without storing the value. The
exact features (schema version 2, identical across all SDKs and unit-tested in
each):

| Feature | What it is |
|---|---|
| `len` | Unicode code-point count |
| `bytes` | UTF-8 byte length (catches DB byte-limit overflows) |
| `charset` | `numeric` / `ascii` / `unicode` |
| `scripts` | sorted Unicode script buckets present (e.g. `["Arabic","Latin"]`) |
| `hasEmoji` | contains an emoji / pictographic code point |
| `isEmpty` | empty or whitespace-only |
| `isRtl` | contains a right-to-left character |
| `hasCombiningMarks` | combining accents (a normalization/layout breaker) |
| `hasZeroWidth` | zero-width / invisible code points |
| `hasNewline` | contains a newline |
| `leadingTrailingWhitespace` | has edge whitespace |

That is the whole schema. There is no field for the value, and the function that
computes it is pure (it can only read these features). A 16-digit card number and
a 16-digit phone number produce the identical fingerprint
(`len:16, charset:numeric`), which is the point: enough to reproduce the bug, not
enough to identify a person.

## Your controls

- **`redactLabels: true`** in `ReproIt.init({...})`: only structural hashes leave
  the app, no visible text of any kind.
- **The endpoint is yours.** `ReproIt.init({ endpoint })` points at wherever you
  run ingest; nothing is hardcoded to a reproit-operated server.
- The SDK is small and source-available; the capture path is auditable (see
  `sdk/reproit-web.js` for the reference implementation, mirrored across
  platforms).

## Adding the SDK

Web (and Electron / Tauri, which use the same SDK) is one line plus init:

```html
<script src="reproit-web.js"></script>
<script>
  ReproIt.init({ appId: "myapp", endpoint: "https://your-ingest/v1/events" });
  // add redactLabels: true to send only hashes
</script>
```

The other platforms (iOS, Android, React Native, Flutter, native desktop, TUI)
ship an SDK under `sdk/` with the same init shape and the identical fingerprint
schema. See each SDK's README for the per-platform install.
