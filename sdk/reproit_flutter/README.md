# reproit_flutter

Production telemetry for Flutter apps. Emits the **same** state-graph and error events from real
users that the reproit test runners emit, so the production graph aligns 1:1 with test-time graphs.
When a user hits an error, the event carries the graph path that produced it, which the reproit
cloud turns into a deterministic replay: a prod "cannot reproduce" becomes a reproducible test.

It mirrors the web SDK (`sdk/reproit-web.js`): same FNV-1a state signature, same event shapes, same
`/v1/events` batch endpoint, so web and Flutter telemetry land in one cloud graph.

## Quickstart

Until the pub.dev package is published, use Flutter's supported git-subdirectory dependency:

```yaml
# pubspec.yaml
dependencies:
  reproit_flutter:
    git:
      url: https://github.com/ReproIt/reproit.git
      path: sdk/reproit_flutter
      ref: v1.0.0
```

```dart
void main() {
  WidgetsFlutterBinding.ensureInitialized();
  ReproIt.init(const ReproItConfig(
    appId: 'app_...',
    endpoint: 'https://ingest.reproit.com',
    apiKey: 'pk_live_...',
    buildVersion: '1.4.2',
    buildCommit: 'abc123',
  ));
  runApp(const MyApp());
}
```

An explicit configuration runs in release builds. `ReproIt.start()` remains a debug/profile
convenience unless `enableInRelease: true` is passed.

## Exploratory testing

Add `ReproIt.captureBug()` to a debug menu or tester button. It sends the rolling structural path
and current state, never field values. Run `reproit create` in the app source directory. The CLI
waits for the report, replays it from a clean launch, shrinks the path, and only then promotes it to
a confirmed bug.

## How it works

- Forces the semantics tree on (`ensureSemantics()`) so it reads the same accessibility tree the
  test runner sees, with no a11y service attached.
- Snapshots that tree, debounced, after the UI settles; the **state signature** is FNV-1a over the
  sorted visible accessible names (byte-identical to the generated explorers).
- Records each tap by hit-testing the tapped semantics node. `action` is a structural replay
  selector (`tap:key:<key>` or `tap:role:<role>#<idx>`); `label` is optional display text.
- Hooks `FlutterError.onError` and `PlatformDispatcher.onError`; an error event carries the full
  action path leading to it.
- Normalizes capture records into version 1 event frames and POSTs them to
  `<endpoint>/v1/events` with
  `Authorization: Bearer <apiKey>`.

## Usage

```dart
import 'package:reproit_flutter/reproit_flutter.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  ReproIt.init(const ReproItConfig(
    appId: 'example',
    endpoint: 'https://ingest.reproit.com',
    apiKey: 'pk_live_...',
    buildVersion: '1.4.2',
    buildCommit: 'abc123',
    sampleRate: 1.0,      // fraction of sessions to record
    redactLabels: false,  // true = send signatures only, no label text
  ));
  runApp(const MyApp());
}
```

Optionally label route transitions as `nav:<routeName>`:

```dart
MaterialApp(
  navigatorObservers: [ReproIt.navigatorObserver],
  // ...
);
```

If `endpoint` is null, events go to the `onEvent` callback (or debug console) instead of the
network, which is handy for local inspection:

```dart
ReproIt.init(ReproItConfig(appId: 'example', onEvent: (e) => print(e)));
```

## Explicit indicator ownership

Use `ReproIt.indicator` for a badge or notification dot with a known owner. The callback returns
global `Rect`s, normally derived with `RenderBox.localToGlobal`, for the indicator, owner, and
container. Mark animation or unresolved transforms in the geometry. ReproIt abstains until two
identical settled samples exist.

## Focus visibility

Use `ReproIt.focusedInput` for an editable that may be hidden by the keyboard. Return the field and
usable viewport as global `Rect`s, and set `exactKeyboardRect` only when the keyboard inset is exact
and settled. The `reveal` callback must use the owning `Scrollable` and return false when no safe
reveal is possible. ReproIt reports only after reveal and two identical samples still show no
visible field rectangle; ambiguous cases abstain.

## Structural lifecycle and action contracts

Use `ReproIt.preserveState` with `stateBoundary(kind, phase)` around authoritative Flutter
lifecycle, navigation, and process events. Use `actionEffect`, `actionBegin`, and `actionEnd` for
exact route and local-state effects. Contracts consume structural tokens rather than labels. Missing
or unsettled evidence abstains; process recreation requires explicit persistent save/load callbacks.

## Causal HTTP reproduction

Use `ReproIt.run` at app entry to make `package:http` traffic automatically capturable and
replayable under ReproIt while remaining inert in production:

```dart
void main() => ReproIt.run(
  const ReproItConfig(appId: 'example'),
  () => runApp(const App()),
);
```

During fuzzing the zone client emits redacted causal exchanges. During `run` it serves the capsule
response and fails closed on any unmatched request. Bootstrap traffic is phase 0; UI actions are
1-based. Custom clients created outside this zone must use `ReproItCausalClient` explicitly.

## Event shapes

Edge (state transition):

```json
{
  "kind": "edge",
  "from": "<sig>",
  "action": "tap:key:open-settings",
  "label": "Open Settings",
  "to": "<sig>",
  "labels": ["..."],
  "t": 1717939200123
}
```

Error (with replay path + PII-safe input fingerprint):

```json
{
  "kind": "error",
  "oracle": "crash",
  "sig": "<sig>",
  "path": [
    { "sig": "s1", "action": "tap:key:open-settings", "label": "Open Settings" },
    { "sig": "s2", "action": "back" }
  ],
  "message": "...",
  "stack": ["..."],
  "source": "file.dart",
  "line": 42,
  "context": {
    "fingerprint": [
      {
        "field": "Email",
        "len": 18,
        "charset": "ascii",
        "hasEmoji": false,
        "isEmpty": false,
        "isRtl": false
      }
    ]
  },
  "t": 1717939200123
}
```

These match the cloud's `POST /v1/events` contract, which folds edges into the production graph and
stores errors with their path for repro (`GET /v1/apps/:app/buckets/:bucket`).

## Privacy

Set `redactLabels: true` to send only state signatures and actions (no human-readable label text).
Use `sampleRate` to record a fraction of sessions.

### Input fingerprinting (PII-safe, features not values)

On an error, for each on-screen text field the SDK derives PII-safe FEATURES of the field's value
and attaches them to the error under `context.fingerprint`. It captures FEATURES, never the raw
value: `{ field, len, charset, hasEmoji,
isEmpty, isRtl }`, where `len` is the Unicode code-point
count, `charset` is `numeric` | `ascii` | `unicode`, and the flags mark emoji / empty / RTL. This
lets the cloud build a property-matched replay fixture (a 312-char name, an emoji, a Turkish dotless
"i", an empty or RTL field) WITHOUT storing PII. The pure function
`ReproIt.fingerprintValue(String)` is host-unit-tested.

Field values are read from the live semantics tree (a text-field node's `value`, which is what the
platform accessibility layer exposes), then fingerprinted and discarded. Honest limitation: obscured
fields (`obscureText`, e.g. passwords) present their value as masked bullets in semantics, so their
fingerprint reflects the masked form (length right, charset `ascii`); the real value is never read.
Fields with no value contribute `isEmpty: true`. The field label is a stable a11y label / hint, or a
positional index, never derived from the value.

## Tests

`flutter test` covers signature parity with the web SDK / runners, live state-graph capture from a
widget tree, and the PII-safe `fingerprintValue` function (`test/fingerprint_test.dart`).
