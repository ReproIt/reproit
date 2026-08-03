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

The SDKs ship as **source in this repository**, not as published packages, so you add them by path
or submodule rather than from a registry. Full working integrations live under `fixtures/` and are
driven by CI on every push, so the snippets below cannot rot without a build going red.

**Node** (`fixtures/llm-agent-fixture/agent.mjs`):

```js
const instrument = require('<path-to>/sdk/reproit-backend-node/instrument.js');
instrument.install();
```

**Node, CI capsule mode** (`fixtures/flaky-ci-fixture/tests/checkout.test.mjs`). Wrap the suite
and a failing test spools a capsule you can re-run on a laptop:

```js
const ci = require('<path-to>/sdk/reproit-backend-node/ci.js');
const test = ci.suite('checkout');
```

**Rust** (`fixtures/rs-backend-fixture/src/main.rs`):

```rust
use reproit_backend::instrument::{self, http};
use reproit_backend::{determinism_envelope, BackendTrace, Recorder, TraceContext};
```

**Go** (`fixtures/go-backend-fixture`). Add the SDK with a `replace` directive, then wrap the
driver you already use:

```go
// go.mod
require github.com/ReproIt/reproit/sdk/reproit-backend-go v0.0.0
replace github.com/ReproIt/reproit/sdk/reproit-backend-go => ../../sdk/reproit-backend-go
```

```go
import reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"

sql.Register("app-pg", &reproit.SQLDriver{Base: pq.Driver{}})
```

Python, Java, .NET, PHP and Ruby follow the same shape: import the SDK, install the
instrumentation once at boot, and keep using your own HTTP client and database driver. Each SDK
ships an acceptance test pinning all four verdicts on a real failure. See
[what a repro is made of](../repros.md).

## Configuration examples

Per-framework `reproit.yaml` files, one for each platform, are in
[docs/examples/configs](../examples/configs). A test asserts every one of them still loads, so
they track the schema instead of drifting from it.

## Turning off labels

`redactLabels: true` sends only structural hashes, with no visible UI text of any kind. Input
values are never sent under any setting; see [security](security.md).
