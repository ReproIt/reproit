//! Shared support for the feature-gated framework middleware (axum, actix).
//!
//! Scan-time: inert unless the request carries `x-reproit-trace`; the
//! finished trace is returned as the `x-reproit-events` response header.
//! Production: pass a `Capture` and every request is traced and handed to
//! the sampler instead. Handlers record observed effects via the [`Recorder`]
//! stored in the request extensions. Every adapter path fails closed:
//! instrumentation errors never reach the host app.
//!
//! Bodies are buffered only when their exact size is known and within a
//! fixed cap, so the start/return events carry the decoded JSON payloads;
//! larger, streaming, or non-JSON bodies are traced without content.

use crate::{BackendTrace, Capture, EffectKind, TraceContext, TraceError};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Bounded request/response body buffering for the canonical input/output.
pub(crate) const MAX_BODY_BYTES: usize = 64 * 1024;

/// Handle to the request's trace, shared between the middleware and the
/// handler through the request extensions. Cheap to clone.
#[derive(Clone)]
pub struct Recorder {
    trace: Arc<Mutex<Option<BackendTrace>>>,
}

impl Recorder {
    pub(crate) fn new(trace: BackendTrace) -> Self {
        Self {
            trace: Arc::new(Mutex::new(Some(trace))),
        }
    }

    /// Record one observed effect on the in-flight trace. Fails once the
    /// middleware has finished the trace (response already left).
    pub fn effect(
        &self,
        effect: EffectKind,
        resource: Option<&str>,
        key: Option<&str>,
        tenant: Option<&str>,
        event: Option<&str>,
        detail: Option<Value>,
    ) -> Result<(), TraceError> {
        let mut guard = self
            .trace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.as_mut() {
            Some(trace) => trace.effect(effect, resource, key, tenant, event, detail),
            None => Err(TraceError::AlreadyFinished),
        }
    }

    /// The middleware takes the trace back exactly once to finish it.
    pub(crate) fn take(&self) -> Option<BackendTrace> {
        self.trace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

/// The trace begun for one request, plus whether it came from a scan-time
/// `x-reproit-trace` header (versus a synthesized capture context).
pub(crate) struct PendingTrace {
    pub recorder: Recorder,
    pub scan: bool,
}

/// Resolve the request's trace context: the trusted scan header wins, the
/// capture sampler substitutes one, otherwise the middleware stays inert.
pub(crate) fn resolve_context(
    mut get_header: impl FnMut(&str) -> Option<String>,
    capture: Option<&Capture>,
) -> Option<(TraceContext, bool)> {
    if let Some(context) = TraceContext::from_header_fn(&mut get_header) {
        return Some((context, true));
    }
    capture.map(|capture| (capture.context(), false))
}

pub(crate) fn decode_json(body: &[u8], content_type: &str) -> Option<Value> {
    if body.is_empty() || !content_type.contains("application/json") {
        return None;
    }
    serde_json::from_slice(body).ok()
}

/// Fold repeated keys into arrays, matching the canonical HttpInput shape.
pub(crate) fn multi_map(pairs: impl Iterator<Item = (String, String)>) -> BTreeMap<String, Value> {
    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    for (key, value) in pairs {
        match fields.get_mut(&key) {
            None => {
                fields.insert(key, Value::String(value));
            }
            Some(Value::Array(items)) => items.push(Value::String(value)),
            Some(prior) => {
                let first = prior.take();
                *prior = Value::Array(vec![first, Value::String(value)]);
            }
        }
    }
    fields
}

/// Decode an application/x-www-form-urlencoded query string into pairs.
pub(crate) fn query_pairs(query: &str) -> impl Iterator<Item = (String, String)> + '_ {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (url_decode(key), url_decode(value))
        })
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                match (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        decoded.push(high << 4 | low);
                        index += 2;
                    }
                    _ => decoded.push(b'%'),
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_pairs_decode_and_multi_map_folds_repeats() {
        let fields = multi_map(query_pairs("tag=a&tag=b&name=hello%20world&flag"));
        assert_eq!(fields["tag"], serde_json::json!(["a", "b"]));
        assert_eq!(fields["name"], "hello world");
        assert_eq!(fields["flag"], "");
    }

    #[test]
    fn malformed_percent_escapes_pass_through() {
        let fields = multi_map(query_pairs("a=%zz&b=%2"));
        assert_eq!(fields["a"], "%zz");
        assert_eq!(fields["b"], "%2");
    }
}
