# SDKs

An SDK reports a failure with enough structure to reproduce it. It is one line plus init. The
dashboard renders the snippet below with your real `appId`, key, and endpoint already substituted.

Use your **publishable** `pk_live_...` key here. It is write-only and accepted only on ingest,
which is what makes it safe to ship inside an application.

Always set `build`. A version and commit are what tie an occurrence back to the code that produced
it, and the regression sweep uses build order to tell a real regression from an old rollout.

## Web (also Electron and Tauri)

```js
import './vendor/reproit-web.js';

ReproIt.start({
  appId: 'your-app',
  key: 'pk_live_...',
  endpoint: 'https://cloud.reproit.com/v1/events',
  build: { version: '1.4.2', commit: 'abc123' },
});
```

## React Native

```js
import { ReproIt } from 'reproit-react-native';

ReproIt.init({
  appId: 'your-app',
  endpoint: 'https://cloud.reproit.com',
  apiKey: 'pk_live_...',
  build: { version: '1.4.2', commit: 'abc123' },
});
```

## Flutter, iOS, Android, Linux

Same shape, per-language spelling: `ReproIt.init(...)` with `appId`, `endpoint`, `apiKey`, and the
build version and commit. Each SDK's README under `sdk/` has its install line and full options.

## Server SDKs

Eight languages (node, python, rust, go, java, dotnet, php, ruby) record a failing operation
**together with its outbound exchanges**, and serve those exchanges back at replay. That is what
makes a production failure re-executable with the database stopped and the network denied:

```js
instrument.install();
```

Each ships an acceptance test pinning all four verdicts on a real failure. See
[what a repro is made of](../repros.md).

## Turning off labels

`redactLabels: true` sends only structural hashes, with no visible UI text of any kind. Input
values are never sent under any setting; see [security](security.md).
