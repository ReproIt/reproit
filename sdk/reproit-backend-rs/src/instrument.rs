//! Outbound-exchange capture and hermetic replay (feature `instrument`).
//!
//! Rust port of the Node SDK's `instrument.js` + `replay.js`. Rust has no
//! monkeypatching, so the boundary is explicit and OPT-IN: route outbound
//! HTTP through [`http::send`] (buffered) or [`http::send_stream`] (a TEE:
//! chunks reach the app live and the exchange records at end of body),
//! database calls through [`db::run`] or the `pg` feature's tokio-postgres
//! wrapper, and every dependency exchange (request line+headers+body AND
//! response status+headers+body, stream chunk boundaries included) is
//! recorded onto the ambient request trace, bounded and redacted at source.
//!
//! With `REPROIT_REPLAY` naming a `reproit-backend-capture` payload, the
//! SAME entry points serve the recorded exchanges instead: strict
//! per-operation ordinal matching, `$reproit` placeholders match any value,
//! truncated-at-capture bodies and stream shapes fail closed, and the first
//! unmatched call emits the structured `REPROIT:DIVERGENCE` stderr line
//! (byte-identical to Node's, `bodyDelta` included) and answers 599 (HTTP)
//! or an error (db). No live dependency is touched in replay mode; see
//! `replay.rs` for the matcher and the envelope pins.
//!
//! The capture envelope pins replay determinism: `TZ` is set from the
//! capture, [`now_millis`] offsets to the capture moment, and [`replay_rng`]
//! yields the seeded stream. Honesty notes: the seed makes REPLAY runs
//! deterministic, it does not reproduce the randomness the app drew in
//! production; and Rust cannot intercept direct `SystemTime::now` /
//! `rand::random` calls, so only reads routed through the shim are pinned.

use crate::framework::Recorder;
use crate::ojson::OValue;
use crate::replay::{self, ReplaySession};
use crate::EffectKind;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Inline body budget per exchange side. Beyond it the body is dropped and
/// only provable identity (byte count + sha256) remains.
pub const MAX_EXCHANGE_BODY_BYTES: usize = 8 * 1024;
/// Recorded headers are capped to keep events bounded. The cap is defined
/// over NAME SORTED order (see [`bounded_headers`]).
pub const MAX_EXCHANGE_HEADERS: usize = 32;
/// Rows recorded per db result; beyond it the result is marked truncated.
pub const MAX_DB_ROWS: usize = 64;
/// Stream chunk boundaries recorded per exchange (SSE / chunked responses,
/// the LLM streaming shape). Beyond it the boundaries are marked truncated
/// and replay fails closed rather than serve a wrong stream shape.
pub const MAX_STREAM_CHUNKS: usize = 128;
/// The structured divergence marker, byte-identical to the Node SDK's.
pub const DIVERGENCE_MARKER: &str = replay::DIVERGENCE_MARKER;

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

/// Capture/replay counters, the Node reference's `stats()`. The failed
/// count is the drop counter for exchanges a finished or full trace (the
/// per-trace event cap) could not accept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstrumentStats {
    pub captured_exchanges: u64,
    pub truncated_bodies: u64,
    pub failed_captures: u64,
}

static CAPTURED: AtomicU64 = AtomicU64::new(0);
static TRUNCATED: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);

pub fn stats() -> InstrumentStats {
    InstrumentStats {
        captured_exchanges: CAPTURED.load(Ordering::Relaxed),
        truncated_bodies: TRUNCATED.load(Ordering::Relaxed),
        failed_captures: FAILED.load(Ordering::Relaxed),
    }
}

/// Load the replay session (when `REPROIT_REPLAY` is set) and pin the
/// process envelope. Idempotent; the first [`http::send`] or [`db::run`]
/// triggers it lazily, but calling it from `main` pins `TZ` and the clock
/// before any time-sensitive code runs.
pub fn init() {
    let _ = session();
}

/// True when this process is serving a recorded capture instead of touching
/// live dependencies.
pub fn replaying() -> bool {
    session().is_some()
}

/// The process clock through the determinism shim: real in capture mode,
/// pinned to the capture moment in replay mode. See `replay::now_millis`.
pub fn now_millis() -> u64 {
    replay::now_millis()
}

fn session() -> Option<&'static ReplaySession> {
    static SESSION: OnceLock<Option<ReplaySession>> = OnceLock::new();
    SESSION
        .get_or_init(|| {
            let path = std::env::var("REPROIT_REPLAY").ok()?;
            if path.trim().is_empty() {
                return None;
            }
            // Fail CLOSED: a replay run whose capture cannot load must never
            // silently fall back to dialing live dependencies.
            let session = ReplaySession::load(&path)
                .unwrap_or_else(|error| panic!("reproit replay refused: {error}"));
            replay::pin_envelope(&session.envelope);
            Some(session)
        })
        .as_ref()
}

/// Deterministic xorshift64* stream from the capture's `replaySeed`. `None`
/// outside replay mode or when the capture carries no envelope. Named gap:
/// this pins randomness the app draws THROUGH the SDK; direct `rand` /
/// `getrandom` calls in application code cannot be intercepted in Rust.
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

/// Bound one exchange body: within budget it is recorded verbatim (JSON
/// parsed when declared), beyond it only byte count + sha256 + truncated.
/// The digest runs over EVERY byte so truncated identity stays provable.
pub fn bounded_body(body: &[u8], content_type: &str) -> Map<String, Value> {
    let mut fields = Map::new();
    if body.is_empty() {
        return fields;
    }
    if body.len() > MAX_EXCHANGE_BODY_BYTES {
        TRUNCATED.fetch_add(1, Ordering::Relaxed);
        let digest = Sha256::digest(body);
        fields.insert("bodyBytes".into(), json!(body.len()));
        fields.insert("bodySha256".into(), json!(hex(&digest)));
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The cap is defined over NAME SORTED order, never the order the headers
/// arrived in: Go capped a randomized map first and recorded a different
/// subset each run, so the same request produced two capsules that disagreed.
pub fn bounded_headers(headers: impl Iterator<Item = (String, String)>) -> Map<String, Value> {
    let mut fields = Map::new();
    let mut lowered: Vec<(String, String)> = headers
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect();
    lowered.sort_by(|left, right| left.0.cmp(&right.0));
    let map: Map<String, Value> = lowered
        .into_iter()
        .take(MAX_EXCHANGE_HEADERS)
        .map(|(name, value)| (name, json!(value)))
        .collect();
    if !map.is_empty() {
        fields.insert("headers".into(), Value::Object(map));
    }
    fields
}

/// Collect a stream's chunks up to one byte past the inline budget; enough
/// to know the true size class without holding unbounded memory. The sha256
/// runs over EVERY byte. Chunk boundaries are recorded as observed byte
/// lengths, bounded by [`MAX_STREAM_CHUNKS`]; boundaries past the cap are
/// counted, never guessed. Port of the Node reference's `bodyCollector`.
struct BodyCollector {
    chunks: Vec<Vec<u8>>,
    boundaries: Vec<usize>,
    bytes: usize,
    dropped_boundaries: usize,
    hash: Sha256,
}

impl BodyCollector {
    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            boundaries: Vec::new(),
            bytes: 0,
            dropped_boundaries: 0,
            hash: Sha256::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.bytes += chunk.len();
        self.hash.update(chunk);
        if self.boundaries.len() < MAX_STREAM_CHUNKS {
            self.boundaries.push(chunk.len());
        } else {
            self.dropped_boundaries += 1;
        }
        if self.bytes <= MAX_EXCHANGE_BODY_BYTES {
            self.chunks.push(chunk.to_vec());
        }
    }

    /// The recorded body fields: empty, inline verbatim, or provable
    /// identity (byte count + digest + truncated) when over budget.
    fn body_fields(self, content_type: &str) -> Map<String, Value> {
        if self.bytes == 0 {
            return Map::new();
        }
        if self.bytes > MAX_EXCHANGE_BODY_BYTES {
            TRUNCATED.fetch_add(1, Ordering::Relaxed);
            let mut fields = Map::new();
            fields.insert("bodyBytes".into(), json!(self.bytes));
            fields.insert("bodySha256".into(), json!(hex(&self.hash.finalize())));
            fields.insert("truncated".into(), json!(true));
            return fields;
        }
        bounded_body(&self.chunks.concat(), content_type)
    }

    /// Chunk boundaries as observed byte lengths. Recorded when the response
    /// is a stream (SSE always; anything else only when it actually arrived
    /// in more than one chunk, since a single-chunk body replays identically
    /// without them). Boundaries past the cap are counted, never guessed.
    fn stream(&self, is_event_stream: bool) -> Option<Value> {
        if self.boundaries.is_empty() {
            return None;
        }
        if !is_event_stream && self.boundaries.len() < 2 && self.dropped_boundaries == 0 {
            return None;
        }
        let mut fields = Map::new();
        fields.insert("chunks".into(), json!(self.boundaries));
        if self.dropped_boundaries > 0 {
            fields.insert("truncated".into(), json!(true));
        }
        Some(Value::Object(fields))
    }
}

/// Outbound HTTP through the exchange boundary.
pub mod http {
    use super::*;

    /// The request identity carried from send to record time.
    struct RequestMeta {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        content_type: String,
    }

    fn request_meta(request: &reqwest::Request) -> RequestMeta {
        RequestMeta {
            method: request.method().as_str().to_string(),
            url: request.url().to_string(),
            headers: request
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
                })
                .collect(),
            body: request
                .body()
                .and_then(|body| body.as_bytes())
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
            content_type: request
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string(),
        }
    }

    /// The live probe for one request, in the ordered shape the matcher and
    /// the divergence marker want (field order mirrors the Node reference).
    fn probe_of(meta: &RequestMeta) -> OValue {
        let mut fields = vec![
            ("method".to_string(), OValue::Str(meta.method.clone())),
            ("url".to_string(), OValue::Str(meta.url.clone())),
        ];
        if !meta.body.is_empty() {
            let text = String::from_utf8_lossy(&meta.body).into_owned();
            let body = if meta.content_type.contains("application/json") {
                crate::ojson::parse(&text).unwrap_or(OValue::Str(text))
            } else {
                OValue::Str(text)
            };
            fields.push(("body".to_string(), body));
        }
        OValue::Obj(fields)
    }

    fn record_exchange(meta: &RequestMeta, response_value: Map<String, Value>) {
        let Some(recorder) = ambient() else {
            return;
        };
        let host = reqwest::Url::parse(&meta.url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string))
            .unwrap_or_default();
        let mut request_value = Map::from_iter([
            ("method".into(), json!(meta.method)),
            ("url".into(), json!(meta.url)),
        ]);
        request_value.extend(bounded_headers(meta.headers.iter().cloned()));
        request_value.extend(bounded_body(&meta.body, &meta.content_type));
        let key = format!("{} {}", meta.method, replay::url_path_and_query(&meta.url));
        // The trace may already have finished or hit its per-trace event
        // cap; the host request goes on and the drop is counted.
        let outcome = recorder.exchange(
            EffectKind::Call,
            Some(&host),
            Some(&key),
            json!({
                "protocol": "http",
                "request": Value::Object(request_value),
                "response": Value::Object(response_value),
            }),
        );
        match outcome {
            Ok(()) => CAPTURED.fetch_add(1, Ordering::Relaxed),
            Err(_) => FAILED.fetch_add(1, Ordering::Relaxed),
        };
    }

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

    /// Send `request` through the exchange boundary, buffered. Capture mode
    /// executes it and records request+response (headers, body, observed
    /// chunk boundaries) onto the ambient trace; replay mode serves the
    /// recorded exchange with no network at all.
    pub async fn send(
        client: &reqwest::Client,
        request: reqwest::Request,
    ) -> Result<ExchangeResponse, reqwest::Error> {
        let meta = request_meta(&request);
        if let Some(session) = session() {
            let served = replay::serve_http(session, &probe_of(&meta));
            return Ok(ExchangeResponse {
                status: served.status,
                headers: served.headers.into_iter().collect(),
                body: served.body_text.into_bytes(),
            });
        }
        let mut response = client.execute(request).await?;
        let status = response.status().as_u16();
        let content_type = response
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
        // The network chunks tee through the collector (boundaries + digest)
        // while the full body is buffered for the caller.
        let mut collector = BodyCollector::new();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            collector.push(&chunk);
            body.extend_from_slice(&chunk);
        }
        let stream = collector.stream(content_type.contains("text/event-stream"));
        let mut response_value = Map::from_iter([("status".into(), json!(status))]);
        response_value.extend(bounded_headers(header_pairs.iter().cloned()));
        let body_fields = collector.body_fields(&content_type);
        let truncated = body_fields.get("truncated") == Some(&json!(true));
        response_value.extend(body_fields);
        // A truncated inline body already fails closed at replay, so
        // boundaries are only kept for bodies recorded verbatim.
        if let (Some(stream), false) = (stream, truncated) {
            response_value.insert("stream".into(), stream);
        }
        record_exchange(&meta, response_value);
        Ok(ExchangeResponse {
            status,
            headers: header_pairs.into_iter().collect(),
            body,
        })
    }

    /// A streamed exchange: chunks reach the app as they arrive (a TEE, not
    /// a drain), and the exchange records at end of body. An abandoned
    /// stream (dropped before EOF) records nothing, exactly like a response
    /// nobody reads. In replay mode the recorded stream shape is re-served
    /// chunk for chunk.
    pub struct ExchangeStream {
        pub status: u16,
        pub headers: std::collections::BTreeMap<String, String>,
        state: StreamState,
    }

    enum StreamState {
        Live(Box<LiveStream>),
        Served {
            chunks: std::collections::VecDeque<Vec<u8>>,
        },
    }

    struct LiveStream {
        response: reqwest::Response,
        collector: Option<BodyCollector>,
        meta: RequestMeta,
        content_type: String,
        header_pairs: Vec<(String, String)>,
    }

    impl ExchangeStream {
        /// The next body chunk, `None` at end of stream (which is the moment
        /// the exchange lands on the trace in capture mode).
        pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, reqwest::Error> {
            match &mut self.state {
                StreamState::Served { chunks } => Ok(chunks.pop_front()),
                StreamState::Live(live) => {
                    if let Some(chunk) = live.response.chunk().await? {
                        if let Some(collector) = live.collector.as_mut() {
                            collector.push(&chunk);
                        }
                        return Ok(Some(chunk.to_vec()));
                    }
                    // End of body: record exactly once.
                    if let Some(collector) = live.collector.take() {
                        let status = live.response.status().as_u16();
                        let stream =
                            collector.stream(live.content_type.contains("text/event-stream"));
                        let mut response_value = Map::from_iter([("status".into(), json!(status))]);
                        response_value.extend(bounded_headers(live.header_pairs.iter().cloned()));
                        let body_fields = collector.body_fields(&live.content_type);
                        let truncated = body_fields.get("truncated") == Some(&json!(true));
                        response_value.extend(body_fields);
                        if let (Some(stream), false) = (stream, truncated) {
                            response_value.insert("stream".into(), stream);
                        }
                        record_exchange(&live.meta, response_value);
                    }
                    Ok(None)
                }
            }
        }
    }

    /// Send `request` through the exchange boundary as a stream (the LLM
    /// SSE shape). See [`ExchangeStream`].
    pub async fn send_stream(
        client: &reqwest::Client,
        request: reqwest::Request,
    ) -> Result<ExchangeStream, reqwest::Error> {
        let meta = request_meta(&request);
        if let Some(session) = session() {
            let served = replay::serve_http(session, &probe_of(&meta));
            let chunks = served
                .chunks
                .unwrap_or_else(|| vec![served.body_text.into_bytes()]);
            return Ok(ExchangeStream {
                status: served.status,
                headers: served.headers.into_iter().collect(),
                state: StreamState::Served {
                    chunks: chunks.into(),
                },
            });
        }
        let response = client.execute(request).await?;
        let status = response.status().as_u16();
        let content_type = response
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
        Ok(ExchangeStream {
            status,
            headers: header_pairs.iter().cloned().collect(),
            state: StreamState::Live(Box::new(LiveStream {
                response,
                collector: Some(BodyCollector::new()),
                meta,
                content_type,
                header_pairs,
            })),
        })
    }
}

/// Database calls through the exchange boundary. Rust has no driver to
/// monkeypatch, so the app routes each statement through [`run`] (or the
/// `pg` feature's tokio-postgres wrapper, which routes here); anything not
/// routed is invisible to capture and unavailable at replay.
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

    /// The live probe in the ordered shape the matcher and marker want.
    fn probe_of(text: &str, values: &[Value]) -> OValue {
        let mut fields = vec![("text".to_string(), OValue::Str(text.to_string()))];
        if !values.is_empty() {
            fields.push((
                "values".to_string(),
                crate::ojson::parse(&serde_json::to_string(values).unwrap_or_default())
                    .unwrap_or(OValue::Arr(Vec::new())),
            ));
        }
        OValue::Obj(fields)
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
            let probe = probe_of(text, values);
            let Some(recorded) = session.match_exchange("pg", &probe) else {
                return Err(DbError {
                    message: "reproit: db call diverged from the capture".into(),
                    code: None,
                });
            };
            let response = recorded
                .get("response")
                .map(OValue::to_serde)
                .unwrap_or(Value::Null);
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
            let outcome = recorder.exchange(
                effect_kind(text),
                Some("pg"),
                Some(&text.chars().take(256).collect::<String>()),
                json!({
                    "protocol": "pg",
                    "request": Value::Object(request),
                    "response": outcome_value(&result),
                }),
            );
            match outcome {
                Ok(()) => CAPTURED.fetch_add(1, Ordering::Relaxed),
                Err(_) => FAILED.fetch_add(1, Ordering::Relaxed),
            };
        }
        result
    }
}
