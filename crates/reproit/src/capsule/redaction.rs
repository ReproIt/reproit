//! Capsule redaction operations.

use super::*;

pub fn redact_capsule(capsule: &mut Capsule, policy: &RedactionPolicy) {
    for exchange in &mut capsule.exchanges {
        redact_exchange(exchange, policy, &mut capsule.redactions);
    }
    for event in &mut capsule.backend_events {
        redact_backend_event(event, policy, &mut capsule.redactions);
    }
    capsule.redactions.sort();
    capsule.redactions.dedup();
}

pub(crate) fn redact_backend_event(
    event: &mut crate::model::backend::BackendEvent,
    policy: &RedactionPolicy,
    manifest: &mut Vec<String>,
) {
    let identity = event.idempotency_key.take().map(|key| {
        if key.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 24 && digest.chars().all(|c| c.is_ascii_hexdigit())
        }) {
            return key;
        }
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(key.as_bytes());
        format!(
            "sha256:{}",
            digest[..12]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    });
    if let Ok(mut value) = serde_json::to_value(&*event) {
        redact_value(&mut value, policy, "$backend", manifest);
        if let Ok(redacted) = serde_json::from_value(value) {
            *event = redacted;
        }
    }
    event.idempotency_key = identity;
}

pub fn redact_exchange(
    exchange: &mut Exchange,
    policy: &RedactionPolicy,
    manifest: &mut Vec<String>,
) {
    redact_headers(&mut exchange.request_headers, policy, manifest);
    redact_headers(&mut exchange.response_headers, policy, manifest);
    if let Some(body) = &mut exchange.request_body {
        redact_value(body, policy, "$request", manifest);
    }
    if let Some(body) = &mut exchange.response_body {
        redact_value(body, policy, "$response", manifest);
    }
}

fn redact_headers(
    headers: &mut BTreeMap<String, String>,
    policy: &RedactionPolicy,
    manifest: &mut Vec<String>,
) {
    let keys: Vec<String> = headers.keys().cloned().collect();
    for key in keys {
        if policy.drop_headers.contains(&key.to_ascii_lowercase()) {
            headers.insert(key.clone(), "<reproit:secret>".into());
            manifest.push(format!("header:{key}"));
        }
    }
}

pub(crate) fn redact_value(
    value: &mut Value,
    policy: &RedactionPolicy,
    path: &str,
    manifest: &mut Vec<String>,
) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                if policy.secret_keys.contains(&key.to_ascii_lowercase()) {
                    *child = typed_placeholder(child);
                    manifest.push(child_path);
                } else {
                    redact_value(child, policy, &child_path, manifest);
                }
            }
        }
        Value::Array(values) => {
            for (i, child) in values.iter_mut().enumerate() {
                redact_value(child, policy, &format!("{path}[{i}]"), manifest);
            }
        }
        _ => {}
    }
}

fn typed_placeholder(value: &Value) -> Value {
    if value.pointer("/$reproit/redacted").and_then(Value::as_bool) == Some(true) {
        return value.clone();
    }
    let (kind, length) = match value {
        Value::Null => ("null", None),
        Value::Bool(_) => ("boolean", None),
        Value::Number(number) if number.is_i64() || number.is_u64() => ("integer", None),
        Value::Number(_) => ("number", None),
        Value::String(value) => ("string", Some(value.chars().count())),
        Value::Array(value) => ("array", Some(value.len())),
        Value::Object(_) => ("object", None),
    };
    serde_json::json!({"$reproit": {
        "redacted": true,
        "type": kind,
        "length": length,
    }})
}
