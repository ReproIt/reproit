# reproit-windows

Production telemetry for native Windows desktop apps: **WPF** (`System.Windows`)
and **WinUI 3** (`Microsoft.UI.Xaml`). Emits the **same** state-graph and error
events from real users that the reproit test runners emit, so the production
graph aligns 1:1 with test-time graphs. When a user hits an error, the event
carries the graph path that produced it, which the reproit cloud turns into a
deterministic replay: a prod "cannot reproduce" becomes a reproducible test.

It mirrors the web SDK (`sdk/reproit-web.js`) and the Android / iOS / Flutter /
React-Native SDKs: same canonical **structural** state signature, same event
shapes, same `/v1/events` batch endpoint, so all platforms land in one cloud
graph. The signature is the canonical contract in `docs/signature.md`, proven
byte-for-byte against `signature_vectors.json` (see the parity test below). The
external Windows runner (`runners/windows-uia.py`) drives the same app over UI
Automation and computes the identical signature, so an in-app crash buckets to
the same node a fuzz finding hits.

## Project layout

```
sdk/reproit-windows/
  ReproIt.Windows.sln
  src/ReproIt.Core/            cross-platform, no Windows dependency (netstandard2.0; net8.0)
    Signature.cs               canonical structural signature (Node, descriptor, FNV-1a, value-class)
    Engine.cs                  snapshot reduction, edge/error state machine, context, batching
    Json.cs                    minimal JSON encoder (events) + decoder (parity vectors)
    Fingerprint.cs             PII-safe input fingerprint (features, not values)
    ReproItConfig.cs           config
  src/ReproIt.Windows/         the WPF + WinUI 3 in-app binding (net8.0-windows)
    Capture.cs                 visual-tree -> canonical Node tree (reflection over both XAML stacks)
    ReproItClient.cs           init, screen anchor, tag helpers, crash handlers, HTTP transport
  test/ReproIt.ParityTests/    cross-platform parity gate (net8.0, references ReproIt.Core only)
    SignatureParityTest.cs     loads ../../../signature_vectors.json, asserts all 25 vectors
    EngineTest.cs              wire-shape / context / fingerprint tests
```

The deterministic, parity-critical logic lives in **ReproIt.Core**, which has
**no WPF / WinUI dependency** and multi-targets `netstandard2.0` + `net8.0`, so
the canonical signature is reproducible on any host (and the parity test runs on
the plain .NET SDK on macOS / Linux / Windows), exactly like the Kotlin / Swift
signature cores run on the host JVM / Foundation without the platform SDK.

## How it works

- `ReproItClient.Init(config)` installs the crash handlers and starts the flush
  timer; `ReproItClient.Attach(window)` begins capturing from your main Window's
  content root.
- Snapshots the **live visual tree** of the attached Window, recursing visible
  elements (`Visibility == Visible`, non-zero `ActualWidth`/`Height`). The **state
  signature** is the canonical STRUCTURAL descriptor of the captured node tree
  (roles + ids + input types + icons + tree shape, prefixed by the screen
  anchor), hashed with FNV-1a 32-bit. Localized text never enters the hash, so an
  EN and a DE render of the same screen hash identically; byte-identical to the
  Rust oracle, the other SDKs, and the `windows-uia.py` runner. Accessible names
  are kept only as a display-only `labels` field, never folded into the hash.
- **Roles** are derived from the control class / `AutomationProperties` (never
  from text), with the same `ControlType -> role` mapping the UIA runner uses
  (`runners/windows-uia.py`). **Ids** come from `AutomationProperties.AutomationId`
  or `x:Name` (or a `ReproItClient.TagId` marker). **Input types** distinguish
  `PasswordBox` (`password`) and `InputScope` hints (`email`/`number`); **icons**
  come from a `FontIcon.Glyph` / `SymbolIcon.Symbol` or a `ReproItClient.TagIcon`
  marker.
- Snapshots are **debounced** (default 350 ms) on `LayoutUpdated` / `SizeChanged`
  / `Activated`, so the snapshot is taken once the UI settles.
- **Taps** are observed by a pass-through `PreviewMouseDown` (WPF) /
  `PointerPressed` (WinUI) handler that never sets `Handled`, so the app's own
  input is unaffected. The event source's accessible name becomes the
  `tap:<label>` edge action, just like a fuzz run.
- **Errors** are captured via `AppDomain.UnhandledException` AND the XAML
  `DispatcherUnhandledException` (WPF `Application.DispatcherUnhandledException` /
  WinUI `Application.UnhandledException`), plus `TaskScheduler.UnobservedTaskException`.
  An error event carries the current signature and the full action path leading
  to it, and is flushed synchronously before the process dies.
- Batches events and POSTs `{appId, sentAt, ctx?, events}` to `<endpoint>/v1/events`
  with `Authorization: Bearer <apiKey>` (via `HttpClient`).
- Attaches a PII-safe **context** map (`ctx`) to each batch (see below), which the
  cloud uses to answer "which users hit this?" without storing identity.

## Usage

WPF (in your `App` or main window):

```csharp
using ReproIt.Core;
using ReproIt.Windows;

public partial class MainWindow : Window
{
    public MainWindow()
    {
        InitializeComponent();
        ReproItClient.Init(new ReproItConfig("example")
        {
            Endpoint = "https://ingest.reproit.example",
            ApiKey = "sk_...",
            SampleRate = 1.0,      // fraction of sessions to record
            RedactLabels = false,  // true = send signatures only, no label text
        });
        ReproItClient.Attach(this); // `this` is the Window
    }
}
```

WinUI 3 (after the main `Window` is created):

```csharp
ReproItClient.Init(new ReproItConfig("example") { Endpoint = "...", ApiKey = "..." });
ReproItClient.Attach(mainWindow);
```

If `Endpoint` is null, events go to the `OnEvent` callback (or `Debug` output)
instead of the network, which is handy for local inspection:

```csharp
ReproItClient.Init(new ReproItConfig("example") { OnEvent = e => Debug.WriteLine(e) });
```

Set the screen anchor from your navigation layer so two same-shaped screens at
different routes hash distinctly:

```csharp
ReproItClient.Screen("/settings");
```

Flush manually before a known teardown with `ReproItClient.Flush()`.

## Config

Field names and defaults mirror the web SDK:

| field          | default | meaning |
|----------------|---------|---------|
| `AppId`        | (req)   | app id in every batch |
| `Endpoint`     | `null`  | `POST <endpoint>/v1/events`; null = OnEvent/log only |
| `ApiKey`       | `null`  | `Authorization: Bearer <apiKey>` when set |
| `OnEvent`      | `null`  | dev hook / custom transport, called per event |
| `SampleRate`   | `1.0`   | fraction of sessions that report (decided once) |
| `MaxLabels`    | `24`    | labels kept per state |
| `MaxLabelLen`  | `40`    | labels longer than this are skipped |
| `PathCap`      | `60`    | max length of the repro trail |
| `FlushMs`      | `5000`  | batch flush interval |
| `RedactLabels` | `false` | true = signatures only, no label text |
| `DebounceMs`   | `350`   | settle window before snapshotting |

## Event shapes

Edge (state transition):

```json
{ "kind": "edge", "from": "<sig>", "action": "tap:Open Settings",
  "to": "<sig>", "labels": ["..."], "t": 1717939200123 }
```

Error (with replay path):

```json
{ "kind": "error", "sig": "<sig>",
  "path": [{ "sig": "s1", "action": "tap:X" }, { "sig": "s2", "action": "nav" }],
  "message": "...", "stack": ["..."], "source": "...", "line": 0,
  "t": 1717939200123 }
```

Batch envelope: `{ "appId": "...", "sentAt": <ms>, "ctx": {...}?, "events": [...] }`
(`ctx` is omitted when empty). These match the cloud's `POST /v1/events`
contract, which folds edges into the production graph and stores errors
with their path for repro.

## Context: which users hit it (`ctx` / `Identify`)

The SDK attaches a small, **PII-safe** context map to every batch as the `ctx`
field; the cloud folds it into each event and computes a cohort discriminator
(e.g. "this error is 6x over-represented in `locale=tr`").

**Tier-1 auto dimensions** are populated automatically at `Init` (zero PII):

| key        | source                              |
|------------|-------------------------------------|
| `platform` | `"winui"`                           |
| `os`       | `Environment.OSVersion.Version`     |
| `locale`   | `CultureInfo.CurrentUICulture.Name` |
| `tz`       | `TimeZoneInfo.Local.Id`             |

`platform` is `"winui"` to match the registered platform id in
`crates/reproit/src/backends/platform.rs` and the UIA runner.

**Custom dimensions** (PII-safe buckets/enums, not raw input):

```csharp
ReproItClient.SetContext("plan", "pro");
ReproItClient.SetContexts(new Dictionary<string, object> { { "role", "admin" } });
```

**Identify**, group "these N users hit it" without storing identity. The raw user
id is hashed with SHA-256 (only a 16-char hex prefix is kept as `uid`); the raw
value is never stored or sent:

```csharp
ReproItClient.Identify(user.Id, new Dictionary<string, object> { { "plan", "pro" } });
// ctx -> { ..., "uid": "a1b2c3d4e5f60718", "plan": "pro" }
```

## Developer annotations

When `AutomationId` / `x:Name` is not enough, annotate elements directly:

```csharp
ReproItClient.TagId(element, "submit");        // stable structural id
ReproItClient.TagIcon(element, "e5cd");         // language-independent icon identity
ReproItClient.TagTransient(element);            // drop this subtree from the hash (rule 2)
ReproItClient.TagValue(counterText);            // fold its value-class into the signature (Layer 3)
ReproItClient.TagValue(scoreText, "42");        // explicit value override
```

`TagValue` is for counters / scores / stopwatches shown in plain `TextBlock`s,
where structure never moves but the displayed value does. Its value is bucketed
into a bounded, locale-safe value-class (`docs/signature.md` "Value-state").

## Build & test

A full build of the Windows binding (`ReproIt.Windows`) targets `net8.0-windows`
and so requires a **Windows** SDK / build host.

The **parity test references only `ReproIt.Core`** (no WPF / WinUI), so it runs on
the plain .NET 8 SDK on any OS:

```sh
# from sdk/reproit-windows/, on any host with the .NET 8 SDK:
dotnet test test/ReproIt.ParityTests/ReproIt.ParityTests.csproj

# on Windows you can also build everything:
dotnet build ReproIt.Windows.sln
```

The parity test loads `../../signature_vectors.json` (the repo-root golden
vectors) and asserts all 25 reproduce byte-for-byte, mirroring
`sdk/reproit-android/src/test/.../SignatureParityTest.kt`,
`sdk/reproit-ios/Tests/...`, and `sdk/test/signature_test.js`.

## Honest limitations

- **Visual tree, not the live UI Automation tree.** An in-process SDK reads the
  XAML visual tree (via reflection over the shared `DependencyObject` /
  `AutomationProperties` surface), mapping each element to a canonical role from
  its control class / automation control type (never from text). For ordinary
  apps this yields the same canonical structure the `windows-uia.py` runner sees
  over UIA. It can differ for:
  - **Heavily virtualized lists**: a `ListView`/`ItemsControl` only realizes
    on-screen item containers, so off-screen items are not in the visual tree
    (the structural collapse rule still folds the realized identical items to one
    `*` token, so this rarely changes the signature).
  - **Custom-drawn surfaces** (a `Win2D`/`SwapChainPanel` canvas) with no child
    elements and no `AutomationProperties`.
  - **Hosted content** (a `WebView2`, or a hosted Win32/HWND island), whose
    internal tree is opaque to the XAML walk.
- **Tap labeling** uses the input event's source element accessible name. Full
  hit-testing differs per stack; an unresolved tap is recorded as `tap:?`.
- **Crash flush is best-effort.** The handlers flush synchronously before the
  process dies, but a corrupt process may not complete the POST. There is no
  on-disk spool/retry across launches in v0.
- **One Window at a time.** `Attach` tracks one root; secondary windows / dialogs
  in separate top-level windows are not separately observed in v0 (a
  `ContentDialog` hosted in the same window IS captured).
- **Reflection-driven binding.** The binding never hard-links WPF or WinUI; it
  reads whichever XAML stack the host references at runtime. An event/property it
  cannot bind is simply not observed (it never throws into the host app).
- **`PasswordBox` entry is never read.** Its value is excluded from both the
  signature value-class and any fingerprinting, by design.
- **Sampling** is a per-process coin flip at `Init`, not a stable per-user/device
  decision.

## Parity note

`ReproIt.Core/Signature.cs` is a faithful port of the Rust oracle
(`crates/reproit/src/model/signature.rs`), cross-checked line-by-line against
`sdk/reproit-react-native/src/signature.ts`, `sdk/reproit-ios/.../Signature.swift`,
and `sdk/reproit-android/.../Signature.kt`. The FNV-1a uses C# `uint` (unsigned,
wraps mod 2^32 by definition), the value-class uses the strict ASCII
period-decimal grammar with `InvariantCulture` parsing (never locale-aware), and
the `V:` section sorts keys by their UTF-8 byte sequence (`Encoding.UTF8.GetBytes`
compared lexicographically) to match the Rust oracle's `String::cmp` byte order.
Note this is NOT `string.CompareOrdinal`, which is UTF-16 code-unit order and
diverges for astral characters (surrogate pairs sort below high-BMP chars under
UTF-16 but above them under UTF-8/code-point order). The golden vectors are the
single source of truth.
