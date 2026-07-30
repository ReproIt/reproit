//! Outbound-exchange capture and hermetic replay (feature `instrument`).
//!
//! Rust port of the Node SDK's `instrument.js` + `replay.js`. Rust has no
//! monkeypatching, so the boundary is explicit and OPT-IN: route outbound
//! HTTP through [`http::send`] and database calls through [`db::run`], and
//! every dependency exchange (request AND response) is recorded onto the
//! ambient request trace, bounded and redacted at source. With the
//! `REPROIT_REPLAY` environment variable naming a `reproit-backend-capture`
//! payload, the SAME entry points serve the recorded exchanges instead:
//! strict per-protocol ordinal matching, `$reproit` redaction placeholders
//! match any value, a truncated-at-capture body fails closed, and the first
//! unmatched call emits a structured `REPROIT:DIVERGENCE` stderr line and
//! answers 599 (HTTP) or an error (db). No live dependency is touched in
//! replay mode.
//!
//! The capture envelope pins replay determinism: `TZ` is set from the
//! capture, and [`replay_rng`] yields the seeded stream. Honesty note: the
//! seed makes REPLAY runs deterministic; it does not reproduce the
//! randomness the app drew in production.

use crate::framework::Recorder;
use crate::EffectKind;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::sync::{Mutex, OnceLock};

/// Inline body budget per exchange side. Beyond it the body is dropped and
/// only provable identity (byte count + sha256) remains.
pub const MAX_EXCHANGE_BODY_BYTES: usize = 8 * 1024;
/// Recorded response headers are capped to keep events bounded.
const MAX_EXCHANGE_HEADERS: usize = 32;
/// Rows recorded per db result; beyond it the result is marked truncated.
const MAX_DB_ROWS: usize = 64;
/// The structured divergence marker, byte-identical to the Node SDK's.
pub const DIVERGENCE_MARKER: &str = "REPROIT:DIVERGENCE ";

tokio::task_local! {
    static AMBIENT: Recorder;
}

/// Run `future` with `recorder` as the ambient trace for [`http::send`] and
/// [`db::run`]. The framework middleware scopes the handler automatically;
/// call this directly only for hand-rolled servers.
pub async fn scope<F: Future>(recorder: Recorder, future: F) -> F::Output {
    AMBIENT.scope(recorder, future).await
}

fn ambient() -> Option<Recorder> {
    AMBIENT.try_with(Clone::clone).ok()
}

/// Load the replay session (when `REPROIT_REPLAY` is set) and pin the
/// process envelope. Idempotent; the first [`http::send`] or [`db::run`]
/// triggers it lazily, but calling it from `main` pins `TZ` before any
/// time-zone-sensitive code runs.
pub fn init() {
    let _ = session();
}

/// True when this process is serving a recorded capture instead of touching
/// live dependencies.
pub fn replaying() -> bool {
    session().is_some()
}

fn session() -> Option<&'static ReplaySession> {
    static SESSION: OnceLock<Option<ReplaySession>> = OnceLock::new();
    SESSION
        .get_or_init(|| {
            let path = std::env::var("REPROIT_REPLAY").ok()?;
            if path.trim().is_empty() {
                return None;
            }
            let session = ReplaySession::load(&path)?;
            session.pin_envelope();
            Some(session)
        })
        .as_ref()
}

/// Deterministic xorshift64* stream from the capture's `replaySeed`. `None`
/// outside replay mode or when the capture carries no envelope.
pub fn replay_rng() -> Option<ReplayRng> {
    let seed = session()?
        .envelope
        .get("replaySeed")
        .and_then(Value::as_str)?
        .to_string();
    let hex: String = seed.chars().take(16).collect();
    let state = u64::from_str_radix(&hex, 16).ok()? | 1;
    Some(ReplayRng { state })
}

pub struct ReplayRng {
    state: u64,
}

impl ReplayRng {
    /// The next draw in [0, 1), matching the Node SDK's stream shape.
    pub fn next_f64(&mut self) -> f64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        let mixed = self.state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        ((mixed >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

struct ExchangeEntry {
    exchange: Value,
    consumed: bool,
}

struct ReplaySession {
    envelope: Value,
    exchanges: Mutex<Vec<ExchangeEntry>>,
}

impl ReplaySession {
    fn load(path: &str) -> Option<Self> {
        let payload: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        if payload.get("format").and_then(Value::as_str) != Some("reproit-backend-capture") {
            return None;
        }
        let version = payload.get("version").and_then(Value::as_u64).unwrap_or(0);
        if !(1..=2).contains(&version) {
            return None;
        }
        let exchanges = payload
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter(|event| event.get("kind").and_then(Value::as_str) == Some("effect"))
                    .filter_map(|event| event.get("exchange"))
                    .filter(|exchange| !exchange.is_null())
                    .map(|exchange| ExchangeEntry {
                        exchange: exchange.clone(),
                        consumed: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            envelope: payload.get("envelope").cloned().unwrap_or(Value::Null),
            exchanges: Mutex::new(exchanges),
        })
    }

    fn pin_envelope(&self) {
        if let Some(tz) = self.envelope.get("tz").and_then(Value::as_str) {
            if !tz.is_empty() {
                std::env::set_var("TZ", tz);
            }
        }
    }

    /// Strict next-unconsumed match: the first unconsumed exchange of the
    /// protocol is the ONLY candidate; skipping it silently would be a fuzzy
    /// match. `None` is a divergence, already reported.
    fn matched(&self, protocol: &str, probe: &Value) -> Option<Value> {
        let mut entries = self
            .exchanges
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for entry in entries.iter_mut() {
            if entry.consumed
                || entry.exchange.get("protocol").and_then(Value::as_str) != Some(protocol)
            {
                continue;
            }
            let request = entry
                .exchange
                .get("request")
                .cloned()
                .unwrap_or(Value::Null);
            let hit = match protocol {
                "http" => http_request_matches(&request, probe),
                _ => db_request_matches(&request, probe),
            };
            if hit {
                entry.consumed = true;
                return Some(entry.exchange.clone());
            }
            break;
        }
        let expected = entries
            .iter()
            .find(|entry| {
                !entry.consumed
                    && entry.exchange.get("protocol").and_then(Value::as_str) == Some(protocol)
            })
            .map(|entry| {
                entry
                    .exchange
                    .get("request")
                    .cloned()
                    .unwrap_or(Value::Null)
            });
        let consumed = entries.iter().filter(|entry| entry.consumed).count();
        let total = entries.len();
        drop(entries);
        self.diverge(protocol, probe, expected, consumed, total);
        None
    }

    fn diverge(
        &self,
        protocol: &str,
        probe: &Value,
        expected: Option<Value>,
        consumed: usize,
        total: usize,
    ) {
        let report = json!({
            "protocol": protocol,
            "got": probe,
            "expected": expected.unwrap_or(Value::Null),
            "consumed": consumed,
            "total": total,
        });
        eprintln!("{DIVERGENCE_MARKER}{report}");
    }
}

/// A recorded value matches a live one when equal, or when the recorded side
/// is a `$reproit` redaction placeholder (any value stood here at capture).
/// Objects compare per key; a recorded null/absent side matches anything.
fn matches(recorded: &Value, live: Option<&Value>) -> bool {
    match recorded {
        Value::Null => true,
        Value::Object(object) => {
            if object.contains_key("$reproit") {
                return true;
            }
            let Some(Value::Object(live)) = live else {
                return false;
            };
            object
                .iter()
                .all(|(key, value)| matches(value, live.get(key)))
        }
        Value::Array(items) => {
            let Some(Value::Array(live)) = live else {
                return false;
            };
            items.len() == live.len()
                && items
                    .iter()
                    .zip(live)
                    .all(|(recorded, live)| matches(recorded, Some(live)))
        }
        value => live == Some(value),
    }
}

fn url_path_and_query(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(parsed) => match parsed.query() {
            Some(query) => format!("{}?{query}", parsed.path()),
            None => parsed.path().to_string(),
        },
        Err(_) => url.to_string(),
    }
}

/// Method, path+query of the original URL, and body modulo placeholders.
/// Recorded headers are deliberately not matched: they carry per-run noise.
fn http_request_matches(recorded: &Value, probe: &Value) -> bool {
    if recorded.get("method") != probe.get("method") {
        return false;
    }
    let recorded_url = recorded.get("url").and_then(Value::as_str).unwrap_or("");
    let probe_url = probe.get("url").and_then(Value::as_str).unwrap_or("");
    if url_path_and_query(recorded_url) != url_path_and_query(probe_url) {
        return false;
    }
    match recorded.get("body") {
        None => true,
        Some(body) => matches(body, probe.get("body")),
    }
}

/// Exact statement text, values modulo placeholders.
fn db_request_matches(recorded: &Value, probe: &Value) -> bool {
    if recorded.get("text") != probe.get("text") {
        return false;
    }
    match recorded.get("values") {
        None => true,
        Some(values) => matches(values, probe.get("values")),
    }
}

/// Bound one exchange body: within budget it is recorded verbatim (JSON
/// parsed when declared), beyond it only byte count + sha256 + truncated.
fn bounded_body(body: &[u8], content_type: &str) -> Map<String, Value> {
    let mut fields = Map::new();
    if body.is_empty() {
        return fields;
    }
    if body.len() > MAX_EXCHANGE_BODY_BYTES {
        let digest = Sha256::digest(body);
        fields.insert("bodyBytes".into(), json!(body.len()));
        fields.insert(
            "bodySha256".into(),
            json!(digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()),
        );
        fields.insert("truncated".into(), json!(true));
        return fields;
    }
    let text = String::from_utf8_lossy(body);
    if content_type.contains("application/json") {
        if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
            fields.insert("body".into(), parsed);
            return fields;
        }
    }
    fields.insert("body".into(), json!(text));
    fields
}

fn bounded_headers(headers: impl Iterator<Item = (String, String)>) -> Map<String, Value> {
    let mut fields = Map::new();
    let map: Map<String, Value> = headers
        .take(MAX_EXCHANGE_HEADERS)
        .map(|(name, value)| (name.to_ascii_lowercase(), json!(value)))
        .collect();
    if !map.is_empty() {
        fields.insert("headers".into(), Value::Object(map));
    }
    fields
}

/// Outbound HTTP through the exchange boundary.
pub mod http {
    use super::*;

    /// The uniform response both modes produce: capture mode buffers the
    /// live response, replay mode synthesizes it from the recording. 599
    /// with a `{"reproit": ...}` body is a divergence, never a guess.
    #[derive(Debug)]
    pub struct ExchangeResponse {
        pub status: u16,
        pub headers: std::collections::BTreeMap<String, String>,
        pub body: Vec<u8>,
    }

    impl ExchangeResponse {
        pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
            serde_json::from_slice(&self.body)
        }

        pub fn text(&self) -> String {
            String::from_utf8_lossy(&self.body).into_owned()
        }
    }

    fn diverged_599(reason: &str) -> ExchangeResponse {
        ExchangeResponse {
            status: 599,
            headers: std::collections::BTreeMap::from([(
                "content-type".into(),
                "application/json".into(),
            )]),
            body: serde_json::to_vec(&json!({"reproit": reason})).unwrap_or_default(),
        }
    }

    /// Send `request` through the exchange boundary. Capture mode executes
    /// it and records request+response onto the ambient trace; replay mode
    /// serves the recorded exchange with no network at all.
    pub async fn send(
        client: &reqwest::Client,
        request: reqwest::Request,
    ) -> Result<ExchangeResponse, reqwest::Error> {
        let method = request.method().as_str().to_string();
        let url = request.url().to_string();
        let content_type = request
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let request_body = request
            .body()
            .and_then(|body| body.as_bytes())
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        if let Some(session) = session() {
            let mut probe =
                Map::from_iter([("method".into(), json!(method)), ("url".into(), json!(url))]);
            probe.extend(bounded_body(&request_body, &content_type));
            let probe = Value::Object(probe);
            let Some(recorded) = session.matched("http", &probe) else {
                return Ok(diverged_599("diverged"));
            };
            let response = recorded.get("response").cloned().unwrap_or(Value::Null);
            if response.get("truncated").and_then(Value::as_bool) == Some(true) {
                // The capture kept identity but not bytes; serving a guessed
                // body would be a silent lie. Fail closed with the reason.
                session.diverge("http", &probe, Some(recorded.clone()), 0, 0);
                return Ok(diverged_599("truncated-exchange-body"));
            }
            let status = response
                .get("status")
                .and_then(Value::as_u64)
                .and_then(|status| u16::try_from(status).ok())
                .unwrap_or(200);
            let mut headers = std::collections::BTreeMap::new();
            if let Some(Value::Object(recorded_headers)) = response.get("headers") {
                for (name, value) in recorded_headers {
                    if matches!(
                        name.as_str(),
                        "content-length" | "transfer-encoding" | "content-encoding"
                    ) {
                        continue;
                    }
                    if let Some(value) = value.as_str() {
                        headers.insert(name.clone(), value.to_string());
                    }
                }
            }
            let body = match response.get("body") {
                None => Vec::new(),
                Some(Value::String(text)) => text.clone().into_bytes(),
                Some(other) => serde_json::to_vec(other).unwrap_or_default(),
            };
            return Ok(ExchangeResponse {
                status,
                headers,
                body,
            });
        }
        let response = client.execute(request).await?;
        let status = response.status().as_u16();
        let response_content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let header_pairs: Vec<(String, String)> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
            })
            .collect();
        let body = response.bytes().await?.to_vec();
        if let Some(recorder) = ambient() {
            let host = reqwest::Url::parse(&url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_string))
                .unwrap_or_default();
            let mut request_value =
                Map::from_iter([("method".into(), json!(method)), ("url".into(), json!(url))]);
            request_value.extend(bounded_body(&request_body, &content_type));
            let mut response_value = Map::from_iter([("status".into(), json!(status))]);
            response_value.extend(bounded_headers(header_pairs.iter().cloned()));
            response_value.extend(bounded_body(&body, &response_content_type));
            // The trace may already have finished; the host request goes on.
            let _ = recorder.exchange(
                EffectKind::Call,
                Some(&host),
                Some(&format!("{method} {}", url_path_and_query(&url))),
                json!({
                    "protocol": "http",
                    "request": Value::Object(request_value),
                    "response": Value::Object(response_value),
                }),
            );
        }
        Ok(ExchangeResponse {
            status,
            headers: header_pairs.into_iter().collect(),
            body,
        })
    }
}

/// Database calls through the exchange boundary. Rust has no driver to
/// monkeypatch, so the app routes each statement through [`run`]; anything
/// not routed here is invisible to capture and unavailable at replay.
pub mod db {
    use super::*;

    #[derive(Debug, Clone, Default)]
    pub struct DbOutcome {
        pub command: Option<String>,
        pub row_count: u64,
        pub rows: Vec<Value>,
    }

    #[derive(Debug, Clone)]
    pub struct DbError {
        pub message: String,
        pub code: Option<String>,
    }

    impl std::fmt::Display for DbError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "{}", self.message)
        }
    }

    impl std::error::Error for DbError {}

    fn effect_kind(text: &str) -> EffectKind {
        let verb: String = text.trim_start().chars().take(8).collect();
        let verb = verb.to_ascii_uppercase();
        if verb.starts_with("SELECT") || verb.starts_with("SHOW") {
            EffectKind::Read
        } else {
            EffectKind::Write
        }
    }

    fn outcome_value(result: &Result<DbOutcome, DbError>) -> Value {
        match result {
            Ok(outcome) => {
                let mut rows = outcome.rows.clone();
                let truncated = rows.len() > MAX_DB_ROWS;
                rows.truncate(MAX_DB_ROWS);
                let mut value = Map::from_iter([
                    ("command".into(), json!(outcome.command)),
                    ("rowCount".into(), json!(outcome.row_count)),
                    ("rows".into(), Value::Array(rows)),
                ]);
                if truncated {
                    value.insert("truncated".into(), json!(true));
                }
                Value::Object(value)
            }
            Err(error) => json!({
                "error": { "message": error.message, "code": error.code },
            }),
        }
    }

    /// Run one statement through the boundary: replay mode serves the
    /// recorded outcome without calling `live`; capture mode awaits `live`
    /// and records the exchange either way it settles.
    pub async fn run<F, Fut>(text: &str, values: &[Value], live: F) -> Result<DbOutcome, DbError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<DbOutcome, DbError>>,
    {
        if let Some(session) = session() {
            let mut probe = Map::from_iter([("text".into(), json!(text))]);
            if !values.is_empty() {
                probe.insert("values".into(), json!(values));
            }
            let probe = Value::Object(probe);
            let Some(recorded) = session.matched("pg", &probe) else {
                return Err(DbError {
                    message: "reproit: db call diverged from the capture".into(),
                    code: None,
                });
            };
            let response = recorded.get("response").cloned().unwrap_or(Value::Null);
            if let Some(error) = response.get("error") {
                return Err(DbError {
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("recorded db error")
                        .to_string(),
                    code: error
                        .get("code")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
            return Ok(DbOutcome {
                command: response
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                row_count: response
                    .get("rowCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                rows: response
                    .get("rows")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        let result = live().await;
        if let Some(recorder) = ambient() {
            let mut request = Map::from_iter([("text".into(), json!(text))]);
            if !values.is_empty() {
                request.insert("values".into(), json!(values));
            }
            let _ = recorder.exchange(
                effect_kind(text),
                Some("pg"),
                Some(&text.chars().take(256).collect::<String>()),
                json!({
                    "protocol": "pg",
                    "request": Value::Object(request),
                    "response": outcome_value(&result),
                }),
            );
        }
        result
    }
}
