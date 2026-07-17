//! reproit-tui: production telemetry SDK for Rust terminal-UI apps.
//!
//! Drop this into a real TUI app (ratatui, crossterm-direct, or any renderer)
//! and it reports the SAME canonical TUI screen signature the `reproit __tui`
//! runner computes, so a production crash reported here carries a state
//! signature the runner can replay locally. Each distinct screen is a state;
//! each screen change is a coverage edge; a panic ships the path that led to
//! it.
//!
//! Parity is by CONSTRUCTION, not by a port: the signature comes from the
//! shared `reproit-tui-sig` crate that the runner itself uses. The Go/TS/Python
//! TUI SDKs port that logic and are pinned to the same golden vectors; this
//! crate links it directly, so it cannot drift from the runner.
//!
//! Capture: a TUI app renders its own cells, so you hand the SDK the rendered
//! screen. ratatui apps build a `ratatui::buffer::Buffer` (convert its cells to
//! a contents string with `ScreenContents::from_rows`); crossterm-direct apps
//! pass the text they drew. The SDK never reads the terminal itself.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::BTreeMap, path::PathBuf};

type ReplayResponse = (u16, BTreeMap<String, String>, serde_json::Value);

// Re-export the shared signature surface so SDK users never reach past this
// crate.
pub use reproit_tui_sig::{content_fingerprint, labels_of, sig_of, structural_sig, value_class};

fn canonical_causal_url(raw: &str) -> String {
    let (base, query) = raw.split_once('?').unwrap_or((raw, ""));
    let mut parts: Vec<_> = query.split('&').filter(|part| !part.is_empty()).collect();
    parts.sort_unstable();
    if parts.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", parts.join("&"))
    }
}

/// HTTP-library-neutral causal side channel. Call `replay` before the real
/// transport and `capture` after it. This keeps the SDK free of a mandatory
/// reqwest/hyper dependency while preserving the universal capsule contract.
pub struct CausalTransport {
    network: Option<PathBuf>,
    action: Option<PathBuf>,
    capabilities: Option<PathBuf>,
    actor: String,
    replay: Option<Vec<serde_json::Value>>,
    used: std::collections::BTreeSet<usize>,
    prior_action: u32,
    ordinal: u32,
}

impl CausalTransport {
    pub fn from_env() -> Self {
        let capsule_path = std::env::var_os("REPROIT_CAPSULE").map(PathBuf::from);
        let replay = capsule_path.as_ref().and_then(|path| {
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            value.get("exchanges")?.as_array().cloned()
        });
        let mut out = Self {
            network: std::env::var_os("REPROIT_NETWORK_FILE").map(PathBuf::from),
            action: std::env::var_os("REPROIT_ACTION_FILE").map(PathBuf::from),
            capabilities: std::env::var_os("REPROIT_CAPABILITIES_FILE").map(PathBuf::from),
            actor: std::env::var("REPROIT_DEVICE").unwrap_or_else(|_| "a".into()),
            replay,
            used: Default::default(),
            prior_action: 0,
            ordinal: 0,
        };
        out.report_capabilities();
        out
    }

    pub fn active(&self) -> bool {
        self.network.is_some() || self.replay.is_some()
    }

    fn action_index(&self) -> u32 {
        self.action
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn replay(&mut self, method: &str, url: &str) -> Result<Option<ReplayResponse>, String> {
        let Some(exchanges) = &self.replay else {
            return Ok(None);
        };
        let action = self.action_index();
        for (index, exchange) in exchanges.iter().enumerate() {
            if self.used.contains(&index)
                || !exchange
                    .get("required")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                || exchange.get("actor").and_then(|v| v.as_str()) != Some(&self.actor)
                || exchange.get("actionIndex").and_then(|v| v.as_u64()) != Some(action as u64)
                || !exchange
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .eq_ignore_ascii_case(method)
                || exchange
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(canonical_causal_url)
                    != Some(canonical_causal_url(url))
            {
                continue;
            }
            self.used.insert(index);
            let status = exchange
                .get("status")
                .and_then(|v| v.as_u64())
                .unwrap_or(200) as u16;
            let headers = exchange
                .get("responseHeaders")
                .and_then(|v| v.as_object())
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            return Ok(Some((
                status,
                headers,
                exchange
                    .get("responseBody")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            )));
        }
        Err(format!("CAPSULE:MISS {method} {url} action={action}"))
    }

    #[allow(clippy::too_many_arguments)] // Mirrors one complete HTTP exchange.
    pub fn capture(
        &mut self,
        method: &str,
        url: &str,
        request_headers: BTreeMap<String, String>,
        request_body: Option<serde_json::Value>,
        status: u16,
        response_headers: BTreeMap<String, String>,
        response_body: Option<serde_json::Value>,
    ) {
        let Some(path) = &self.network else { return };
        let action = self.action_index();
        if action != self.prior_action {
            self.prior_action = action;
            self.ordinal = 0;
        }
        let ordinal = self.ordinal;
        self.ordinal += 1;
        let exchange = serde_json::json!({
            "id": format!("{}-{action}-{ordinal}", self.actor), "actor": self.actor,
            "actionIndex": action, "ordinal": ordinal,
            "protocol": url.split(':').next().unwrap_or("http"), "method": method,
            "url": canonical_causal_url(url),
            "requestHeaders": redact_headers_rs(request_headers),
            "requestBody": request_body.map(redact_rs), "status": status,
            "responseHeaders": redact_headers_rs(response_headers),
            "responseBody": response_body.map(redact_rs), "required": true,
        });
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{exchange}");
        }
    }

    fn report_capabilities(&mut self) {
        let Some(path) = &self.capabilities else {
            return;
        };
        let mut value: serde_json::Value = std::fs::read(path)
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        value["http"] =
            serde_json::json!({"status":"captured","detail":"Rust HTTP-library-neutral adapter"});
        value["http_replay"] =
            serde_json::json!({"status":"captured","detail":"Rust fail-closed adapter"});
        let _ = std::fs::write(path, value.to_string());
    }
}

fn secret_rs(key: &str) -> bool {
    let key: String = key
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| !matches!(ch, '-' | '_' | '.' | ' '))
        .collect();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "authorization",
        "cookie",
        "email",
        "phone",
        "apikey",
        "publishablekey",
        "privatekey",
        "accesskey",
        "signingkey",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}
fn redact_headers_rs(headers: BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .into_iter()
        .map(|(k, v)| {
            let safe = if secret_rs(&k) {
                "<reproit:secret>".into()
            } else {
                v
            };
            (k, safe)
        })
        .collect()
}
fn redact_rs(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let safe = if secret_rs(&k) {
                        serde_json::Value::String(match &v {
                            serde_json::Value::String(s) => {
                                format!("<reproit:string:length={}>", s.chars().count())
                            }
                            _ => "<reproit:secret>".into(),
                        })
                    } else {
                        redact_rs(v)
                    };
                    (k, safe)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(redact_rs).collect())
        }
        other => other,
    }
}

/// One rendered terminal screen: the contents string (what
/// `vt100::screen().contents()` would produce) plus the 0-based `(row, col)`
/// cursor. The cursor convention must match the runner, which reads
/// `screen().cursor_position()`.
#[derive(Clone, Debug, Default)]
pub struct ScreenContents {
    pub text: String,
    pub cursor: (u16, u16),
}

impl ScreenContents {
    /// Build from the already-rendered screen text.
    pub fn from_text(text: impl Into<String>, row: u16, col: u16) -> Self {
        Self {
            text: text.into(),
            cursor: (row, col),
        }
    }

    /// Build from per-row strings (join with newlines). Convenient for ratatui:
    /// read each `Buffer` row's symbols into a `String` and pass the slice
    /// here.
    pub fn from_rows<S: AsRef<str>>(rows: &[S], row: u16, col: u16) -> Self {
        let text = rows
            .iter()
            .map(|r| r.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            text,
            cursor: (row, col),
        }
    }

    /// The canonical structural signature (the state identity).
    pub fn structural_sig(&self) -> String {
        structural_sig(&self.text, self.cursor)
    }

    /// The runner-local content fingerprint (effect detection; never the
    /// identity).
    pub fn content_fingerprint(&self) -> String {
        content_fingerprint(&self.text, self.cursor)
    }
}

/// Where telemetry batches go. Implement this to wire your own HTTP client (the
/// batch is a complete `{appId, sentAt, ctx?, events}` JSON string). The
/// default [`SpoolTransport`] writes newline-delimited JSON to a file or
/// stderr; an HTTP transport is a few lines over any blocking client (see the
/// README).
pub trait Transport: Send + Sync {
    fn send(&self, batch_json: &str);
}

/// Default transport: append each batch as one line of JSON to a spool file, or
/// to stderr when no path is set. Lets an app run with zero networking wired.
pub struct SpoolTransport {
    path: Option<std::path::PathBuf>,
}

impl SpoolTransport {
    /// Spool batches to stderr.
    pub fn stderr() -> Self {
        Self { path: None }
    }
    /// Spool batches (newline-delimited JSON) to a file.
    pub fn file(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
        }
    }
}

impl Transport for SpoolTransport {
    fn send(&self, batch_json: &str) {
        match &self.path {
            Some(p) => {
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                {
                    let _ = writeln!(f, "{batch_json}");
                }
            }
            None => eprintln!("{batch_json}"),
        }
    }
}

/// SDK configuration. `ctx` is optional zero-PII context (release, build, etc.)
/// merged into every batch.
pub struct Config {
    pub app_id: String,
    pub ctx: Option<serde_json::Value>,
}

/// A registered app invariant: an `id` and a predicate that returns `Ok(())`
/// when it HOLDS or `Err(message)` when it is VIOLATED. A predicate that panics
/// is also a violation (message = the panic text), mirroring the web SDK's
/// "throws => violated" contract.
type Predicate = Box<dyn Fn() -> Result<(), String> + Send>;

struct Inner {
    app_id: String,
    ctx: Option<serde_json::Value>,
    transport: Box<dyn Transport>,
    last_sig: Option<String>,
    last_action: Option<String>,
    events: Vec<serde_json::Value>,
    /// App-declared invariants, keyed by id (registration is idempotent). Inert
    /// in production; evaluated only under the fuzzer (see
    /// [`report_invariants`]).
    invariants: Vec<(String, Predicate)>,
}

/// The SDK handle. Cheap to clone (shared inner state), so a clone can be moved
/// into a panic hook for crash flushing.
#[derive(Clone)]
pub struct ReproIt {
    inner: Arc<Mutex<Inner>>,
}

impl ReproIt {
    /// Create an SDK with an explicit transport. Use `SpoolTransport::stderr()`
    /// (or `::file(path)`) to start, or your own [`Transport`].
    pub fn new(config: Config, transport: Box<dyn Transport>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                app_id: config.app_id,
                ctx: config.ctx,
                transport,
                last_sig: None,
                last_action: None,
                events: Vec::new(),
                invariants: Vec::new(),
            })),
        }
    }

    /// Register an app invariant: a predicate the app declares that must hold
    /// in EVERY visited state. It returns `Ok(())` when it holds, or
    /// `Err(message)` (or panics) when it is violated. reproit's fuzzer
    /// evaluates every registered invariant on each state-observe and
    /// reports the failures as `invariant` findings; in production the
    /// registry is INERT (a plain store, never evaluated), so this is
    /// zero-overhead until a run reproduces it. Registration is idempotent
    /// by id, so re-registering an id replaces it.
    pub fn invariant<F>(&self, id: impl Into<String>, predicate: F) -> &Self
    where
        F: Fn() -> Result<(), String> + Send + 'static,
    {
        let id = id.into();
        let predicate: Predicate = Box::new(predicate);
        let mut inner = self.inner.lock().unwrap();
        if let Some(slot) = inner.invariants.iter_mut().find(|(k, _)| *k == id) {
            slot.1 = predicate;
        } else {
            inner.invariants.push((id, predicate));
        }
        self
    }

    /// Observe one rendered frame after the given action. The first frame
    /// records a `state` event; a frame whose structural signature differs
    /// from the last records an `edge` (from -> action -> to), mirroring
    /// the runner's coverage edges. Frames that do not change the signature
    /// are no-ops.
    pub fn observe(&self, screen: &ScreenContents, action: &str) {
        let sig = screen.structural_sig();
        let mut inner = self.inner.lock().unwrap();
        // App-invariant oracle (SDK-self-triggered): under the fuzzer, evaluate
        // the app's registered predicates against this state and report failures
        // on the diagnostic channel the TUI backend scrapes. No-op in production.
        report_invariants(&inner, &sig);
        match &inner.last_sig {
            None => inner
                .events
                .push(serde_json::json!({"kind": "state", "sig": sig})),
            Some(prev) if *prev != sig => {
                let from = prev.clone();
                inner.events.push(serde_json::json!({
                    "kind": "edge", "from": from, "action": action, "to": sig,
                }));
            }
            Some(_) => {}
        }
        inner.last_sig = Some(sig);
        inner.last_action = Some(action.to_string());
    }

    /// Record an error at the current state (with the path that led here) and
    /// flush immediately, so a production failure ships a replayable signature.
    pub fn record_error(&self, message: &str) {
        let mut inner = self.inner.lock().unwrap();
        let sig = inner.last_sig.clone().unwrap_or_default();
        let path = inner.last_action.clone();
        inner.events.push(serde_json::json!({
            "kind": "error", "sig": sig, "path": path, "message": message,
        }));
        Self::flush_locked(&mut inner);
    }

    /// Send any buffered events as one batch and clear the buffer.
    pub fn flush(&self) {
        let mut inner = self.inner.lock().unwrap();
        Self::flush_locked(&mut inner);
    }

    fn flush_locked(inner: &mut Inner) {
        if inner.events.is_empty() {
            return;
        }
        let mut batch = serde_json::json!({
            "appId": inner.app_id,
            "sentAt": now_millis(),
            "events": std::mem::take(&mut inner.events),
        });
        if let Some(ctx) = &inner.ctx {
            batch["ctx"] = ctx.clone();
        }
        inner.transport.send(&batch.to_string());
    }

    /// Install a panic hook that records the panic as an error event (with the
    /// current state + path) and flushes before the previous hook runs. This is
    /// the idiomatic Rust crash path; true fatal-signal handling (SIGSEGV) can
    /// be layered on with a signal crate if an app needs it.
    pub fn install_crash_handler(&self) {
        let me = self.clone();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            me.record_error(&format!("panic: {info}"));
            prev(info);
        }));
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The text of a caught panic payload (mirrors the "throws => violated"
/// message).
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        String::new()
    }
}

/// Evaluate every registered invariant and, ONLY under the fuzzer (the
/// `REPROIT_INVARIANT_FILE` env var the TUI backend sets is present and names a
/// file, which is also the fuzzer-detection gate), append one marker line
///   REPROIT_INVARIANT {"sig":"<sig>","items":[{"id","message"}...]}
/// listing the VIOLATED invariants to that file. The TUI backend scrapes the
/// file and re-emits each as `EXPLORE:INVARIANT`. Silent when the registry is
/// empty or every invariant held (no empty-items line). In production the env
/// var is unset, so this returns immediately and the registry is inert.
fn report_invariants(inner: &Inner, sig: &str) {
    let path = match std::env::var("REPROIT_INVARIANT_FILE") {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };
    if inner.invariants.is_empty() {
        return;
    }
    let mut items: Vec<serde_json::Value> = Vec::new();
    for (id, predicate) in &inner.invariants {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(predicate));
        let message = match outcome {
            Ok(Ok(())) => continue,                 // held
            Ok(Err(msg)) => msg,                    // violated with a message
            Err(payload) => panic_message(payload), // panicked => violated
        };
        items.push(serde_json::json!({ "id": id, "message": message }));
    }
    if items.is_empty() {
        return;
    }
    let line = serde_json::json!({ "sig": sig, "items": items });
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "REPROIT_INVARIANT {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // A transport that captures batches in memory for assertions.
    struct Capture(Arc<StdMutex<Vec<String>>>);
    impl Transport for Capture {
        fn send(&self, batch_json: &str) {
            self.0.lock().unwrap().push(batch_json.to_string());
        }
    }

    #[test]
    fn edge_recorded_only_on_signature_change() {
        let sink = Arc::new(StdMutex::new(Vec::new()));
        let sdk = ReproIt::new(
            Config {
                app_id: "t".into(),
                ctx: None,
            },
            Box::new(Capture(sink.clone())),
        );
        let a = ScreenContents::from_text("Count: 0\n", 0, 8);
        let b = ScreenContents::from_text("Count: 1\n", 0, 8);
        sdk.observe(&a, "start"); // state
        sdk.observe(&a, "noop"); // same sig -> nothing
        sdk.observe(&b, "key:Up"); // sig change -> edge
        sdk.flush();
        let batches = sink.lock().unwrap();
        assert_eq!(batches.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&batches[0]).unwrap();
        let events = v["events"].as_array().unwrap();
        assert_eq!(
            events.len(),
            2,
            "one state + one edge, no event for the no-op"
        );
        assert_eq!(events[0]["kind"], "state");
        assert_eq!(events[1]["kind"], "edge");
        assert_eq!(events[1]["action"], "key:Up");
    }

    #[test]
    fn invariant_reports_only_violations_under_the_fuzzer() {
        // Point the SDK at a scratch marker file (this is also the fuzzer gate).
        let path =
            std::env::temp_dir().join(format!("reproit-tui-inv-{}.ndjson", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::env::set_var("REPROIT_INVARIANT_FILE", &path);

        let sdk = ReproIt::new(
            Config {
                app_id: "t".into(),
                ctx: None,
            },
            Box::new(SpoolTransport::stderr()),
        );
        // One invariant holds, one is violated (with a message), one panics.
        sdk.invariant("stays-ok", || Ok(()));
        sdk.invariant("went-negative", || Err("balance < 0".to_string()));
        sdk.invariant("throwing", || panic!("kaboom"));

        let screen = ScreenContents::from_text("Balance: -5\n", 0, 0);
        sdk.observe(&screen, "key:Down");

        let logged = std::fs::read_to_string(&path).unwrap_or_default();
        // The marker carries the SDK's own sig and ONLY the two failures; the
        // holding invariant never appears (silent when it holds).
        assert!(
            logged.contains("REPROIT_INVARIANT "),
            "a marker was written"
        );
        let json_part = logged
            .lines()
            .find(|l| l.starts_with("REPROIT_INVARIANT "))
            .and_then(|l| l.strip_prefix("REPROIT_INVARIANT "))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(v["sig"], screen.structural_sig());
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "only the two violations are reported");
        let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"went-negative") && ids.contains(&"throwing"));
        assert!(!ids.contains(&"stays-ok"));

        // Clean direction: when every registered invariant holds, no marker line
        // is appended (the file stays at its current size).
        let before = std::fs::read_to_string(&path).unwrap_or_default();
        let clean = ReproIt::new(
            Config {
                app_id: "t".into(),
                ctx: None,
            },
            Box::new(SpoolTransport::stderr()),
        );
        clean.invariant("stays-ok", || Ok(()));
        clean.observe(&ScreenContents::from_text("all good\n", 0, 0), "load");
        let after = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(before, after, "a satisfied invariant writes nothing");

        std::env::remove_var("REPROIT_INVARIANT_FILE");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn error_carries_current_sig_and_path() {
        let sink = Arc::new(StdMutex::new(Vec::new()));
        let sdk = ReproIt::new(
            Config {
                app_id: "t".into(),
                ctx: None,
            },
            Box::new(Capture(sink.clone())),
        );
        let s = ScreenContents::from_text("boom screen\n", 0, 0);
        sdk.observe(&s, "tap:crash");
        sdk.record_error("kaboom");
        let batches = sink.lock().unwrap();
        let v: serde_json::Value = serde_json::from_str(batches.last().unwrap()).unwrap();
        let err = v["events"].as_array().unwrap().last().unwrap();
        assert_eq!(err["kind"], "error");
        assert_eq!(err["message"], "kaboom");
        assert_eq!(err["sig"], s.structural_sig());
        assert_eq!(err["path"], "tap:crash");
    }

    #[test]
    fn causal_transport_uses_side_files_and_redacts() {
        let dir = std::env::temp_dir().join(format!("reproit-tui-rs-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let network = dir.join("network.ndjson");
        let action = dir.join("action.txt");
        let capabilities = dir.join("capabilities.json");
        std::fs::write(&network, "").unwrap();
        std::fs::write(&action, "4").unwrap();
        std::fs::write(&capabilities, "{}").unwrap();
        std::env::set_var("REPROIT_NETWORK_FILE", &network);
        std::env::set_var("REPROIT_ACTION_FILE", &action);
        std::env::set_var("REPROIT_CAPABILITIES_FILE", &capabilities);
        std::env::set_var("REPROIT_DEVICE", "b");
        let mut causal = CausalTransport::from_env();
        causal.capture(
            "POST",
            "https://app.test/feed",
            BTreeMap::from([("authorization".into(), "raw".into())]),
            Some(serde_json::json!({"token":"raw"})),
            200,
            BTreeMap::from([("content-type".into(), "application/json".into())]),
            Some(serde_json::json!({"profile":{"email":"a@example.com"},"ok":true})),
        );
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&network).unwrap()).unwrap();
        assert_eq!(value["actor"], "b");
        assert_eq!(value["actionIndex"], 4);
        assert_eq!(value["requestHeaders"]["authorization"], "<reproit:secret>");
        assert_eq!(value["requestBody"]["token"], "<reproit:string:length=3>");
        assert_eq!(
            value["responseBody"]["profile"]["email"],
            "<reproit:string:length=13>"
        );
        let capsule = dir.join("capsule.json");
        std::fs::write(
            &capsule,
            serde_json::json!({"exchanges":[{
                "id":"b-4-0", "actor":"b", "actionIndex":4, "ordinal":0,
                "method":"GET", "url":"https://app.test/config?a=1&b=2",
                "status":200, "responseHeaders":{"content-type":"application/json"},
                "responseBody":{"ok":true}, "required":true
            }]})
            .to_string(),
        )
        .unwrap();
        std::env::set_var("REPROIT_CAPSULE", &capsule);
        let mut replay = CausalTransport::from_env();
        assert_eq!(
            replay
                .replay("GET", "https://app.test/config?b=2&a=1")
                .unwrap()
                .unwrap()
                .0,
            200
        );
        assert!(replay
            .replay("GET", "https://app.test/miss")
            .unwrap_err()
            .contains("CAPSULE:MISS"));
        for key in [
            "REPROIT_NETWORK_FILE",
            "REPROIT_ACTION_FILE",
            "REPROIT_CAPABILITIES_FILE",
            "REPROIT_DEVICE",
            "REPROIT_CAPSULE",
        ] {
            std::env::remove_var(key);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn explicit_secret_keys_redact_without_hiding_ordinary_keys() {
        let safe = redact_rs(serde_json::json!({
            "apiKey":"raw-api", "publishable-key":"raw-pub", "private_key":"raw-private",
            "access.key":"raw-access", "signing key":"raw-signing",
            "keyboardLayout":"dvorak", "key":"ordinary"
        }));
        assert_eq!(safe["keyboardLayout"], "dvorak");
        assert_eq!(safe["key"], "ordinary");
        let encoded = safe.to_string();
        for raw in [
            "raw-api",
            "raw-pub",
            "raw-private",
            "raw-access",
            "raw-signing",
        ] {
            assert!(!encoded.contains(raw), "raw secret survived: {raw}");
        }
    }
}
