//! Draft OpenAPI emission for `reproit init`. The output is honestly
//! marked as a derived draft (`x-reproit-derived` plus a header comment) and
//! deliberately loose: parsed-only body fields, string-typed path params, and
//! responses only where a live probe actually observed one. Fewer claims
//! means fewer oracles, which is the zero-false-positive discipline.

use super::enrich::Observation;
use super::extract::{path_params, Derived, METHODS};
use super::probe_plan::{PlannedProbe, ProbePlan};
use anyhow::{ensure, Result};
use std::collections::BTreeMap;

/// Sampled response shapes are recorded types-only and bounded.
const SHAPE_MAX_DEPTH: usize = 3;
const SHAPE_MAX_PROPERTIES: usize = 16;

/// Where a contract entry came from. The order is one-way: `inferred` (read
/// from source) and `observed` (seen live exactly once) are the only values
/// init may ever write, and NOTHING in this codebase upgrades an entry to
/// `confirmed`; that word belongs to an explicit statement by the user in the
/// schema file. A test pins the emitter to the first two.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Provenance {
    Inferred,
    Observed,
    #[allow(dead_code)] // Written only by the user; init must know the word.
    Confirmed,
}

impl Provenance {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Provenance::Inferred => "inferred",
            Provenance::Observed => "observed",
            Provenance::Confirmed => "confirmed",
        }
    }
}

/// Render the draft schema, then fail closed unless reproit's own schema
/// importer reads back exactly the derived operations.
pub(super) fn draft_yaml(
    title: &str,
    framework: &str,
    derived: &Derived,
    plan: &ProbePlan,
    observations: &BTreeMap<(String, String), Observation>,
) -> Result<String> {
    let yaml = render(title, framework, derived, plan, observations);
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
    plan: &ProbePlan,
    observations: &BTreeMap<(String, String), Observation>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# DRAFT schema derived by `reproit init` from {framework} source patterns.\n\
         # It is a starting point, not a verified contract: the routes were read from\n\
         # source, the types are loose placeholders, and any recorded response was\n\
         # observed exactly once. Review it, tighten the types and statuses your\n\
         # service actually promises, then run `reproit doctor`.\n\
         #\n\
         # Provenance is one-way. Every entry is marked `inferred` (read from source)\n\
         # or `observed` (seen live once during init). Only you may change a mark to\n\
         # `confirmed`; nothing in reproit upgrades one on its own.\n\
         openapi: 3.1.0\n\
         info:\n  title: {}\n  version: 0.1.0-draft\n\
         x-reproit-derived: true\npaths:\n",
        quote(title)
    ));
    for (path, methods) in &derived.routes {
        out.push_str(&format!("  {}:\n", quote(path)));
        for method in METHODS.iter().filter(|known| methods.contains(*known)) {
            let key = (method.to_string(), path.clone());
            out.push_str(&format!("    {method}:\n"));
            out.push_str(&format!(
                "      operationId: {}\n",
                operation_id(method, path)
            ));
            out.push_str(&format!(
                "      x-reproit-provenance: {}\n",
                Provenance::Inferred.as_str()
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
                push_request_body(&mut out, derived, method, path);
            }
            if let Some(observed) = observations.get(&key) {
                push_observed(&mut out, observed, plan.probe_for(method, path));
            } else if let Some(reason) = plan.skip_reason(method, path) {
                out.push_str(&format!("      # not probed during init: {reason}\n"));
            }
        }
    }
    out
}

/// The request body for a mutating route: a bare object unless the source
/// reader parsed field names for this handler, in which case each parsed
/// field is stated (untyped: a name read from source carries no type claim)
/// and required-ness only where the source said so.
fn push_request_body(out: &mut String, derived: &Derived, method: &str, path: &str) {
    let fields = derived
        .handlers
        .get(&(method.to_uppercase(), path.to_string()))
        .and_then(|handler| derived.bodies.get(handler))
        .filter(|fields| !fields.is_empty());
    out.push_str(
        "      requestBody:\n        content:\n          application/json:\n\
         \x20           schema:\n              type: object\n",
    );
    let Some(fields) = fields else {
        return;
    };
    out.push_str("              properties:\n");
    for (name, _) in fields.iter().take(SHAPE_MAX_PROPERTIES) {
        out.push_str(&format!("                {}: {{}}\n", quote(name)));
    }
    let required: Vec<&String> = fields
        .iter()
        .filter(|(_, fact)| fact.required)
        .map(|(name, _)| name)
        .take(SHAPE_MAX_PROPERTIES)
        .collect();
    if !required.is_empty() {
        out.push_str("              required:\n");
        for name in required {
            out.push_str(&format!("                - {}\n", quote(name)));
        }
    }
}

/// The observed-response block for one probed route: a comment stating what
/// was sent and seen (synthesized params and body, status, adapter effects),
/// and the response entry itself, marked `observed`.
fn push_observed(out: &mut String, observed: &Observation, probe: Option<&PlannedProbe>) {
    let mut sent = String::new();
    if let Some(probe) = probe {
        if !probe.params.is_empty() {
            let params: Vec<String> = probe
                .params
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            sent.push_str(&format!("; path params synthesized: {}", params.join(", ")));
        }
        if let Some(body) = &probe.body {
            sent.push_str(&format!(
                "; request body synthesized from parsed source fields: {body}"
            ));
        }
    }
    let effects = if observed.effects.is_empty() {
        String::new()
    } else {
        format!("; adapter effects: {}", observed.effects.join(", "))
    };
    out.push_str(&format!(
        "      # observed live during init: HTTP {}{sent}{effects}\n",
        observed.status
    ));
    out.push_str(&format!(
        "      responses:\n        \"{}\":\n          description: observed once by the \
         init live probe; verify before relying on it\n          x-reproit-provenance: {}\n",
        observed.status,
        Provenance::Observed.as_str()
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
