# reproit-android

Production telemetry for native Android (Kotlin) apps. Emits the **same**
state-graph and error events from real users that the reproit test runners emit,
so the production graph aligns 1:1 with test-time graphs. When a user hits an
error, the event carries the graph path that produced it, which the reproit cloud
turns into a deterministic replay: a prod "cannot reproduce" becomes a
reproducible test.

It mirrors the web SDK (`sdk/reproit-web.js`) and the Flutter / iOS /
React-Native SDKs: same canonical **structural** state signature, same event
shapes, same `/v1/events` batch endpoint, so all platforms land in one cloud
graph. The signature is the canonical contract in `docs/signature.md`, proven
byte-for-byte against `signature_vectors.json` (see `src/test/`).

## Quickstart

Until the Maven package is published, use the Android library directly from the
public repository:

```sh
git submodule add https://github.com/ReproIt/reproit vendor/reproit
```

```kotlin
// settings.gradle.kts
include(":reproit-android")
project(":reproit-android").projectDir = file("vendor/reproit/sdk/reproit-android")
```

```kotlin
// app/build.gradle.kts
dependencies {
    implementation(project(":reproit-android"))
}
```

```kotlin
// Application.onCreate
ReproIt.init(
    this,
    ReproItConfig(
        appId = "app_...",
        endpoint = "https://ingest.reproit.com",
        apiKey = "pk_live_...",
        buildVersion = "1.4.2",
        buildCommit = "abc123",
    ),
)
```

An explicit configuration runs in release builds. The no-argument
`ReproIt.start(this)` remains a debuggable-build convenience.

The reserved Maven coordinate is `com.reproit:reproit-android`. Do not use it
until it is published. The source checkout above is the supported 0.1 path.

## How it works

- `ReproIt.init(application, config)` registers an
  `Application.ActivityLifecycleCallbacks` to track the foreground Activity.
- Snapshots the **live view tree** of the current Activity's `window.decorView`,
  recursing `ViewGroup`s over visible views (`visibility == VISIBLE`,
  width/height > 0). When it reaches a **Jetpack Compose** host (an
  `AndroidComposeView`), it walks the Compose **semantics tree** (the same tree
  TalkBack and the Appium/UiAutomator2 runner read) instead of the opaque View
  leaf, so a Compose UI surfaces its real structure (roles, testTags, editable /
  value state) and produces the same signature the runner sees. The **state
  signature** is the canonical STRUCTURAL
  descriptor of the captured node tree (roles + ids + input types + icons + tree
  shape, prefixed by the screen anchor), hashed with FNV-1a 32-bit. Localized
  text never enters the hash, so an EN and a DE render of the same screen hash
  identically; byte-identical to the Rust oracle and the other SDKs. The
  `contentDescription ?? text` names are kept only as a display-only `labels`
  field, never folded into the hash.
- Snapshots are **debounced** (default 350 ms) via a
  `ViewTreeObserver.OnGlobalLayoutListener` + `OnScrollChangedListener` on the
  decor view, so the snapshot is taken once the UI settles.
- **Taps** are captured by a pass-through `decorView.setOnTouchListener` (returns
  `false`, never consuming the event). On `ACTION_DOWN` it hit-tests the view
  tree for the deepest clickable view under the point. `action` is a structural
  replay selector (`tap:key:<id>` or `tap:role:<role>#<idx>`); `label` is the
  optional human-readable display text.
- **Errors** are captured via `Thread.setDefaultUncaughtExceptionHandler`
  (chained to the previous handler). An error event carries the current signature
  and the full action path leading to it, and is flushed synchronously before the
  process dies.
- Batches events and POSTs `{appId, sentAt, ctx?, events}` to `<endpoint>/v1/events`
  with `Authorization: Bearer <apiKey>` (via `HttpURLConnection`, no extra dep).
- Attaches a PII-safe **context** map (`ctx`) to each batch (see below), which the
  cloud uses to answer "which users hit this?" without storing identity.

## Usage

```kotlin
import android.app.Application
import com.reproit.android.ReproIt
import com.reproit.android.ReproItConfig

class App : Application() {
    override fun onCreate() {
        super.onCreate()
        ReproIt.init(
            this,
            ReproItConfig(
                appId = "example",
                endpoint = "https://ingest.reproit.com",
                apiKey = "pk_live_...",
                buildVersion = "1.4.2",
                buildCommit = "abc123",
                sampleRate = 1.0,      // fraction of sessions to record
                redactLabels = false,  // true = send signatures only, no label text
            ),
        )
    }
}
```

Register `App` in your manifest: `<application android:name=".App" ...>`.

If `endpoint` is null, events go to the `onEvent` callback (or logcat) instead of
the network, which is handy for local inspection:

```kotlin
ReproIt.init(this, ReproItConfig(appId = "example", onEvent = { e -> Log.d("ev", e.toString()) }))
```

Flush manually before a known teardown with `ReproIt.flush()`.

## Config

Field names and defaults mirror the web SDK:

| field          | default | meaning |
|----------------|---------|---------|
| `appId`        | (req)   | app id in every batch |
| `endpoint`     | `null`  | `POST <endpoint>/v1/events`; null = onEvent/log only |
| `apiKey`       | `null`  | `Authorization: Bearer <apiKey>` when set |
| `onEvent`      | `null`  | dev hook / custom transport, called per event |
| `sampleRate`   | `1.0`   | fraction of sessions that report (decided once) |
| `maxLabels`    | `24`    | labels kept per state |
| `maxLabelLen`  | `40`    | labels longer than this are skipped |
| `pathCap`      | `60`    | max length of the repro trail |
| `flushMs`      | `5000`  | batch flush interval |
| `redactLabels` | `false` | true = signatures only, no label text |
| `debounceMs`   | `350`   | settle window before snapshotting |

## Event shapes

Edge (state transition):

```json
{ "kind": "edge", "from": "<sig>", "action": "tap:key:open-settings",
  "label": "Open Settings", "to": "<sig>", "labels": ["..."], "t": 1717939200123 }
```

Error (with replay path + PII-safe input fingerprint):

```json
{ "kind": "error", "oracle": "crash", "sig": "<sig>",
  "path": [{ "sig": "s1", "action": "tap:key:open-settings", "label": "Open Settings" },
           { "sig": "s2", "action": "nav" }],
  "message": "...", "stack": ["..."], "source": "File.kt", "line": 42,
  "context": { "fingerprint": [
    { "field": "Email", "len": 18, "charset": "ascii",
      "hasEmoji": false, "isEmpty": false, "isRtl": false }
  ] },
  "t": 1717939200123 }
```

Batch envelope: `{ "appId": "...", "sentAt": <ms>, "ctx": {...}?, "events": [...] }`
(`ctx` is omitted when empty). These match the cloud's `POST /v1/events`
contract, which folds edges into the production graph and stores errors
as bucket packages for repro (`GET /v1/apps/:app/buckets/:bucket`).

## Context: which users hit it (`ctx` / `identify`)

Errors that "can't be reproduced" are usually scoped to a cohort (a locale, an OS
version, a plan tier). The SDK attaches a small, **PII-safe** context map to every
batch as the `ctx` field; the cloud's ingest endpoint (`POST /v1/events`) folds it
into each event and computes a **cohort discriminator** (`GET /v1/errors/:app/cohorts`),
e.g. "this error is 6x over-represented in `locale=tr`".

**Tier-1 auto dimensions** are populated automatically at `init` (zero PII):

| key        | source                                  |
|------------|-----------------------------------------|
| `platform` | `"android"`                             |
| `os`       | `Build.VERSION.RELEASE` (e.g. `"14"`)   |
| `locale`   | `Locale.getDefault().toLanguageTag()`   |
| `tz`       | `TimeZone.getDefault().id`              |

**Custom dimensions**, add your own PII-safe dimensions (role, plan, a count
bucket). Do **not** put raw emails, names, or free-form user input here:

```kotlin
ReproIt.setContext("plan", "pro")
ReproIt.setContexts(mapOf("role" to "admin", "betaCohort" to true))
```

**identify**, group "these N users hit it" without storing identity. The raw
user id is hashed with SHA-256 (only a 16-char hex prefix is kept as `uid`); the
raw value is never stored or sent. Optional context is merged in the same call:

```kotlin
ReproIt.identify(user.id, mapOf("plan" to "pro"))
// ctx -> { ..., "uid": "a1b2c3d4e5f60718", "plan": "pro" }
```

These are the same `ctx` map, `identify`/`setContext`/`setContexts` semantics, and
tier-1 dimensions as the Flutter SDK, so all platforms produce comparable cohorts.

## App invariants

Declare a predicate the app must satisfy in EVERY state it reaches (a running
total never negative, the selected tab always highlighted). The fuzzer checks it
on each state it explores and reports the failures as `invariant` findings.

```kotlin
ReproIt.invariant("cart-total-nonneg") { cart.total >= 0 }

// Throwing supplies the failure message.
ReproIt.invariant("one-tab-selected") {
    val selected = tabs.count { it.isSelected }
    if (selected != 1) error("$selected tabs selected")
    true
}
```

The predicate returns `true` when it holds; returning `false` or throwing marks
it violated (a thrown exception's message becomes the finding message).
Registration is idempotent by id and INERT in production: the predicate is stored
but only evaluated when the SDK detects it is running under the reproit fuzzer.
UiAutomator2 runs the app in its own un-instrumented process (no app-env
channel), so the runner signals fuzz mode via the unprivileged
`debug.reproit.fuzz` system property (set over Appium's `mobile: shell` with
`setprop`; the `REPROIT_FUZZ=1` env var is also honored for local runs). Under
the fuzzer, a violated invariant is logged as a `REPROIT_INVARIANT` marker on
logcat (`android.util.Log`) that the mobile runner scrapes into the finding.

## Privacy

Set `redactLabels = true` to send only state signatures and actions (no
human-readable label text). Use `sampleRate` to record a fraction of sessions.
Signatures are **structural** (a hash of which controls exist), not user data.
`identify` stores only a SHA-256 hex prefix of the user id (`uid`), never the raw
value; the auto context dimensions (`platform`/`os`/`locale`/`tz`) carry no PII.
Anything you pass to `setContext`/`setContexts` is sent verbatim, so keep it
PII-safe (buckets/enums, not raw user input).

Shipping on Google Play? [DATA_SAFETY.md](DATA_SAFETY.md) maps exactly what this
SDK collects onto the Play Data safety form, category by category.

### Input fingerprinting (PII-safe, features not values)

On an error, for each on-screen text field the SDK derives PII-safe FEATURES of
the field's value and attaches them under `context.fingerprint`. It captures
FEATURES, never the raw value: `{ field, len, charset, hasEmoji, isEmpty,
isRtl }`, where `len` is the Unicode code-point count, `charset` is `numeric` |
`ascii` | `unicode`, and the flags mark emoji / empty / RTL. The cloud uses
these to build a property-matched replay fixture (a 312-char name, an emoji, a
Turkish dotless "i", an empty or RTL field) WITHOUT storing PII. The pure-Kotlin
`Fingerprint.fingerprintValue(String)` is host-unit-tested in `run_host_test.sh`.

Field values are read from `EditText` views on the foreground decor view, then
fingerprinted and discarded; raw text never leaves the device. Honest
limitation: password fields (an `inputType` with a PASSWORD variation) are
**skipped entirely** and never read. Compose `TextField`s are folded into the
**state-graph** signature via the Compose semantics walk (below), but the tier-3
on-error input fingerprint still reads only classic `EditText` views in v0;
fingerprinting Compose text fields is a follow-up. Empty fields report
`isEmpty: true`. The field label is `contentDescription` or `hint`, or a
positional index, never derived from the value.

## Architecture / testability

The heavy, deterministic logic lives in **pure-Kotlin** files with no `android.*`
imports, so it is host-testable without the Android SDK:

- `Signature.kt`, canonical STRUCTURAL signature (the `Node` tree, the
  `descriptor` serialization, FNV-1a) plus a dependency-free `Node` JSON reader
  for the parity gate. The Kotlin port of `crates/reproit/src/model/signature.rs`.
- `Engine.kt`, snapshot reduction (structural tree + anchor -> sig; localized
  names kept as display-only labels), edge/error
  state machine, the context map (`setContext`/`setContexts`/`identify` with the
  SHA-256 `uid` hash) + batch-envelope `ctx`, batching, payload building.
- `Json.kt`, minimal JSON encoder for the event payloads + a minimal JSON
  decoder used by the parity gate to read `signature_vectors.json` on the host.
- `Fingerprint.kt`, the PII-safe input `fingerprintValue` (features, not values).
- `Compose.kt`, the pure Jetpack Compose semantics-to-descriptor mapping: a
  framework-free `ComposeSemantics` holder + `roleOf` / `typeOf` / `valueOf` /
  `toNode`, mapping each Compose semantics node into the SAME `Signature.Node`
  model the View walk produces (NO `androidx.*` import, host-unit-tested in
  `ComposeMappingTest`).
- `Config.kt`, config data class.

`ReproIt.kt` is the thin Android binding (lifecycle, view-tree walk, taps,
errors, `HttpURLConnection`). `ComposeCapture.kt` is the Android-side bridge that
reads the Compose `SemanticsOwner` via the public `androidx.compose.ui.semantics`
APIs and builds `Compose.ComposeSemantics` holders for `Compose.toNode`; like
`ReproIt.kt` it imports `androidx.*` and is excluded from the host parity test.
The Compose dependency is `compileOnly`, so apps that do not use Compose pull in
nothing extra (a runtime class probe in `ReproIt` makes the Compose walk a no-op
when Compose is absent).

## Build & test

This is an Android library module (`build.gradle.kts` applies
`com.android.library`). A full build of the whole module (including `ReproIt.kt`)
requires the **Android SDK**.

The signature-parity + payload tests under `src/test/` import only the
pure-Kotlin core, so they run on a plain host JVM:

```sh
# from sdk/reproit-android/, requires the Android SDK + Gradle:
./gradlew test

# host-only (no Android SDK), using a standalone kotlinc + JUnit:
sh ./run_host_test.sh
```

`run_host_test.sh` compiles `Signature.kt`, `Json.kt`, `Config.kt`, `Engine.kt`,
`Fingerprint.kt` and the test, then runs it with JUnit on the host JVM.

## Honest limitations

- **View tree, not the accessibility tree.** Android's true a11y tree
  (`AccessibilityNodeInfo`) is only readable from an `AccessibilityService` or
  from a UIAutomator/Appium harness (which is what `runners/rn` drives over
  Appium). An in-process SDK cannot read it, so this SDK walks the **`View`
  tree**, mapping each view to a canonical role from its widget class /
  `AccessibilityNodeInfo` (never from text), with ids from the resource-entry
  name (or a `ReproIt.tagId` marker), input types from `EditText.inputType`, and
  optional icons from a `ReproIt.tagIcon` marker. For ordinary apps this yields
  the same canonical structure the runner sees.
  - **Jetpack Compose** is now **supported**: a `ComposeView` is one native
    `View`, so its internal composables are invisible to a plain `View`-tree
    walk, but the SDK detects the hosted `AndroidComposeView` and walks its
    Compose **semantics** tree (`SemanticsOwner`, the same tree TalkBack and the
    Appium/UiAutomator2 runner read), mapping each semantics node's role / text /
    contentDescription / testTag / editable-value into the SAME canonical
    `Node` model the View walk uses (`Compose.kt`). So a Compose screen produces
    the same structural signature the runner sees, byte-for-byte. The Compose
    dependency is `compileOnly` (no extra weight for non-Compose apps), and
    `testTagsAsResourceId` is not required for the SDK (it reads `testTag`
    directly from semantics). Plain Android Views (XML layouts, AppCompat) remain
    fully covered. Honest limits: only `password` is distinguished as a Compose
    field TYPE (email/number refinement is not reliably in semantics), icons are
    not read from Compose, and the merge boundary follows the UNMERGED semantics
    tree (a `mergeDescendants` Button surfaces its inner text node as a child).
  - **WebView** content (its DOM is opaque to the View tree).
  - Custom-drawn canvases with no child Views and no `contentDescription`.
- **Tap hit-testing** uses `getLocationOnScreen` + bounds and picks the deepest
  clickable, named View. Overlapping siblings or non-rectangular touch targets
  can mislabel; an unresolved tap is recorded as `tap:?`.
- **Crash flush is best-effort.** The uncaught-exception handler flushes
  synchronously on the crashing thread before chaining to the prior handler, but
  if the process is already corrupt the POST may not complete. There is no
  on-disk spool/retry across launches in v0.
- **One `decorView` listener at a time.** The SDK tracks the foreground Activity;
  dialogs/popups in separate windows are not separately observed in v0.
- **`setOnTouchListener` is pass-through** (returns false) but it does replace any
  touch listener the host set directly on the decor view (uncommon). View-level
  listeners are unaffected.
- **Sampling** is a per-process coin flip at `init`, not a stable per-user/device
  decision.
