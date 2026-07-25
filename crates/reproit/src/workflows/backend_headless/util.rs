//! Small shared encoders and JSON-path helpers for the backend workflows.

use serde_json::Value;

pub(super) fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub(super) fn hex_hash(value: &[u8]) -> String {
    crate::domain::hash::sha256_hex(value)
}

pub(super) fn json_path_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() || path == "$" {
        return Some(value);
    }
    path.trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .try_fold(value, |current, part| current.get(part))
}

pub(super) fn set_json_path(value: &mut Value, path: &str, replacement: Value) -> bool {
    let parts = path
        .trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let Some((last, parents)) = parts.split_last() else {
        *value = replacement;
        return true;
    };
    let Some(parent) = parents
        .iter()
        .try_fold(value, |current, part| current.get_mut(*part))
    else {
        return false;
    };
    let Some(object) = parent.as_object_mut() else {
        return false;
    };
    if !object.contains_key(*last) {
        return false;
    }
    object.insert((*last).into(), replacement);
    true
}

pub(super) fn is_scalar_identity(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}
