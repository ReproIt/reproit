# reproit-react-native

Production telemetry for React Native apps. Emits the **same** state-graph and
error events from real users that the reproit test runners emit, so the
production graph aligns 1:1 with test-time graphs. When a user hits an error,
the event carries the graph **path** that produced it, which the reproit cloud
turns into a deterministic replay: a prod "cannot reproduce" becomes a
reproducible test.

It mirrors the web SDK (`sdk/reproit-web.js`) and the Flutter SDK
(`sdk/reproit_flutter`): same FNV-1a state signature, same event shapes, same
`{appId, sentAt, events}` batch POSTed to `<endpoint>/v1/events`, so web,
Flutter and React Native telemetry land in one cloud graph.

## Install

```sh
npm install reproit-react-native
```

`react` and `react-native` are peer dependencies (use your app's versions).

## Usage

One init call in your app entry (e.g. `index.js` / `App.tsx`):

```ts
import { ReproIt } from 'reproit-react-native';

ReproIt.init({
  appId: 'example',
  endpoint: 'https://ingest.reproit.example', // null => onEvent / debug only
  apiKey: 'sk_...',                           // optional Bearer token
  sampleRate: 1.0,                            // fraction of sessions to record
  redactLabels: false,                        // true => signatures only, no text
  build: { version: '1.4.2', commit: 'abc123' }, // your build, so reproit can tell
                                              // which build a bug regressed/resolved in
});
```

Pass your **build** (app version from `package.json`/`Info.plist`/gradle + the
git commit from CI) and reproit segments every error by it, so the cloud can
tell you which build a bug regressed in and stops alerting once a later build
stops hitting it. RN can't auto-detect these without a native module, so you
provide them from your build pipeline. It rides every event's context as
`context.build = { version, commit }` (only the fields you set). Omit `build`
entirely and behavior is unchanged.

That alone captures **state snapshots** (from the live React fiber tree) and
**errors** (with their graph path). To also label edges with `tap:<label>` and
`nav:<route>`, wrap your tree with the optional provider:

```tsx
import { ReproItProvider } from 'reproit-react-native';
import { createNavigationContainerRef } from '@react-navigation/native';

const navigationRef = createNavigationContainerRef();

export default function App() {
  return (
    <ReproItProvider navigationRef={navigationRef}>
      <NavigationContainer ref={navigationRef}>
        {/* ...your app... */}
      </NavigationContainer>
    </ReproItProvider>
  );
}
```

`@react-navigation` is **not** a dependency; the `navigationRef` prop is
duck-typed (any object with `addListener('state', cb)` and `getCurrentRoute()`
works). Omit it and route edges fall back to `auto`/`tap:` from snapshots.

If `endpoint` is null, events go to the `onEvent` callback (or the debug
console) instead of the network, which is handy for local inspection:

```ts
ReproIt.init({ appId: 'example', onEvent: (e) => console.log(e) });
```

## What it captures (and how)

React Native has **no DOM** and no public synchronous accessibility-tree API a
library can read. Here is exactly what this SDK does and what it cannot do.

**State snapshots, React fiber walk.** On a debounced settle, the SDK walks
the mounted React fiber tree (via the React DevTools global hook that the RN
renderer always registers) and collects the same three signals the runner reads
out of Appium's accessibility XML at test time (`runners/rn/runner.mjs`):

- **labels**: visible accessible names, `accessibilityLabel || text content`,
  trimmed, first line only, skipped if empty or longer than `maxLabelLen` (40),
  deduped, capped at `maxLabels` (24) per state.
- **tappables**: nodes with an `onPress` / `onClick` or a button|link|tab|
  switch|checkbox|... `accessibilityRole`.
- **unlabeled**: count of tappables that have **no** accessible name (the same
  a11y smell the runner flags).

The **state signature** is FNV-1a over the sorted, `|`-joined labels,
byte-identical to the web SDK, Flutter SDK, and runner. Sorting makes it
order-independent.

**Edges.** When a settled snapshot's signature differs from the current one,
an `edge` is emitted. The first one is `load`; later ones use the pending
action (`tap:<label>` or `nav:<route>`) if the provider supplied one, else
`auto`.

**Navigation.** With a `navigationRef`, route changes are recorded as
`nav:<routeName>` (mirrors the Flutter `navigatorObserver` and the runner's
`nav:` edges).

**Taps.** The provider installs a root-level
`onStartShouldSetResponderCapture` that observes every touch-down **without
stealing the gesture** (it always returns `false`), then labels the next edge.

**Errors.** The SDK wraps `global.ErrorUtils.setGlobalHandler` (RN's uncaught
JS error hook) and chains the previous handler so the red box still shows. It
also listens for `unhandledRejection` where a tracker is available. Each error
event carries `sig`, the graph `path`, `message`, up to 8 `stack` lines, and a
best-effort `source`/`line` parsed from the top frame.

## Context, "which users hit it"

A state graph tells you *what* broke; the batch **context** (`ctx`) tells you
*who* it broke for, so a prod "works for me but not for them" becomes a
queryable cohort. The cloud's ingest endpoint (`POST /v1/events`) folds `ctx`
into every event and computes a **cohort discriminator** (e.g. "this error hits
users where `locale=tr`"). All dimensions are low-cardinality and zero-PII.

**Tier-1 auto dimensions** are collected at `init` (dependency-free):

- **platform**: `Platform.OS` (`ios` | `android` | `web` | ...)
- **osVersion**: `Platform.Version`, stringified
- **locale**: from `Intl.DateTimeFormat().resolvedOptions().locale`
- **tz**: IANA timezone from the same Intl resolved options
- **release**: `!__DEV__` (true in a release build)

**Identify (hashed).** Group "these N users hit it" without storing identity:

```ts
ReproIt.identify('user@example.com', { role: 'admin', plan: 'free' });
```

`userId` is hashed with SHA-256 (first 16 hex chars) into `uid`; the raw value
is **never sent**. The hash is byte-identical to the Flutter SDK, so the same
user maps to the same `uid` across platforms. The optional second argument
merges extra dimensions.

**Set dimensions** any time (merged into the next batch's `ctx`):

```ts
ReproIt.setContext('plan', 'free');
ReproIt.setContexts({ region: 'eu', seats: 3 });
```

When non-empty, the context rides along as the batch-level `ctx`:

```json
{ "appId": "example", "sentAt": 1717939200123,
  "ctx": { "platform": "ios", "osVersion": "17.4", "locale": "en-US",
           "tz": "America/New_York", "release": true,
           "uid": "8f1b...", "role": "admin" },
  "events": [ ... ] }
```

> **Locale source (honest limitation).** `locale`/`tz` come from the JS `Intl`
> API, not a native module, to stay dependency-free. On Hermes, `Intl` ships
> when built with `intl` enabled (the RN default since 0.73); when it is not,
> `locale`/`tz` are simply omitted (never throws). A device locale via a native
> module would be more precise but needs a native dependency, which this SDK
> deliberately avoids. `platform`/`osVersion` come from RN core `Platform` and
> are omitted only if `react-native` is unavailable (e.g. a pure-JS test env).

## Limitations (honest)

This is the hard part on RN; here is what is **best-effort** rather than exact:

- **Fiber walk reads React internals.** There is no public API to enumerate
  mounted accessibility nodes synchronously, so the SDK reads the DevTools
  global hook + fiber `memoizedProps`. This is the same mechanism RN's own
  testing/inspection tooling relies on, but it is not a stable public contract:
  if a future React renderer changes shape, the walk degrades to an **empty
  snapshot** (it never throws, and never breaks your app). **State signatures
  are exact when the walk succeeds**; they are simply absent if it can't read
  the tree.
- **No on-screen visibility / occlusion test.** The web SDK uses
  `getBoundingClientRect` + computed style to drop hidden nodes. RN gives a
  library no synchronous geometry, so the walk skips only
  `accessibilityElementsHidden` / `aria-hidden` subtrees. A screen that is
  *mounted but off-screen* (e.g. a tab navigator that keeps inactive tabs
  mounted) can contribute labels. React Navigation's default stack
  unmounts/detaches inactive screens, which keeps this close to correct in
  practice; tab navigators are best-effort.
- **Tap-label precision.** RN exposes no synchronous hit-test by screen point,
  so the provider cannot map a touch coordinate to the exact tapped node the way
  the Flutter SDK hit-tests its semantics tree. When several tappables are on
  screen, the `tap:<label>` is a best-effort pick (first tappable in tree
  order). **This only affects the edge's action label, not the from/to STATE
  signatures**, which come from the fiber snapshot and are exact. Navigation
  edges (`nav:<route>`) are exact when a `navigationRef` is supplied.
- **Content outside the fiber tree** (native modules, WebViews) is invisible to
  the walk. Use the documented manual hook for those (below).
- **Unhandled promise rejections** are only captured where a global
  `process.on('unhandledRejection', ...)` exists; this varies by RN engine
  (Hermes/JSC) and version. Synchronous uncaught errors via `ErrorUtils` are
  always captured.
- **Input fingerprints read fiber props.** The on-error `context.fingerprint`
  reads each `TextInput`'s `text`/`value`/`defaultValue` host prop. Controlled
  inputs expose live text; uncontrolled inputs only expose `defaultValue`, so
  mid-edit text in an uncontrolled field may be missed. Password fields
  (`secureTextEntry`) are never read. See "Input fingerprinting" under Privacy.

### Manual snapshot escape hatch

For UI the fiber walk can't see, contribute a snapshot from a known list of
accessible names. The signature is computed exactly as for an automatic
snapshot, so it aligns with test-time data:

```ts
ReproIt.recordSnapshot(['Buy Now', 'Add to Cart', 'Back'], 'nav:Checkout');
```

## Configuration

All fields mirror the web/Flutter SDKs:

| field          | default | meaning                                            |
| -------------- | ------- | -------------------------------------------------- |
| `appId`        | (req.)  | identifies the app in the cloud                    |
| `endpoint`     | `null`  | `POST <endpoint>/v1/events`; null => onEvent/debug |
| `apiKey`       | `null`  | `Authorization: Bearer <apiKey>` when set          |
| `onEvent`      | `null`  | callback for every event (dev hook / transport)    |
| `build`        | `null`  | `{ version?, commit? }`; stamped as `context.build` |
| `sampleRate`   | `1.0`   | fraction of sessions that report                   |
| `maxLabels`    | `24`    | labels per state signature                         |
| `maxLabelLen`  | `40`    | labels longer than this are ignored                |
| `pathCap`      | `60`    | length of the repro action trail kept              |
| `flushMs`      | `5000`  | batch flush interval                               |
| `redactLabels` | `false` | true => send signatures only, no label text        |
| `debounceMs`   | `350`   | settle window before snapshotting                  |

## Event shapes

Edge (state transition):

```json
{ "kind": "edge", "from": "<sig>", "action": "tap:Open Settings",
  "to": "<sig>", "labels": ["..."], "t": 1717939200123 }
```

Error (with replay path + PII-safe input fingerprint):

```json
{ "kind": "error", "sig": "<sig>",
  "path": [{ "sig": "s1", "action": "tap:X" }, { "sig": "s2", "action": "nav:Settings" }],
  "message": "...", "stack": ["..."], "source": "App.tsx", "line": 42,
  "context": { "fingerprint": [
    { "field": "email", "len": 18, "charset": "ascii",
      "hasEmoji": false, "isEmpty": false, "isRtl": false }
  ] },
  "t": 1717939200123 }
```

These match the cloud's `POST /v1/events` contract, which folds edges
into the production graph and stores errors with their path for repro
(`GET /v1/apps/:app/buckets/:bucket`).

## Privacy

Signatures are **structural** (a hash of which controls exist), not user data.
Set `redactLabels: true` to send only signatures + actions (no human-readable
label text). Use `sampleRate` to record a fraction of sessions.

### Input fingerprinting (PII-safe, features not values)

On an error, for each on-screen text field the SDK derives PII-safe FEATURES of
the field's value and attaches them under `context.fingerprint`. It captures
FEATURES, never the raw value: `{ field, len, charset, hasEmoji, isEmpty,
isRtl }`, where `len` is the Unicode code-point count, `charset` is `numeric` |
`ascii` | `unicode`, and the flags mark emoji / empty / RTL. The cloud uses
these to build a property-matched replay fixture (a 312-char name, an emoji, a
Turkish dotless "i", an empty or RTL field) WITHOUT storing PII. The pure
function `fingerprintValue(str)` is exported and host-unit-tested
(`test/fingerprint.test.ts`).

Honest limitation: field values are read from the fiber tree's `TextInput` host
props (`text` / `value`, else `defaultValue`). Controlled inputs expose their
current text; an uncontrolled input only exposes `defaultValue`, so a user's
live edits to an uncontrolled field may not be visible. Password fields
(`secureTextEntry`) are skipped entirely and never read. Values are
fingerprinted to features immediately and discarded; raw text never leaves the
device. The field label is a stable a11y label / testID / placeholder, or a
positional index, never derived from the value.

## Development

```sh
npm install
npm test         # jest: signature parity + snapshot label rules
npm run lint     # eslint
npm run typecheck
npm run build    # tsc -> dist/ with .d.ts
```

The parity test (`test/signature.test.ts`) asserts the FNV-1a signature is
byte-identical to the web SDK, Flutter SDK, and runner:

```
["Home Screen","Open Settings","Open Profile","Trigger Crash"] => "951259c1"
["Settings","Back"]                                            => "054d1bbf"
[]                                                             => "811c9dc5"
```

## License

Elastic License v2 (ELv2), consistent with the reproit runner.
