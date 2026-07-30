//! The dotted JSON-path grammar shared by contract evaluation and headless probing.
//!
//! Both the evaluator (`domain::backend::evaluate`) and the prober
//! (`workflows::backend_headless`) resolve identity and proof paths against captured
//! payloads. They must parse the exact same grammar: a private copy on either side lets
//! contract evaluation silently diverge from contract probing, so this module is the
//! single implementation. Grammar: an optional `$` root, then `.`-separated object keys;
//! empty segments are ignored, and `""` or `$` selects the root value.

use serde_json::Value;

pub(crate) fn json_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() || path == "$" {
        return Some(value);
    }
    path.trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .try_fold(value, |current, part| current.get(part))
}

/// Like [`json_path`], but arrays fan out: every element is descended, and a terminal
/// array contributes its items rather than itself.
pub(crate) fn json_path_values<'a>(value: &'a Value, path: &str) -> Vec<&'a Value> {
    let parts = path
        .trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    fn descend<'a>(value: &'a Value, parts: &[&str], values: &mut Vec<&'a Value>) {
        if parts.is_empty() {
            match value {
                Value::Array(items) => values.extend(items),
                _ => values.push(value),
            }
            return;
        }
        match value {
            Value::Array(items) => {
                for item in items {
                    descend(item, parts, values);
                }
            }
            Value::Object(object) => {
                if let Some(next) = object.get(parts[0]) {
                    descend(next, &parts[1..], values);
                }
            }
            _ => {}
        }
    }
    let mut values = Vec::new();
    descend(value, &parts, &mut values);
    values
}

/// Replaces the value at `path`, but only when the full parent chain and the final key
/// already exist: probing must never invent structure the capture did not contain.
pub(crate) fn set_json_path(value: &mut Value, path: &str, replacement: Value) -> bool {
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

/// Only scalars may serve as resource identities: objects and arrays compare by
/// structure, which makes replayed identity substitution ambiguous.
pub(crate) fn is_scalar_identity(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}
