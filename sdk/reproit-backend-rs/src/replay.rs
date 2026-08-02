//! Hermetic replay mode (feature `instrument`).
//!
//! Rust port of the Node SDK's `replay.js`, the wire reference. When
//! `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same
//! boundaries that record exchanges at capture time SERVE them instead:
//! outbound HTTP is answered in process and the database boundary returns
//! recorded results, so the application re-executes against exactly what
//! production saw, with no live dependencies.
//!
//! Determinism is a contract, not a similarity score. Matching is strict
//! per-operation ordinals: within one operation (method plus path for HTTP,
//! statement text for pg) exchanges are consumed in recorded order, so
//! pooled clients and LLM tool-call loops that interleave operations still
//! match exactly. Recorded `$reproit` redaction placeholders match any value
//! at their position; nothing else is tolerated. The first unmatched call is
//! a DIVERGENCE: a structured `REPROIT:DIVERGENCE` stderr line (with a
//! `bodyDelta` naming WHERE the bodies differ; chat-shaped bodies name the
//! first differing message index) and a hard 599 (HTTP) or error (db).
//!
//! The marker line is BYTE-identical to the Node reference: field insertion
//! order and compact separators, which is why this module runs on the
//! order-preserving [`crate::ojson`] values rather than serde_json's sorted
//! maps.
//!
//! The envelope pins the replay's determinism: `TZ` from the capture, the
//! process clock offset to the capture moment (via [`now_millis`]), and the
//! seeded RNG stream (via `instrument::replay_rng`). Honesty note: the seed
//! makes REPLAY runs deterministic; it does not reproduce the randomness the
//! app drew in production.

use crate::ojson::{self, scalar_eq, OValue};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

/// The structured divergence marker, byte-identical to the Node SDK's.
pub const DIVERGENCE_MARKER: &str = "REPROIT:DIVERGENCE ";
const CAPTURE_FORMAT: &str = "reproit-backend-capture";

struct Entry {
    exchange: OValue,
    consumed: bool,
}

pub struct ReplaySession {
    /// The capture envelope (serde view; key order is irrelevant here).
    pub envelope: serde_json::Value,
    exchanges: Mutex<Vec<Entry>>,
    /// Marker lines emitted by this session, for tests and the parity probe.
    markers: Mutex<Vec<String>>,
}

impl ReplaySession {
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("read REPROIT_REPLAY file {path}: {error}"))?;
        Self::from_text(&text)
    }

    pub fn from_text(text: &str) -> Result<Self, String> {
        let payload: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| format!("REPROIT_REPLAY file is not JSON: {error}"))?;
        if payload.get("format").and_then(serde_json::Value::as_str) != Some(CAPTURE_FORMAT) {
            return Err("REPROIT_REPLAY file is not a reproit-backend-capture payload".into());
        }
        let version = payload
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if !(1..=2).contains(&version) {
            return Err(format!("unsupported capture version {version}"));
        }
        // The ordered parse feeds the matcher and the marker, so recorded
        // request objects re-serialize in their original key order exactly
        // as Node's JSON.parse/JSON.stringify pair does.
        let ordered =
            ojson::parse(text).ok_or_else(|| "REPROIT_REPLAY file is not JSON".to_string())?;
        let exchanges = ordered
            .get("events")
            .and_then(OValue::as_arr)
            .map(|events| {
                events
                    .iter()
                    .filter(|event| event.get("kind").and_then(OValue::as_str) == Some("effect"))
                    .filter_map(|event| event.get("exchange"))
                    .filter(|exchange| matches!(exchange, OValue::Obj(_)))
                    .map(|exchange| Entry {
                        exchange: exchange.clone(),
                        consumed: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            envelope: payload
                .get("envelope")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            exchanges: Mutex::new(exchanges),
            markers: Mutex::new(Vec::new()),
        })
    }

    /// Strict per-operation ordinal match. Returns the exchange or `None`
    /// (divergence, already reported on stderr).
    pub fn match_exchange(&self, protocol: &str, probe: &OValue) -> Option<OValue> {
        let key = operation_key(protocol, probe);
        let mut entries = lock(&self.exchanges);
        for entry in entries.iter_mut() {
            if entry.consumed
                || entry.exchange.get("protocol").and_then(OValue::as_str) != Some(protocol)
            {
                continue;
            }
            let request = entry
                .exchange
                .get("request")
                .cloned()
                .unwrap_or(empty_obj());
            if operation_key(protocol, &request) != key {
                continue;
            }
            let hit = if protocol == "http" {
                http_request_matches(&request, probe)
            } else {
                pg_request_matches(&request, probe)
            };
            if hit {
                entry.consumed = true;
                return Some(entry.exchange.clone());
            }
            // Strict ordinal within an operation: the next unconsumed
            // exchange of THIS operation is the only candidate; skipping it
            // silently would be a fuzzy match. Other operations' exchanges
            // may interleave (pg pooling, tool-call loops), which is why the
            // key filters above.
            break;
        }
        drop(entries);
        self.diverge(protocol, probe);
        None
    }

    /// Report one divergence: the structured marker line on stderr, field
    /// order and separators byte-identical to the Node reference.
    pub fn diverge(&self, protocol: &str, probe: &OValue) {
        let key = operation_key(protocol, probe);
        let entries = lock(&self.exchanges);
        let candidates: Vec<&Entry> = entries
            .iter()
            .filter(|entry| {
                !entry.consumed
                    && entry.exchange.get("protocol").and_then(OValue::as_str) == Some(protocol)
            })
            .collect();
        let expected = candidates
            .iter()
            .find(|entry| {
                let request = entry
                    .exchange
                    .get("request")
                    .cloned()
                    .unwrap_or(empty_obj());
                operation_key(protocol, &request) == key
            })
            .or_else(|| candidates.first())
            .map(|entry| {
                entry
                    .exchange
                    .get("request")
                    .cloned()
                    .unwrap_or(OValue::Null)
            });
        let consumed = entries.iter().filter(|entry| entry.consumed).count() as u64;
        let total = entries.len() as u64;
        drop(entries);
        let mut report = vec![
            ("protocol".to_string(), OValue::Str(protocol.to_string())),
            ("got".to_string(), probe.clone()),
            (
                "expected".to_string(),
                expected.clone().unwrap_or(OValue::Null),
            ),
            ("consumed".to_string(), OValue::num(consumed)),
            ("total".to_string(), OValue::num(total)),
        ];
        // Prompt drift: when the recorded and live bodies both exist and
        // differ, name WHERE they differ. Chat-shaped bodies (OpenAI or
        // Anthropic messages arrays) name the first differing message index;
        // unknown shapes fall back to the byte offset of the first differing
        // byte.
        let delta = expected
            .as_ref()
            .and_then(|request| body_delta(request.get("body"), probe.get("body")));
        if let Some(delta) = delta {
            report.push(("bodyDelta".to_string(), delta));
        }
        let line = format!("{DIVERGENCE_MARKER}{}", OValue::Obj(report).to_compact());
        eprintln!("{line}");
        lock(&self.markers).push(line);
    }

    /// Marker lines this session has emitted (tests and the parity probe).
    pub fn markers(&self) -> Vec<String> {
        lock(&self.markers).clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn empty_obj() -> OValue {
    OValue::Obj(Vec::new())
}

/// One operation's identity for ordinal matching: HTTP is method plus
/// path+query, pg is the exact statement text.
pub fn operation_key(protocol: &str, request: &OValue) -> String {
    if protocol == "http" {
        let method = request.get("method").and_then(OValue::as_str).unwrap_or("");
        let url = request.get("url").and_then(OValue::as_str).unwrap_or("");
        format!("{method} {}", url_path_and_query(url))
    } else {
        request
            .get("text")
            .and_then(OValue::as_str)
            .unwrap_or("")
            .to_string()
    }
}

/// The messages array of an OpenAI/Anthropic-shaped chat body, else `None`.
fn chat_messages(body: &OValue) -> Option<&[OValue]> {
    match body {
        OValue::Obj(_) => body.get("messages").and_then(OValue::as_arr),
        _ => None,
    }
}

fn delta_bytes(value: &OValue) -> Vec<u8> {
    match value {
        OValue::Str(text) => text.as_bytes().to_vec(),
        other => other.to_compact().into_bytes(),
    }
}

/// Locate the first difference between a recorded request body and a live
/// one, modulo redaction placeholders. `None` when there is nothing to
/// report: an ABSENT body on either side (absence is not `null`; a recorded
/// `null` body still compares), or no difference the matcher objects to.
pub fn body_delta(recorded: Option<&OValue>, live: Option<&OValue>) -> Option<OValue> {
    let (recorded, live) = (recorded?, live?);
    if matches(recorded, Some(live)) {
        return None;
    }
    if let (Some(recorded_messages), Some(live_messages)) =
        (chat_messages(recorded), chat_messages(live))
    {
        let bound = recorded_messages.len().min(live_messages.len());
        let mut index =
            (0..bound).find(|&i| !matches(&recorded_messages[i], Some(&live_messages[i])));
        // All shared indexes match: the drift is a longer/shorter
        // conversation, and the first differing message is the first
        // unshared one. If lengths also agree the drift is outside
        // `messages`; fall through to bytes.
        if index.is_none() && recorded_messages.len() != live_messages.len() {
            index = Some(bound);
        }
        if let Some(index) = index {
            return Some(OValue::Obj(vec![
                ("kind".to_string(), OValue::Str("message".to_string())),
                (
                    "firstDifferingMessage".to_string(),
                    OValue::num(index as u64),
                ),
                (
                    "recordedMessages".to_string(),
                    OValue::num(recorded_messages.len() as u64),
                ),
                (
                    "liveMessages".to_string(),
                    OValue::num(live_messages.len() as u64),
                ),
            ]));
        }
    }
    let recorded_bytes = delta_bytes(recorded);
    let live_bytes = delta_bytes(live);
    let bound = recorded_bytes.len().min(live_bytes.len());
    let offset = (0..bound)
        .find(|&i| recorded_bytes[i] != live_bytes[i])
        .unwrap_or(bound);
    Some(OValue::Obj(vec![
        ("kind".to_string(), OValue::Str("byte".to_string())),
        ("offset".to_string(), OValue::num(offset as u64)),
    ]))
}

/// A recorded value matches a live one when equal, or when the recorded side
/// is a `$reproit` redaction placeholder (any value stood here at capture).
/// Objects compare per key; a recorded null/absent side matches anything.
pub fn matches(recorded: &OValue, live: Option<&OValue>) -> bool {
    match recorded {
        OValue::Null => true,
        OValue::Obj(fields) => {
            if recorded.get("$reproit").is_some() {
                return true;
            }
            let Some(live @ OValue::Obj(_)) = live else {
                return false;
            };
            fields
                .iter()
                .all(|(key, value)| matches(value, live.get(key)))
        }
        OValue::Arr(items) => {
            let Some(OValue::Arr(live)) = live else {
                return false;
            };
            items.len() == live.len()
                && items
                    .iter()
                    .zip(live)
                    .all(|(recorded, live)| matches(recorded, Some(live)))
        }
        scalar => live.is_some_and(|live| scalar_eq(scalar, live)),
    }
}

/// Path plus query of a URL, the host-independent identity replay matches
/// on (a replayed app dials a different origin than production did).
pub fn url_path_and_query(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[scheme_end + 3..];
    let rest = rest.split('#').next().unwrap_or(rest);
    match rest.find(['/', '?']) {
        Some(at) if rest.as_bytes()[at] == b'?' => format!("/{}", &rest[at..]),
        Some(at) => rest[at..].to_string(),
        None => "/".to_string(),
    }
}

/// Method, path+query of the original URL, and body modulo placeholders.
/// Recorded headers are deliberately not matched: they carry per-run noise
/// (dates, connection management) that would turn every replay into a
/// divergence.
pub fn http_request_matches(recorded: &OValue, probe: &OValue) -> bool {
    let method = |request: &OValue| {
        request
            .get("method")
            .and_then(OValue::as_str)
            .unwrap_or("")
            .to_string()
    };
    if method(recorded) != method(probe) {
        return false;
    }
    let url = |request: &OValue| {
        url_path_and_query(request.get("url").and_then(OValue::as_str).unwrap_or(""))
    };
    if url(recorded) != url(probe) {
        return false;
    }
    match recorded.get("body") {
        None => true,
        Some(body) => matches(body, probe.get("body")),
    }
}

/// Exact statement text, values modulo placeholders.
pub fn pg_request_matches(recorded: &OValue, probe: &OValue) -> bool {
    if recorded.get("text").and_then(OValue::as_str) != probe.get("text").and_then(OValue::as_str) {
        return false;
    }
    match recorded.get("values") {
        None => true,
        Some(values) => matches(values, probe.get("values")),
    }
}

/// The uniform served response: status, headers, body text, and (for a
/// recorded stream shape) the body split at the recorded chunk boundaries.
pub struct Served {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_text: String,
    pub chunks: Option<Vec<Vec<u8>>>,
}

fn diverged_599(reason: &str) -> Served {
    Served {
        status: 599,
        headers: vec![("content-type".to_string(), "application/json".to_string())],
        body_text: format!("{{\"reproit\":\"{reason}\"}}"),
        chunks: None,
    }
}

/// Append one marker field to a probe, mirroring Node's `{...probe, k: true}`.
fn probe_with_flag(probe: &OValue, flag: &str) -> OValue {
    let mut fields = match probe {
        OValue::Obj(fields) => fields.clone(),
        _ => Vec::new(),
    };
    fields.push((flag.to_string(), OValue::Bool(true)));
    OValue::Obj(fields)
}

/// Resolve a live HTTP probe against the session, entirely in process. A
/// divergence and a truncated-at-capture body (or stream shape) both serve
/// a hard 599 so the application observes an attributable failure instead
/// of a guess.
pub fn serve_http(session: &ReplaySession, probe: &OValue) -> Served {
    let Some(recorded) = session.match_exchange("http", probe) else {
        return diverged_599("diverged");
    };
    let response = recorded.get("response").cloned().unwrap_or(empty_obj());
    if response.get("truncated").and_then(OValue::as_bool) == Some(true) {
        // The capture kept identity but not bytes; serving a guessed body
        // would be a silent lie. Fail closed with the named reason.
        session.diverge("http", &probe_with_flag(probe, "truncated"));
        return diverged_599("truncated-exchange-body");
    }
    let headers: Vec<(String, String)> = match response.get("headers") {
        Some(OValue::Obj(fields)) => fields
            .iter()
            .filter(|(name, _)| {
                !matches!(
                    name.as_str(),
                    "content-length" | "transfer-encoding" | "content-encoding"
                )
            })
            .filter_map(|(name, value)| {
                value
                    .as_str()
                    .map(|value| (name.clone(), value.to_string()))
            })
            .collect(),
        _ => Vec::new(),
    };
    let body_text = match response.get("body") {
        None => String::new(),
        Some(OValue::Str(text)) => text.clone(),
        Some(other) => other.to_compact(),
    };
    let status = response
        .get("status")
        .and_then(OValue::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or(200);
    let mut served = Served {
        status,
        headers,
        body_text,
        chunks: None,
    };
    if let Some(stream) = response.get("stream") {
        if let Some(lengths) = stream.get("chunks").and_then(OValue::as_arr) {
            if stream.get("truncated").and_then(OValue::as_bool) == Some(true) {
                // The capture kept the body but not every chunk boundary;
                // serving a guessed stream shape would be a silent lie.
                session.diverge("http", &probe_with_flag(probe, "streamBoundariesTruncated"));
                return diverged_599("truncated-stream-boundaries");
            }
            served.chunks = Some(split_chunks(&served.body_text, lengths));
        }
    }
    served
}

/// Split a replayed body at the recorded chunk boundaries (byte lengths).
/// Redaction can change body byte counts, so lengths are clamped and the
/// last chunk absorbs any remainder: the CHUNK COUNT (the stream shape the
/// app observed) is preserved exactly, the recorded content never padded.
pub fn split_chunks(body_text: &str, lengths: &[OValue]) -> Vec<Vec<u8>> {
    let bytes = body_text.as_bytes();
    let mut chunks = Vec::with_capacity(lengths.len());
    let mut offset = 0usize;
    for (index, length) in lengths.iter().enumerate() {
        let last = index == lengths.len() - 1;
        let size = length.as_u64().unwrap_or(0) as usize;
        let end = if last {
            bytes.len()
        } else {
            (offset + size).min(bytes.len())
        };
        chunks.push(bytes[offset..end].to_vec());
        offset = end;
    }
    chunks
}

/// Millisecond offset applied to [`now_millis`] in replay mode; 0 outside.
static CLOCK_OFFSET_MS: AtomicI64 = AtomicI64::new(0);

fn real_now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// The process clock through the determinism shim: real time in capture
/// mode, offset to the capture moment in replay mode (the analogue of the
/// Node reference offsetting `Date.now`). Named limitation: Rust has no
/// monkeypatching, so application code calling `SystemTime::now` directly
/// reads the real clock; only reads routed through this shim are pinned.
pub fn now_millis() -> u64 {
    (real_now_millis() + CLOCK_OFFSET_MS.load(Ordering::Relaxed)).max(0) as u64
}

/// Pin process determinism from the capture envelope: `TZ` (named
/// limitation: the std library does not consult TZ; chrono-style formatters
/// do) and the [`now_millis`] clock offset. Runs once at session load.
pub fn pin_envelope(envelope: &serde_json::Value) {
    if let Some(tz) = envelope.get("tz").and_then(serde_json::Value::as_str) {
        if !tz.is_empty() {
            std::env::set_var("TZ", tz);
        }
    }
    if let Some(observed_at) = envelope
        .get("observedAtMs")
        .and_then(serde_json::Value::as_u64)
    {
        CLOCK_OFFSET_MS.store(observed_at as i64 - real_now_millis(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(events: &str) -> ReplaySession {
        let text =
            format!("{{\"format\":\"reproit-backend-capture\",\"version\":2,\"events\":{events}}}");
        ReplaySession::from_text(&text).expect("session")
    }

    #[test]
    fn interleaved_operations_match_per_operation_ordinals() {
        let session = session(
            r#"[
            {"kind":"effect","exchange":{"protocol":"pg",
              "request":{"text":"SELECT a"},"response":{"rows":[{"n":1}]}}},
            {"kind":"effect","exchange":{"protocol":"pg",
              "request":{"text":"SELECT b"},"response":{"rows":[{"n":2}]}}},
            {"kind":"effect","exchange":{"protocol":"pg",
              "request":{"text":"SELECT a"},"response":{"rows":[{"n":3}]}}}
            ]"#,
        );
        let probe =
            |text: &str| OValue::Obj(vec![("text".to_string(), OValue::Str(text.to_string()))]);
        // Consuming out of recorded order across operations is fine; within
        // one operation the ordinals hold.
        let hit = session.match_exchange("pg", &probe("SELECT b")).unwrap();
        assert!(hit.to_compact().contains("\"n\":2"));
        let hit = session.match_exchange("pg", &probe("SELECT a")).unwrap();
        assert!(hit.to_compact().contains("\"n\":1"));
        let hit = session.match_exchange("pg", &probe("SELECT a")).unwrap();
        assert!(hit.to_compact().contains("\"n\":3"));
    }

    #[test]
    fn divergence_marker_is_byte_identical_to_the_node_reference() {
        // Expected line generated by sdk/reproit-backend-node/replay.js over
        // this exact capsule and probe; the parity suite re-proves it live.
        let session = session(
            r#"[{"kind":"effect","exchange":{"protocol":"http",
                 "request":{"method":"GET","url":"http://svc/prices"}}}]"#,
        );
        let probe = OValue::Obj(vec![
            ("method".to_string(), OValue::Str("GET".to_string())),
            (
                "url".to_string(),
                OValue::Str("http://svc/unknown".to_string()),
            ),
        ]);
        assert!(session.match_exchange("http", &probe).is_none());
        let marker = session.markers().pop().expect("marker");
        assert_eq!(
            marker,
            "REPROIT:DIVERGENCE {\"protocol\":\"http\",\
             \"got\":{\"method\":\"GET\",\"url\":\"http://svc/unknown\"},\
             \"expected\":{\"method\":\"GET\",\"url\":\"http://svc/prices\"},\
             \"consumed\":0,\"total\":1}"
        );
    }

    #[test]
    fn prompt_drift_names_the_first_differing_message() {
        let recorded = ojson::parse(
            r#"{"messages":[{"role":"user","content":"hello"},
                {"role":"assistant","content":"hi"},
                {"role":"user","content":"weather?"}]}"#,
        )
        .unwrap();
        let live = ojson::parse(
            r#"{"messages":[{"role":"user","content":"hello"},
                {"role":"assistant","content":"hi"},
                {"role":"user","content":"DIFFERENT"}]}"#,
        )
        .unwrap();
        let delta = body_delta(Some(&recorded), Some(&live)).expect("delta");
        assert_eq!(
            delta.to_compact(),
            "{\"kind\":\"message\",\"firstDifferingMessage\":2,\
             \"recordedMessages\":3,\"liveMessages\":3}"
        );
        // ABSENT is not null: a missing live body reports nothing, while a
        // recorded null body matches anything (no delta either way, but for
        // different reasons the matcher distinguishes).
        assert!(body_delta(Some(&recorded), None).is_none());
        assert!(body_delta(Some(&OValue::Null), Some(&live)).is_none());
        // Unknown shapes fall back to the first differing byte offset.
        let a = ojson::parse(r#"{"q":"abcd"}"#).unwrap();
        let b = ojson::parse(r#"{"q":"abXd"}"#).unwrap();
        let delta = body_delta(Some(&a), Some(&b)).expect("delta");
        assert_eq!(delta.to_compact(), "{\"kind\":\"byte\",\"offset\":8}");
    }

    #[test]
    fn stream_serves_chunk_for_chunk_and_truncated_boundaries_fail_closed() {
        let session = session(
            r#"[
            {"kind":"effect","exchange":{"protocol":"http",
              "request":{"method":"GET","url":"http://llm/stream"},
              "response":{"status":200,"headers":{"content-type":"text/event-stream"},
                "body":"data: a\n\ndata: b\n\ndata: c\n\n","stream":{"chunks":[9,9,9]}}}},
            {"kind":"effect","exchange":{"protocol":"http",
              "request":{"method":"GET","url":"http://llm/cut"},
              "response":{"status":200,"body":"xyz",
                "stream":{"chunks":[1,1],"truncated":true}}}}
            ]"#,
        );
        let probe = |url: &str| {
            OValue::Obj(vec![
                ("method".to_string(), OValue::Str("GET".to_string())),
                ("url".to_string(), OValue::Str(url.to_string())),
            ])
        };
        let served = serve_http(&session, &probe("http://llm/stream"));
        assert_eq!(served.status, 200);
        let chunks = served.chunks.expect("chunks");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], b"data: a\n\n".to_vec());
        assert_eq!(chunks[2], b"data: c\n\n".to_vec());
        let served = serve_http(&session, &probe("http://llm/cut"));
        assert_eq!(served.status, 599);
        assert_eq!(
            served.body_text,
            "{\"reproit\":\"truncated-stream-boundaries\"}"
        );
        let marker = session.markers().pop().expect("marker");
        assert!(marker.contains("\"streamBoundariesTruncated\":true"));
    }

    #[test]
    fn truncated_recorded_body_fails_closed() {
        let session = session(
            r#"[{"kind":"effect","exchange":{"protocol":"http",
                 "request":{"method":"GET","url":"http://svc/blob"},
                 "response":{"status":200,"bodyBytes":40000,
                   "bodySha256":"aa","truncated":true}}}]"#,
        );
        let probe = OValue::Obj(vec![
            ("method".to_string(), OValue::Str("GET".to_string())),
            (
                "url".to_string(),
                OValue::Str("http://svc/blob".to_string()),
            ),
        ]);
        let served = serve_http(&session, &probe);
        assert_eq!(served.status, 599);
        assert_eq!(
            served.body_text,
            "{\"reproit\":\"truncated-exchange-body\"}"
        );
    }

    #[test]
    fn invalid_payloads_fail_closed() {
        assert!(ReplaySession::from_text("{}").is_err());
        assert!(ReplaySession::from_text(
            "{\"format\":\"reproit-backend-capture\",\"version\":99}"
        )
        .is_err());
    }
}
