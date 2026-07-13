# reproit-web

Production telemetry SDK for **web apps, Electron, and Tauri**. One file,
`reproit-web.js`, drop-in, no build step required.

It emits the same marker protocol the reproit runner uses, but driven by real
users: each screen a user lands on is hashed to a structural state signature,
each navigation is an edge. The signature function is byte-identical to the
runner's, so a production session aligns 1:1 with your test app map and a
production error ships with the exact graph path that led to it (a deterministic
repro instead of a "cannot reproduce" ticket).

## Quickstart (one line)

Drop in the file and start with a single call. No configuration required:
`start()` derives the app id from the page host and reports to `onEvent` / the
console until you point it at an endpoint.

```html
<script src="reproit-web.js"></script>
<script>ReproIt.start()</script>
```

Or as a module:

```js
import "./reproit-web.js";
ReproIt.start();
```

Point it at your ingest endpoint by passing options through the same call (any
`init` option works here):

```js
ReproIt.start({ endpoint: "https://ingest.reproit.com/v1/events", key: "sk_live_..." });
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
<script src="reproit-web.js"></script>
<script>
  ReproIt.init({ appId: "myapp", endpoint: "https://ingest.reproit.com/v1/events" })
</script>
```

Or as a module:

```js
import "./reproit-web.js";
ReproIt.init({ appId: "myapp", endpoint: "https://ingest.reproit.com/v1/events" });
```

## Electron

Load it in the **renderer** process, where the DOM lives, exactly like a web
page. Do not load it in the main process (there is no DOM there).

```js
// renderer entry (e.g. renderer.js), or a <script> in your renderer HTML
import "./reproit-web.js";
ReproIt.init({ appId: "myapp-desktop", endpoint: "https://ingest.reproit.com/v1/events" });
```

## Tauri

Import it from your frontend bundle like any other web dependency. It runs in
the WebView, so the DOM walk and signatures behave exactly as on the web.

```js
import "./reproit-web.js";
ReproIt.init({ appId: "myapp-tauri", endpoint: "https://ingest.reproit.com/v1/events" });
```

## Build tagging

Pass your **build** so reproit can tell you which build a bug regressed in (and
stop alerting once a later build no longer hits it):

```js
ReproIt.init({
  appId: "myapp",
  endpoint: "https://ingest.reproit.com/v1/events",
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
