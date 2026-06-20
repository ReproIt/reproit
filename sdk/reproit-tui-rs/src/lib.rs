//! reproit-tui: production telemetry SDK for Rust terminal-UI apps.
//!
//! Drop this into a real TUI app (ratatui, crossterm-direct, or any renderer) and
//! it reports the SAME canonical TUI screen signature the `reproit __tui` runner
//! computes, so a production crash reported here carries a state signature the
//! runner can replay locally. Each distinct screen is a state; each screen change
//! is a coverage edge; a panic ships the path that led to it.
//!
//! Parity is by CONSTRUCTION, not by a port: the signature comes from the shared
//! `reproit-tui-sig` crate that the runner itself uses. The Go/TS/Python TUI SDKs
//! port that logic and are pinned to the same golden vectors; this crate links it
//! directly, so it cannot drift from the runner.
//!
//! Capture: a TUI app renders its own cells, so you hand the SDK the rendered
//! screen. ratatui apps build a `ratatui::buffer::Buffer` (convert its cells to a
//! contents string with `ScreenContents::from_rows`); crossterm-direct apps pass
//! the text they drew. The SDK never reads the terminal itself.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

// Re-export the shared signature surface so SDK users never reach past this crate.
pub use reproit_tui_sig::{content_fingerprint, labels_of, sig_of, structural_sig, value_class};

/// One rendered terminal screen: the contents string (what `vt100::screen().contents()`
/// would produce) plus the 0-based `(row, col)` cursor. The cursor convention must
/// match the runner, which reads `screen().cursor_position()`.
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
    /// read each `Buffer` row's symbols into a `String` and pass the slice here.
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

    /// The runner-local content fingerprint (effect detection; never the identity).
    pub fn content_fingerprint(&self) -> String {
        content_fingerprint(&self.text, self.cursor)
    }
}

/// Where telemetry batches go. Implement this to wire your own HTTP client (the
/// batch is a complete `{appId, sentAt, ctx?, events}` JSON string). The default
/// [`SpoolTransport`] writes newline-delimited JSON to a file or stderr; an HTTP
/// transport is a few lines over any blocking client (see the README).
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

struct Inner {
    app_id: String,
    ctx: Option<serde_json::Value>,
    transport: Box<dyn Transport>,
    last_sig: Option<String>,
    last_action: Option<String>,
    events: Vec<serde_json::Value>,
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
            })),
        }
    }

    /// Observe one rendered frame after the given action. The first frame records
    /// a `state` event; a frame whose structural signature differs from the last
    /// records an `edge` (from -> action -> to), mirroring the runner's coverage
    /// edges. Frames that do not change the signature are no-ops.
    pub fn observe(&self, screen: &ScreenContents, action: &str) {
        let sig = screen.structural_sig();
        let mut inner = self.inner.lock().unwrap();
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
    /// the idiomatic Rust crash path; true fatal-signal handling (SIGSEGV) can be
    /// layered on with a signal crate if an app needs it.
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
}
