//! Capsule matching operations.

use super::*;

#[cfg(test)]
pub fn normalized_url(raw: &str) -> String {
    let (base, query) = raw.split_once('?').unwrap_or((raw, ""));
    let mut params: Vec<&str> = query.split('&').filter(|p| !p.is_empty()).collect();
    params.sort_unstable();
    if params.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", params.join("&"))
    }
}

#[cfg(test)]
pub fn exchange_match_key(exchange: &Exchange) -> String {
    let request_hash = exchange
        .request_body
        .as_ref()
        .map(|v| hex_sha256(&serde_json::to_vec(v).unwrap_or_default()))
        .unwrap_or_default();
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        exchange.actor,
        exchange.action_index,
        exchange.method.to_ascii_uppercase(),
        normalized_url(&exchange.url),
        request_hash,
        exchange.ordinal
    )
}

/// Deterministic JSON reduction candidates, largest structural removals first.
/// The caller replays each candidate and retains it only for the exact finding.
pub fn json_reductions(value: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    match value {
        Value::Object(map) => {
            for key in map.keys() {
                let mut candidate = map.clone();
                candidate.remove(key);
                out.push(Value::Object(candidate));
            }
            for (key, child) in map {
                for reduced in json_reductions(child) {
                    let mut candidate = map.clone();
                    candidate.insert(key.clone(), reduced);
                    out.push(Value::Object(candidate));
                }
            }
        }
        Value::Array(values) => {
            for i in 0..values.len() {
                let mut candidate = values.clone();
                candidate.remove(i);
                out.push(Value::Array(candidate));
            }
            for (i, child) in values.iter().enumerate() {
                for reduced in json_reductions(child) {
                    let mut candidate = values.clone();
                    candidate[i] = reduced;
                    out.push(Value::Array(candidate));
                }
            }
        }
        Value::String(s) if !s.is_empty() => out.push(Value::String(String::new())),
        Value::Number(_) => out.push(Value::from(0)),
        Value::Bool(true) => out.push(Value::Bool(false)),
        _ => {}
    }
    out
}
