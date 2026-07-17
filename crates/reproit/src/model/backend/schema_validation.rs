use super::hash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A standards-backed defect in an API description. These findings do not
/// infer application behavior. They only report a rule the description itself
/// violates.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackendSchemaViolation {
    pub operation: String,
    pub oracle: String,
    pub pointer: String,
    pub reason: String,
    pub fingerprint: String,
}

/// Validate OpenAPI's parameter uniqueness rule.
///
/// Parameters are unique by `(name, in)` within a Path Item or Operation. An
/// Operation parameter with the same key as a Path Item parameter is a legal
/// override, not a duplicate. Local references are resolved before comparing
/// keys. Unresolved and cyclic references abstain.
pub fn validate_openapi_parameter_uniqueness(document: &Value) -> Vec<BackendSchemaViolation> {
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut violations = Vec::new();
    for (path, raw_path_item) in paths {
        let Some(path_item) = resolve_local_ref_chain(raw_path_item, document) else {
            continue;
        };
        let path_pointer = format!("/paths/{}", escape_json_pointer(path));
        validate_parameter_list(
            document,
            path_item.get("parameters"),
            &format!("{path_pointer}/parameters"),
            &format!("PATH {path}"),
            &mut violations,
        );
        let Some(methods) = path_item.as_object() else {
            continue;
        };
        for (method, operation) in methods {
            if ![
                "get", "post", "put", "patch", "delete", "head", "options", "trace",
            ]
            .contains(&method.as_str())
            {
                continue;
            }
            let operation_id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} {path}", method.to_ascii_uppercase()));
            validate_parameter_list(
                document,
                operation.get("parameters"),
                &format!("{path_pointer}/{method}/parameters"),
                &operation_id,
                &mut violations,
            );
        }
    }
    violations.sort_by(|left, right| left.fingerprint.cmp(&right.fingerprint));
    violations
}

fn validate_parameter_list(
    document: &Value,
    raw: Option<&Value>,
    pointer: &str,
    operation: &str,
    violations: &mut Vec<BackendSchemaViolation>,
) {
    let Some(parameters) = raw.and_then(Value::as_array) else {
        return;
    };
    let mut first = BTreeMap::<(String, String), usize>::new();
    for (index, raw_parameter) in parameters.iter().enumerate() {
        let Some(parameter) = resolve_local_ref_chain(raw_parameter, document) else {
            continue;
        };
        let (Some(name), Some(location)) = (
            parameter.get("name").and_then(Value::as_str),
            parameter.get("in").and_then(Value::as_str),
        ) else {
            continue;
        };
        let key = (name.to_string(), location.to_string());
        let Some(first_index) = first.insert(key.clone(), index) else {
            continue;
        };
        let reason = format!(
            "parameter {name:?} in {location:?} appears more than once in the same parameter list"
        );
        let identity =
            format!("openapi-parameter-uniqueness:{operation}:{pointer}:{name}:{location}");
        violations.push(BackendSchemaViolation {
            operation: operation.to_string(),
            oracle: "openapi-parameter-uniqueness".into(),
            pointer: format!("{pointer}/{index}"),
            reason: format!("{reason}; first declared at {pointer}/{first_index}"),
            fingerprint: hash(identity.as_bytes())[..20].to_string(),
        });
    }
}

fn resolve_local_ref_chain<'a>(value: &'a Value, document: &'a Value) -> Option<&'a Value> {
    let mut current = value;
    let mut seen = BTreeSet::new();
    loop {
        let Some(reference) = current.get("$ref").and_then(Value::as_str) else {
            return Some(current);
        };
        let pointer = reference.strip_prefix('#')?;
        if !seen.insert(pointer) {
            return None;
        }
        current = document.pointer(pointer)?;
    }
}

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
