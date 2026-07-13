# Reproit Tauri causal plugin

Add the crate and one builder plugin:

```rust
tauri::Builder::default()
    .plugin(tauri_plugin_reproit::init())
```

Outside a Reproit run the injected script is empty. During a run it installs at
document start, captures redacted frontend `fetch` and `XMLHttpRequest`
exchanges, reads the shared action clock, and serves capsule responses
fail-closed before application code. Synchronous XHR is rejected during causal
runs because it cannot be replayed without re-entering the webview event loop.
