# ReproIt iOS

Production telemetry for native iOS apps (UIKit and SwiftUI), shipped as a Swift
Package. It emits the **same** state-graph and error events from real users that
the reproit test runners emit, so the production usage graph aligns 1:1 with
test-time graphs. When a user hits an error, the event carries the graph path
that produced it, which the reproit cloud turns into a deterministic replay: a
prod "cannot reproduce" becomes a reproducible test.

It mirrors the web SDK (`sdk/reproit-web.js`) and `reproit_flutter`: same FNV-1a
state signature, same event shapes, same `{appId, sentAt, events}` batch POST to
`<endpoint>/v1/events`, so web, Flutter, and iOS telemetry land in one cloud
graph.

## Install

Swift Package Manager. In Xcode: *File > Add Package Dependencies* and point at
this directory (or its git URL), then add the `ReproIt` library to your app
target. Or in a `Package.swift`:

```swift
.package(path: "../sdk/reproit-ios"),
// ...
.target(name: "MyApp", dependencies: [.product(name: "ReproIt", package: "ReproIt")])
```

## Usage (one call)

Call `ReproIt.start` once at launch.

UIKit (`AppDelegate`):

```swift
import ReproIt

func application(_ app: UIApplication,
                didFinishLaunchingWithOptions opts: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
    ReproIt.start(ReproItConfig(
        appId: "example",
        endpoint: "https://ingest.reproit.example",
        apiKey: "sk_...",
        sampleRate: 1.0,     // fraction of sessions to record
        redactLabels: false  // true = send signatures only, no label text
    ))
    return true
}
```

SwiftUI (`App`):

```swift
@main
struct MyApp: App {
    init() {
        ReproIt.start(ReproItConfig(appId: "example",
                                    endpoint: "https://ingest.reproit.example",
                                    apiKey: "sk_..."))
    }
    var body: some Scene { WindowGroup { ContentView() } }
}
```

If `endpoint` is nil, events go to the `onEvent` callback (or, if that is also
nil, a `print` debug line) instead of the network, which is handy for local
inspection:

```swift
ReproIt.start(ReproItConfig(appId: "example", onEvent: { print($0) }))
```

## What is captured

- **State signatures.** The SDK walks the live view hierarchy
  (`UIApplication.shared.connectedScenes` -> key window -> recursive
  `subviews`), reading each visible element's accessible name. The **state
  signature** is FNV-1a over the sorted, `|`-joined visible accessible names,
  byte-identical to the runners (`runners/macos-ax.swift`), `sdk/reproit-web.js`,
  and `templates/explorer.dart`.
- **Accessible name** of an element: `accessibilityLabel`, falling back to the
  element's own title/text (`UIButton` title, `UILabel`/`UITextField` text,
  search-bar text/placeholder), then `accessibilityValue`. The raw name is
  trimmed, reduced to its first line, skipped if empty or longer than
  `maxLabelLen` (40), deduped, and capped at `maxLabels` (24).
- **Visibility** mirrors the runner: not hidden, alpha > 0, non-zero bounds. A
  hidden subtree is skipped entirely.
- **Edges.** Snapshots are debounced (350 ms by default); when the signature
  changes, an edge is recorded. The first snapshot is `load`; an unattributed
  change is `auto`.
- **Taps.** A `UITapGestureRecognizer` is added to the key window with
  `cancelsTouchesInView = false` and a delegate that allows simultaneous
  recognition, so it never swallows the app's own gestures. On tap, the hierarchy
  is hit-tested for the deepest tappable, named view under the touch point, and
  the next edge is labeled `tap:<label>`.
- **Tappable** = a `UIControl`, a view carrying the `.button`/`.link`
  accessibility trait, or a view with a tap gesture recognizer. A tappable view
  with no usable name increments an `unlabeled` coverage counter.
- **Errors.** An `NSSetUncaughtExceptionHandler` records an error event with the
  current signature and the full repro path before the process dies, then chains
  to any previously installed handler (e.g. Crashlytics). The crash-path flush is
  synchronous (best-effort, bounded to ~2 s).
- **Context (the "which users" answer).** Each batch carries a PII-safe `ctx`
  map of cohort dimensions. The cloud uses it to compute a *discriminator*: when
  an error cohort over-represents some dimension vs the baseline (e.g.
  `locale=tr`), it surfaces that as the thing that distinguishes "happens to some
  users" from "happens to all". At `start` the SDK seeds **tier-1 auto
  dimensions** (zero-PII, Foundation-only, host-testable): `platform` (always
  `"ios"`), `os` (clean `major.minor`), `locale` (`Locale.current.identifier`),
  and `tz` (`TimeZone.current.identifier`). The map is included in the batch only
  when non-empty, matching the web/Flutter SDKs.

## Identify & custom context

Mirrors `reproit_flutter`. All calls are no-ops if telemetry was sampled out.

```swift
// Attach a hashed user id so the cloud can group "these N users hit it" without
// storing identity. The raw id is hashed to a 16-char `uid` and never sent.
ReproIt.identify("user-123", context: ["plan": "pro"])

// Set PII-safe dimensions (roles, plans, count buckets, never raw user data).
ReproIt.setContext("role", "admin")
ReproIt.setContexts(["tenant": "acme", "seats": 12])

// Read-only view of what ships with each batch.
let ctx = ReproIt.context // ["platform": "ios", "os": "...", "locale": ..., ...]
```

The `uid` is `SHA-256(userId)` truncated to 16 hex chars when **CryptoKit** is
available (`#if canImport(CryptoKit)`), byte-identical to the Flutter SDK. On the
rare platform without CryptoKit, a documented Foundation FNV-1a-64 fallback is
used instead (deterministic and non-reversible, but a different value than the
CryptoKit path). Either way the raw id is never transmitted or stored.

## Event shapes

Edge (state transition):

```json
{ "kind": "edge", "from": "<sig>", "action": "tap:Open Settings",
  "to": "<sig>", "labels": ["..."], "t": 1717939200123 }
```

Error (with replay path + PII-safe input fingerprint):

```json
{ "kind": "error", "sig": "<sig>",
  "path": [{ "sig": "s1", "action": "tap:X" }, { "sig": "s2", "action": "back" }],
  "message": "...", "stack": ["..."], "source": null, "line": null,
  "context": { "fingerprint": [
    { "field": "Email", "len": 18, "charset": "ascii",
      "hasEmoji": false, "isEmpty": false, "isRtl": false }
  ] },
  "t": 1717939200123 }
```

Batched as `{ "appId", "sentAt", "ctx"?, "events": [...] }` and POSTed to
`<endpoint>/v1/events` with `Content-Type: application/json` and, when `apiKey`
is set, `Authorization: Bearer <apiKey>`. `ctx` is the PII-safe context map and
is present only when non-empty. These match `crates/cloud/src/ingest.rs`, which
folds edges into the production graph, attaches `ctx` to each event for cohort
discrimination, and stores errors with their path for repro
(`GET /v1/errors/:app/:idx/repro`).

## Configuration

Field names and defaults mirror the web and Flutter SDKs:

| Field | Default | Meaning |
|-------|---------|---------|
| `appId` | (required) | App id in every batch |
| `endpoint` | `nil` | `POST <endpoint>/v1/events`; nil => `onEvent`/debug only |
| `apiKey` | `nil` | `Authorization: Bearer <apiKey>` when set |
| `onEvent` | `nil` | Per-event dev hook / custom transport |
| `sampleRate` | `1.0` | Fraction of sessions that report (decided once) |
| `maxLabels` | `24` | Labels per state |
| `maxLabelLen` | `40` | Labels longer than this are ignored |
| `pathCap` | `60` | Repro-path trail length |
| `flushInterval` | `5.0` s | Batch flush interval |
| `redactLabels` | `false` | true => send signatures/actions only, no label text |
| `debounce` | `0.350` s | Settle window before snapshotting |

## Privacy

Signatures are **structural** (a hash of which controls exist), not user data.
Set `redactLabels: true` to send only signatures and actions (no human-readable
label text). Use `sampleRate` to record a fraction of sessions.

### Input fingerprinting (PII-safe, features not values)

On an error, for each on-screen text field the SDK derives PII-safe FEATURES of
the field's value and attaches them under `context.fingerprint`. It captures
FEATURES, never the raw value: `{ field, len, charset, hasEmoji, isEmpty,
isRtl }`, where `len` is the Unicode scalar count, `charset` is `numeric` |
`ascii` | `unicode`, and the flags mark emoji / empty / RTL. The cloud uses
these to build a property-matched replay fixture (a 312-char name, an emoji, a
Turkish dotless "i", an empty or RTL field) WITHOUT storing PII. The pure,
Foundation-only `ReproItFingerprint.fingerprintValue(_:)` is host-unit-tested.

Field values are read from `UITextField`/`UITextView` on the key window, then
fingerprinted and discarded; raw text never leaves the device. Honest
limitation: `isSecureTextEntry` (password) fields are **skipped entirely** and
never read. Fields with no text report `isEmpty: true`. The field label is the
field's `accessibilityLabel` or `placeholder`, or a positional index, never
derived from the value.

## Honest limitations

- **Threading / main-thread coupling.** Snapshotting and tap hit-testing read
  live UIKit objects and therefore run on the main thread; the engine itself
  (buffering, signature, network) is thread-safe and queue-agnostic. Debounced
  snapshots are scheduled on the main run loop.
- **Crash-path delivery is best-effort, not guaranteed.** The uncaught-exception
  handler catches Obj-C/Swift `NSException`s and does a bounded synchronous
  flush. It does **not** catch fatal signals (`SIGSEGV`, `SIGABRT` from
  `fatalError`/`precondition`, watchdog kills, OOM). A signal handler is
  intentionally **not** installed by default: running non-async-signal-safe code
  (URLSession, JSON serialization) inside a signal handler is undefined
  behaviour, and the right way to capture signal crashes is to pair this SDK with
  a dedicated crash reporter and replay the last buffered path on next launch.
  For guaranteed delivery, persist the buffer and resend on relaunch (not yet
  implemented).
- **Accessibility surface only.** Like a screen reader and like the runner, the
  SDK sees what the accessibility tree exposes. SwiftUI bridges to UIKit
  accessibility, so SwiftUI views are covered, but custom-drawn content with no
  `accessibilityLabel` is invisible by design. Set proper a11y labels to improve
  both screen-reader support and graph fidelity.
- **No navigation-name labeling yet.** Route changes are captured structurally
  (the signature changes), but there is no `UINavigationController` delegate hook
  to label edges `nav:<title>` the way the Flutter SDK's `navigatorObserver`
  does. Such transitions surface as `auto`.
- **Single key window.** The tap recognizer attaches to the current key window
  and re-binds on window/orientation changes; multi-window iPad scenes capture
  whichever window is key.
- **Sampling and the debounce timer** mean very fast intermediate screens
  (visible < `debounce`) may be skipped, by design, to avoid noisy half-rendered
  states. This matches the web/Flutter settle behaviour.

## Build & test

The package is split so the canonical contract (signature + payload encoding +
engine) is **pure Foundation** and the UIKit capture lives in `Capture.swift`
behind `#if canImport(UIKit)`. That lets the parity test run on a macOS host:

```sh
swift build   # builds on macOS host (Capture.swift compiles to nothing there)
swift test    # runs the parity + engine tests on the host
```

The tests assert the canonical signatures
(`["Home Screen","Open Settings","Open Profile","Trigger Crash"] => 951259c1`,
`["Settings","Back"] => 054d1bbf`, `[] => 811c9dc5`) plus name normalization,
snapshot rules, event/batch encoding, and the engine's load/tap edge logic. The
UIKit layer is type-checked against the iOS SDK; full device capture requires an
iOS target/simulator.

## Layout

```
Package.swift
Sources/ReproIt/
  Core.swift      # Foundation-only: config, FNV-1a signature, name rule,
                  # snapshot model, event/batch encoding
  Engine.swift    # Foundation-only: state machine, buffer, flush, URLSession
  ReproIt.swift   # public facade: start/flush/reset + sampling
  Capture.swift   # UIKit-only (#if canImport(UIKit)): hierarchy walk, taps,
                  # error hook
Tests/ReproItTests/ReproItTests.swift  # host-runnable parity + engine tests
```
