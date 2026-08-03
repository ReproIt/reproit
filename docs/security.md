# Security and data handling

For anyone deciding whether to put ReproIt in production. The short answer: the SDK
captures the **structure of what happened** (which screens, which controls, in what order) so a
crash can be reproduced, and it is designed so that **user input values and personal data never
leave your app**.

If you only read one section, read the next one.

## What leaves your app, and what never does

**Sent** to whatever ingest endpoint you configure:

- **Structural signatures**: a short hash (FNV-1a, 8 hex chars) of each screen's _shape_ (roles,
  developer keys, nesting), with all visible text stripped out before hashing. The same screen in
  any language produces the same hash. See [signature.md](signature.md).
- **The action sequence**: which controls were operated, addressed by their stable selector (a
  developer key or a structural index), and the transitions between screens.
- **Control labels** (the visible UI text of controls, e.g. a button reading "Submit"), by default,
  to make the graph readable. You can turn this off (`redactLabels: true`) so only hashes leave,
  never any UI text.
- **A typed finding identity.** Each finding carries `identity.oracle`, its structural invariant,
  boundary, trigger, and minimized action path. A genuine uncaught error, native crash, or fatal
  signal uses the `crash` oracle.

All SDKs send one bounded, versioned protocol. Every frame has a run id, monotonically increasing
sequence, explicit evidence scope, and one typed event. Invalid or oversized evidence is represented
as a `stream-defect` reason code so evaluation can abstain instead of treating missing evidence as a
clean result.

**Never sent:**

- **The values users type.** Text-field contents are not transmitted. On an error, the SDK attaches
  _derived features_ of a field (length, charset, "has emoji", and so on, listed below), never the
  value itself.
- **Password and hidden fields.** These are never read at all, not even to fingerprint them.

Confirmed causal replay can additionally retain already-redacted JSON request and response
structure. Credential/identity keys and secret headers are replaced before persistence, non-JSON
bodies retain length only, and the complete capsule is AES-256-GCM encrypted at rest. See
[repros.md](repros.md). Referenced findings and kept repros pin their encrypted capsule. Only
abandoned candidate capsules are automatically bounded by age and count.

The capsule key defaults to a random per-machine file (`.reproit/capsule.key`, mode 0600,
gitignored) and rotates every 90 days. To share a capsule across a team or with CI, set
`REPROIT_CAPSULE_KEY` to a 64-character hexadecimal key: reproit reads it, never writes it, and
stops rotating while it is set. Anyone holding that key can read every capsule it protects, so
distribute it like any other shared secret.

So a crash report can say "a 312-character name with mixed Arabic and Latin script broke the
checkout screen" without anyone ever seeing the name.

## The PII-safe input fingerprint

When an error fires, the SDK records _features_ of the on-screen field values so a replay fixture
can be synthesized that triggers the same bug (a long value, an emoji, an empty field, a
right-to-left string) without storing the value. The exact features (schema version 2, identical
across all SDKs and unit-tested in each):

| Feature                     | What it is                                                        |
| --------------------------- | ----------------------------------------------------------------- |
| `len`                       | Unicode code-point count                                          |
| `bytes`                     | UTF-8 byte length (catches DB byte-limit overflows)               |
| `charset`                   | `numeric` / `ascii` / `unicode`                                   |
| `scripts`                   | sorted Unicode script buckets present (e.g. `["Arabic","Latin"]`) |
| `hasEmoji`                  | contains an emoji / pictographic code point                       |
| `isEmpty`                   | empty or whitespace-only                                          |
| `isRtl`                     | contains a right-to-left character                                |
| `hasCombiningMarks`         | combining accents (a normalization/layout breaker)                |
| `hasZeroWidth`              | zero-width / invisible code points                                |
| `hasNewline`                | contains a newline                                                |
| `leadingTrailingWhitespace` | has edge whitespace                                               |

That is the whole schema. There is no field for the value, and the function that computes it is pure
(it can only read these features). A 16-digit card number and a 16-digit phone number produce the
identical fingerprint (`len:16, charset:numeric`), which is the point: enough to reproduce the bug,
not enough to identify a person.

## Your controls

- **`redactLabels: true`** in `ReproIt.init({...})`: only structural hashes leave the app, no
  visible text of any kind.
- **The endpoint is yours.** `ReproIt.init({ endpoint })` points at wherever you run ingest; nothing
  is hardcoded to a reproit-operated server.
- The SDK is small and source-available; the capture path is auditable (see `sdk/reproit-web.js` for
  the reference implementation, mirrored across platforms).

## Adding the SDK

Web (and Electron / Tauri, which use the same SDK) is one line plus init:

```html
<script src="reproit-web.js"></script>
<script>
ReproIt.init({ appId: "myapp", endpoint: "https://your-ingest/v1/events" });
// add redactLabels: true to send only hashes
</script>
```

The other platforms (iOS, Android, React Native, Flutter, native desktop, TUI) ship an SDK under
`sdk/` with the same init shape and the identical fingerprint schema. See each SDK's README for the
per-platform install.

## Offline support bundles (`.rpb`)

The `.rpb` format is a bounded evidence carrier for a failure you cannot send any other way. It is not an executable package.

## Guarantees

- Every artifact is identified by the SHA-256 digest of its exact bytes.
- The encrypted payload uses XChaCha20-Poly1305 with a fresh 24-byte nonce.
- The manifest includes the ciphertext digest and is signed with Ed25519.
- Inspection verifies the manifest, payload digest, and signature without decrypting artifacts.
- Import authenticates and decrypts before extracting.
- Archive entries must have exactly the form `artifacts/<64 hexadecimal characters>`.
- Collection accepts at most 128 regular files and 64 MiB of plaintext.
- The header is limited to 1 MiB and the complete imported file is bounded.
- Output files are created without overwrite.
- Generated encryption keys are written separately with mode 0600 on Unix.
- Import and inspection require a signer key supplied independently from the bundle.

## Authenticity boundary

When `REPROIT_BUNDLE_SIGNING_KEY` is configured, the signature authenticates that collector to an
importer that holds its public key. Without a configured key, collection generates an ephemeral
signing key and writes `<bundle>.signer`. Transfer this public key through a different authorized
channel. Import and inspection compare the manifest signer to
`REPROIT_BUNDLE_TRUSTED_SIGNER`, the public key derived from
`REPROIT_BUNDLE_SIGNING_KEY`, or that separate signer file. An embedded key alone is never trusted.

The encryption key comes from `REPROIT_BUNDLE_ENCRYPTION_KEY` as 64 hexadecimal characters. When
the variable is absent, collection generates a random key and writes `<bundle>.key`. Send the key
through a different authorized channel. Possession of the bundle and key permits reading every
included artifact.

## Policy boundary

Encryption does not make evidence exportable. Default collection marks artifacts
`local-analysis-only` and `unredacted-restricted`. `--exportable` is the operator's assertion that
redaction already happened at source and that policy permits export. Reproit does not infer consent
from a filename, file type, encryption setting, or destination.

Imported strings, paths from the source environment, logs, dump contents, and manifest labels are
untrusted data. They never become argv, environment values, working directories, cleanup actions,
or provider definitions. Execution is allowed only after an explicit plan binding resolves to a
checkout-owned provider with an exact matching digest.

## Current limits

The first format encrypts the archive in memory and therefore uses a 64 MiB plaintext limit. Large
dumps need a streaming encryption and resumable transfer format. The format does not wrap a
symmetric key to an asymmetric recipient key or manage organization key rotation.
