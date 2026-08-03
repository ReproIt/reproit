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
build version and commit.

## Server SDKs

Eight languages (node, python, rust, go, java, dotnet, php, ruby) record a failing operation
**together with its outbound exchanges**, and serve those exchanges back at replay. That is what
makes a production failure re-executable with the database stopped and the network denied.

Mounting one is a single line, because each SDK ships a framework adapter: express and fastify for
Node, ASGI for Python, `net/http` middleware for Go, a tower layer for axum and actix, a servlet
filter for Java, `UseReproit` for ASP.NET Core, PSR-15 for PHP, and Rack for Ruby.

```python
app.add_middleware(ReproitMiddleware, capture=capture)
```

Every language, with its install line and where to reach the trace, is in
[Add ReproIt to a backend](../backend-sdk.md).

Each SDK ships an acceptance test pinning all four verdicts on a real failure. See
[what a repro is made of](../repros.md).

## Configuration examples

Per-framework `reproit.yaml` files, one for each platform, are in
[docs/examples/configs](../examples/configs). A test asserts every one of them still loads, so
they track the schema instead of drifting from it.

## Turning off labels

`redactLabels: true` sends only structural hashes, with no visible UI text of any kind. Input
values are never sent under any setting; see [security](security.md).
