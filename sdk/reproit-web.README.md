# reproit-web

Production telemetry SDK for **web apps, Electron, and Tauri**. One file,
`reproit-web.js`, drop-in, no build step required.

It emits the same marker protocol the reproit runner uses, but driven by real
users: each screen a user lands on is hashed to a structural state signature,
each navigation is an edge. The signature function is byte-identical to the
runner's, so a production session aligns 1:1 with your test app map and a
production error ships with the exact graph path that led to it (a deterministic
repro instead of a "cannot reproduce" ticket).

## Quickstart

Vendor the source file from the public repository, then initialize it with the
project values shown by ReproIt Cloud:

```sh
mkdir -p src/vendor
curl -fsSLo src/vendor/reproit-web.js \
  https://raw.githubusercontent.com/ReproIt/reproit/main/sdk/reproit-web.js
```

```html
<script src="/src/vendor/reproit-web.js"></script>
<script>
  ReproIt.start({
    appId: "app_...",
    endpoint: "https://ingest.reproit.com/v1/events",
    key: "pk_live_...",
    build: { version: "1.4.2", commit: "abc123" }
  })
</script>
```

Or as a module:

```js
import "./vendor/reproit-web.js";

ReproIt.start({
  appId: "app_...",
  endpoint: "https://ingest.reproit.com/v1/events",
  key: "pk_live_...",
  build: { version: "1.4.2", commit: "abc123" },
});
```

The endpoint is the exact POST target and must end in `/v1/events`. The
`pk_live_...` key is write-only and safe to ship in client code. Never put an
`sk_live_...` key in a web application.

For local inspection without Cloud, omit `endpoint` and `key`. Events go to
`onEvent` or the console:

```js
ReproIt.start({ onEvent: console.log });
```

`ReproIt.init(opts)` (below) stays the full, explicit entry point; `start()` is
just the zero-config one-liner over it.

## Why one SDK covers three platforms

Electron and Tauri both render their UI in a webview (Electron uses Chromium,
Tauri uses the host system WebView). The reproit signature is computed from the
DOM accessibility structure, so the exact same walk produces the exact same
signature in all three. There is nothing platform-specific to build. The
electron and tauri **runners** (`runners/electron.mjs`, `runners/tauri.mjs`)
compute the identical signature, and that equality is gated in
`runners/signature_test.mjs`. The SDK's own parity gate is
`sdk/test/signature_test.js`, which asserts all 25 golden vectors in
`signature_vectors.json` reproduce exactly.

## Web

```html
<script src="/src/vendor/reproit-web.js"></script>
<script>
  ReproIt.init({
    appId: "app_...",
    endpoint: "https://ingest.reproit.com/v1/events",
    key: "pk_live_..."
  })
</script>
```

Or as a module:

```js
import "./vendor/reproit-web.js";
ReproIt.init({
  appId: "app_...",
  endpoint: "https://ingest.reproit.com/v1/events",
  key: "pk_live_..."
});
```

## Electron

Load it in the **renderer** process, where the DOM lives, exactly like a web
page. Do not load it in the main process (there is no DOM there).

```js
// renderer entry (e.g. renderer.js), or a <script> in your renderer HTML
import "./vendor/reproit-web.js";
ReproIt.init({
  appId: "app_...",
  endpoint: "https://ingest.reproit.com/v1/events",
  key: "pk_live_..."
});
```

## Tauri

Import it from your frontend bundle like any other web dependency. It runs in
the WebView, so the DOM walk and signatures behave exactly as on the web.

```js
import "./vendor/reproit-web.js";
ReproIt.init({
  appId: "app_...",
  endpoint: "https://ingest.reproit.com/v1/events",
  key: "pk_live_..."
});
```

## Build tagging

Pass your **build** so reproit can tell you which build a bug regressed in (and
stop alerting once a later build no longer hits it):

```js
ReproIt.init({
  appId: "app_...",
  endpoint: "https://ingest.reproit.com/v1/events",
  key: "pk_live_...",
  build: { version: "1.4.2", commit: "abc123" }, // app version + git commit from CI
});
```

It rides every event's context as `context.build = { version, commit }` (only
the fields you set). Omit `build` and behavior is unchanged.

## Privacy

Signatures are structural (a hash of which controls exist), not user data. With
`redactLabels: true`, only hashes leave the browser. On an error, the SDK
attaches PII-safe input fingerprints (field length, charset, isRtl, and so on),
never raw values. Password and hidden fields are never read.

## Parity

```sh
# from the sdk/ directory
node test/signature_test.js        # SDK core vs the 25 golden vectors
node ../runners/signature_test.mjs # electron + tauri runners vs the same vectors
```

Both run automatically in CI (the `signature-parity` job in
`.github/workflows/ci.yml`).
