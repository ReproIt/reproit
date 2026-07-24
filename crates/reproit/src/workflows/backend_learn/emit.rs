//! Draft OpenAPI emission for `reproit init --learn`. The output is honestly
//! marked as a derived draft (`x-reproit-derived` plus a header comment) and
//! deliberately loose: free-form bodies for mutating routes, string-typed path
//! params, and responses only where a live probe actually observed one. Fewer
//! claims means fewer oracles, which is the zero-false-positive discipline.

use super::enrich::Observation;
use super::extract::{path_params, Derived, METHODS};
use anyhow::{ensure, Result};
use std::collections::BTreeMap;

/// Sampled response shapes are recorded types-only and bounded.
const SHAPE_MAX_DEPTH: usize = 3;
const SHAPE_MAX_PROPERTIES: usize = 16;

/// Render the draft schema, then fail closed unless reproit's own schema
/// importer reads back exactly the derived operations.
pub(super) fn draft_yaml(
    title: &str,
    framework: &str,
    derived: &Derived,
    observations: &BTreeMap<String, Observation>,
) -> Result<String> {
    let yaml = render(title, framework, derived, observations);
    let document: serde_json::Value = serde_yaml::from_str(&yaml)?;
    let imported = crate::domain::backend::import_service_schema(&document).len();
    ensure!(
        imported == derived.operation_count(),
        "derived draft round-trip mismatch: emitted {} operations, importer read {imported}",
        derived.operation_count()
    );
    Ok(yaml)
}

fn render(
    title: &str,
    framework: &str,
    derived: &Derived,
    observations: &BTreeMap<String, Observation>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# DRAFT schema derived by `reproit init --learn` from {framework} source patterns.\n\
         # It is a starting point, not a verified contract: the routes were read from\n\
         # source, the types are loose placeholders, and any recorded response was\n\
         # observed exactly once. Review it, tighten the types and statuses your\n\
         # service actually promises, then run `reproit doctor`.\n\
         openapi: 3.1.0\n\
         info:\n  title: {}\n  version: 0.1.0-draft\n\
         x-reproit-derived: true\npaths:\n",
        quote(title)
    ));
    for (path, methods) in &derived.routes {
        out.push_str(&format!("  {}:\n", quote(path)));
        for method in METHODS.iter().filter(|known| methods.contains(*known)) {
            out.push_str(&format!("    {method}:\n"));
            out.push_str(&format!(
                "      operationId: {}\n",
                operation_id(method, path)
            ));
            let params = path_params(path);
            if !params.is_empty() {
                out.push_str("      parameters:\n");
                for name in params {
                    out.push_str(&format!(
                        "        - name: {}\n          in: path\n          required: true\n\
                         \x20         schema:\n            type: string\n",
                        quote(name)
                    ));
                }
            }
            if matches!(*method, "post" | "put" | "patch") {
                out.push_str(
                    "      requestBody:\n        content:\n          application/json:\n\
                     \x20           schema:\n              type: object\n",
                );
            }
            if *method == "get" {
                if let Some(observed) = observations.get(path) {
                    push_observed(&mut out, observed);
                }
            }
        }
    }
    out
}

/// The observed-response block for one probed GET route: a comment stating
/// what was seen (status, adapter effects), and the response entry itself.
fn push_observed(out: &mut String, observed: &Observation) {
    let effects = if observed.effects.is_empty() {
        String::new()
    } else {
        format!("; adapter effects: {}", observed.effects.join(", "))
    };
    out.push_str(&format!(
        "      # observed live by --learn: HTTP {}{effects}\n",
        observed.status
    ));
    out.push_str(&format!(
        "      responses:\n        \"{}\":\n          description: observed once by the \
         --learn live probe; verify before relying on it\n",
        observed.status
    ));
    if let Some(shape) = &observed.body {
        out.push_str("          content:\n            application/json:\n              schema:\n");
        push_shape(out, shape, 16, 0);
    }
}

/// Types-only JSON shape, depth- and width-bounded. `indent` is the column of
/// the schema's own keys.
fn push_shape(out: &mut String, value: &serde_json::Value, indent: usize, depth: usize) {
    let pad = " ".repeat(indent);
    use serde_json::Value;
    match value {
        Value::Object(fields) => {
            out.push_str(&format!("{pad}type: object\n"));
            if !fields.is_empty() && depth < SHAPE_MAX_DEPTH {
                out.push_str(&format!("{pad}properties:\n"));
                for (name, field) in fields.iter().take(SHAPE_MAX_PROPERTIES) {
                    out.push_str(&format!("{pad}  {}:\n", quote(name)));
                    push_shape(out, field, indent + 4, depth + 1);
                }
            }
        }
        Value::Array(items) => {
            out.push_str(&format!("{pad}type: array\n"));
            if let Some(first) = items.first() {
                if depth < SHAPE_MAX_DEPTH {
                    out.push_str(&format!("{pad}items:\n"));
                    push_shape(out, first, indent + 2, depth + 1);
                }
            }
        }
        Value::String(_) => out.push_str(&format!("{pad}type: string\n")),
        Value::Bool(_) => out.push_str(&format!("{pad}type: boolean\n")),
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            out.push_str(&format!("{pad}type: integer\n"))
        }
        Value::Number(_) => out.push_str(&format!("{pad}type: number\n")),
        // A null sample proves nothing about the type: claim nothing.
        Value::Null => out.push_str(&format!("{pad}{{}}\n")),
    }
}

/// Deterministic operation id: method + sanitized path segments.
fn operation_id(method: &str, path: &str) -> String {
    let flat: String = path
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    let segments: Vec<&str> = flat.split('_').filter(|part| !part.is_empty()).collect();
    if segments.is_empty() {
        format!("{method}_root")
    } else {
        format!("{method}_{}", segments.join("_"))
    }
}

/// JSON string quoting is valid YAML and handles every special character.
fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
}
