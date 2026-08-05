//! Draft OpenAPI emission for `reproit init`. The output is honestly
//! marked as a derived draft (`x-reproit-derived` plus a header comment) and
//! deliberately loose: parsed-only body fields, string-typed path params, and
//! responses only where a live probe actually observed one. Fewer claims
//! means fewer oracles, which is the zero-false-positive discipline.

use super::enrich::Observation;
use super::extract::{path_params, Derived, METHODS};
use super::probe_plan::{PlannedProbe, ProbePlan};
use super::response_facts::{ResponseFact, Serializers, WireShape};
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
            push_parameters(&mut out, derived, method, path);
            if matches!(*method, "post" | "put" | "patch") {
                push_request_body(&mut out, derived, method, path);
            }
            let observed = observations.get(&key);
            if observed.is_none() {
                if let Some(reason) = plan.skip_reason(method, path) {
                    out.push_str(&format!("      # not probed during init: {reason}\n"));
                }
            }
            let inferred = derived
                .handlers
                .get(&(method.to_uppercase(), path.clone()))
                .and_then(|handler| derived.responses.get(handler).map(|fact| (handler, fact)));
            push_responses(
                &mut out,
                inferred,
                observed,
                plan.probe_for(method, path),
                &derived.serializers,
            );
        }
    }
    out
}

/// The responses block for one operation, from either or both sources of
/// evidence: statuses and body shapes INFERRED from the handler's source
/// (typed languages), and the response OBSERVED once by the live probe. Each
/// entry carries its own provenance, comment and marker; where both speak
/// about one status, the inferred types carry the schema and the comment
/// records both.
fn push_responses(
    out: &mut String,
    inferred: Option<(&String, &ResponseFact)>,
    observed: Option<&Observation>,
    probe: Option<&PlannedProbe>,
    serializers: &Serializers,
) {
    let mut statuses: Vec<u16> = inferred
        .map(|(_, fact)| fact.statuses.keys().copied().collect())
        .unwrap_or_default();
    if let Some(observation) = observed {
        if !statuses.contains(&observation.status) {
            statuses.push(observation.status);
            statuses.sort_unstable();
        }
    }
    if statuses.is_empty() {
        return;
    }
    // The observation comment states what was SENT as well as what was seen,
    // so it sits above the block exactly as the observed-only draft wrote it.
    if let Some(observation) = observed {
        push_observed_comment(out, observation, probe);
    }
    out.push_str("      responses:\n");
    for status in statuses {
        let stated = inferred
            .and_then(|(handler, fact)| fact.statuses.get(&status).map(|shape| (handler, shape)));
        let seen = observed.filter(|observation| observation.status == status);
        match (stated, seen) {
            (Some((handler, shape)), seen) => {
                // A status the live probe saw is observed, whether or not the
                // source also stated it: the tag must agree with the comment,
                // or the same block claims both at once. The inferred types
                // still carry the schema.
                let (also, provenance) = if seen.is_some() {
                    ("; also observed live during init", Provenance::Observed)
                } else {
                    ("", Provenance::Inferred)
                };
                out.push_str(&format!(
                    "        # inferred from `{handler}` return types in source{also}\n"
                ));
                out.push_str(&format!(
                    "        \"{status}\":\n          description: inferred from the \
                     handler's return types; verify before relying on it\n          \
                     x-reproit-provenance: {}\n",
                    provenance.as_str()
                ));
                push_inferred_body(out, shape, serializers);
            }
            (None, Some(observation)) => {
                out.push_str(&format!(
                    "        \"{status}\":\n          description: observed once by the \
                     init live probe; verify before relying on it\n          \
                     x-reproit-provenance: {}\n",
                    Provenance::Observed.as_str()
                ));
                if let Some(shape) = &observation.body {
                    out.push_str(
                        "          content:\n            application/json:\n              \
                         schema:\n",
                    );
                    push_shape(out, shape, 16, 0);
                }
            }
            (None, None) => {}
        }
    }
}

/// The content block for an inferred response body. An unknown shape states no
/// content at all: the handler writes SOMETHING there, but the types do not
/// say what, and an empty schema would read as a claim.
fn push_inferred_body(out: &mut String, shape: &WireShape, serializers: &Serializers) {
    if *shape == WireShape::Unknown {
        return;
    }
    out.push_str("          content:\n            application/json:\n              schema:\n");
    push_wire_shape(out, shape, serializers, 16, 0);
}

/// Render a wire shape as a schema. Named types resolve against the collected
/// serializers; a name with no surviving declaration claims nothing. Depth is
/// bounded, which also bounds self-referential types.
fn push_wire_shape(
    out: &mut String,
    shape: &WireShape,
    serializers: &Serializers,
    indent: usize,
    depth: usize,
) {
    let pad = " ".repeat(indent);
    match shape {
        WireShape::Primitive(name) => out.push_str(&format!("{pad}type: {name}\n")),
        WireShape::Object => out.push_str(&format!("{pad}type: object\n")),
        WireShape::Array(items) => {
            out.push_str(&format!("{pad}type: array\n"));
            if depth < SHAPE_MAX_DEPTH && **items != WireShape::Unknown {
                out.push_str(&format!("{pad}items:\n"));
                push_wire_shape(out, items, serializers, indent + 2, depth + 1);
            }
        }
        WireShape::Named(name) => match serializers.get(name) {
            Some(fields) if depth < SHAPE_MAX_DEPTH => {
                out.push_str(&format!("{pad}type: object\n"));
                out.push_str(&format!("{pad}properties:\n"));
                for (field, wire) in fields.iter().take(SHAPE_MAX_PROPERTIES) {
                    out.push_str(&format!("{pad}  {}:\n", quote(field)));
                    push_wire_shape(out, &wire.shape, serializers, indent + 4, depth + 1);
                }
                // Required stays within the emitted property window: a field
                // the width bound dropped must not be demanded by name.
                let required: Vec<&String> = fields
                    .iter()
                    .take(SHAPE_MAX_PROPERTIES)
                    .filter(|(_, wire)| wire.required)
                    .map(|(field, _)| field)
                    .collect();
                if !required.is_empty() {
                    out.push_str(&format!("{pad}required:\n"));
                    for field in required {
                        out.push_str(&format!("{pad}  - {}\n", quote(field)));
                    }
                }
            }
            // The name did not survive collection (undeclared here, or two
            // conflicting declarations): claim nothing rather than guess.
            _ => out.push_str(&format!("{pad}{{}}\n")),
        },
        WireShape::Unknown => out.push_str(&format!("{pad}{{}}\n")),
    }
}

/// The parameters block: the path template's own parameters, plus the query
/// parameters the handler's source names.
///
/// A query parameter is emitted with `required: false`: the name is what the
/// source states, a demand is not, and the draft's discipline is that a claim
/// it cannot support is worse than silence. An unnamed parameter, though, is
/// worse than both: it is a knob no generated request and no oracle can reach,
/// which is why `/search?q=` used to derive as a bare path.
///
/// The `string` type is not a claim about how the handler parses the value; it
/// is what a query parameter IS on the wire, the same reason a path parameter
/// carries it. An empty schema instead means "any JSON", and the generator
/// duly produced objects and nulls for it, which no query string can carry:
/// the request failed to build and the whole operation went unexercised, so
/// naming the parameter would have cost coverage rather than buying it.
fn push_parameters(out: &mut String, derived: &Derived, method: &str, path: &str) {
    let path_params = path_params(path);
    let query = derived
        .handlers
        .get(&(method.to_uppercase(), path.to_string()))
        .and_then(|handler| derived.queries.get(handler))
        .filter(|fields| !fields.is_empty());
    if path_params.is_empty() && query.is_none() {
        return;
    }
    out.push_str("      parameters:\n");
    for name in path_params {
        out.push_str(&format!(
            "        - name: {}\n          in: path\n          required: true\n\
             \x20         x-reproit-provenance: {}\n          schema:\n            type: string\n",
            quote(name),
            Provenance::Inferred.as_str()
        ));
    }
    let Some(query) = query else {
        return;
    };
    for (name, fact) in query.iter().take(SHAPE_MAX_PROPERTIES) {
        out.push_str(&format!(
            "        # inferred from the handler's source: {}\n",
            fact.evidence.as_deref().unwrap_or("named in the handler")
        ));
        out.push_str(&format!(
            "        - name: {}\n          in: query\n          required: {}\n\
             \x20         x-reproit-provenance: {}\n          schema:\n            type: string\n",
            quote(name),
            fact.required,
            Provenance::Inferred.as_str()
        ));
    }
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
    out.push_str(&format!(
        "      requestBody:\n        x-reproit-provenance: {}\n        content:\n          \
         application/json:\n            schema:\n              type: object\n",
        Provenance::Inferred.as_str()
    ));
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

/// The comment above an observed responses block: what was sent (synthesized
/// params and body) and what was seen (status, adapter effects). The response
/// entry itself is written by `push_responses`, marked `observed`.
fn push_observed_comment(out: &mut String, observed: &Observation, probe: Option<&PlannedProbe>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::backend_learn::response_facts::WireField;
    use std::collections::BTreeSet;

    /// One GET /items served by `listItems`, whose code states a 200 carrying
    /// an array of Item, plus the Item serializer's typed fields.
    fn derived_with_responses() -> Derived {
        let mut derived = Derived::default();
        derived
            .routes
            .insert("/items".into(), BTreeSet::from(["get"]));
        derived
            .handlers
            .insert(("GET".into(), "/items".into()), "listItems".into());
        let mut fact = ResponseFact::default();
        fact.state(
            200,
            WireShape::Array(Box::new(WireShape::Named("Item".into()))),
        );
        derived.responses.insert("listItems".into(), fact);
        let item = BTreeMap::from([
            (
                "id".to_string(),
                WireField {
                    shape: WireShape::Primitive("string"),
                    required: true,
                },
            ),
            (
                "price".to_string(),
                WireField {
                    shape: WireShape::Primitive("number"),
                    required: true,
                },
            ),
            (
                "note".to_string(),
                WireField {
                    shape: WireShape::Primitive("string"),
                    required: false,
                },
            ),
        ]);
        derived.serializers.insert("Item".into(), item);
        derived
    }

    #[test]
    fn an_inferred_response_contract_lands_in_the_draft_with_its_provenance() {
        // The typed-language track: what a reader inferred becomes a
        // responses block the importer enforces, every entry marked inferred,
        // statuses outside the source never claimed.
        let derived = derived_with_responses();
        let yaml = draft_yaml(
            "fixture",
            "gin",
            &derived,
            &ProbePlan::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(
            yaml.contains("# inferred from `listItems` return types in source"),
            "{yaml}"
        );
        assert!(yaml.contains("\"200\":"), "{yaml}");
        assert!(yaml.contains("type: array"), "{yaml}");
        assert!(
            yaml.matches("x-reproit-provenance: inferred").count() >= 2,
            "the operation and its response entry both carry the mark: {yaml}"
        );
        assert!(
            yaml.contains("\"note\":") && !yaml.contains("- \"note\""),
            "an optional field is stated but never demanded: {yaml}"
        );
        let document: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
        let operations = crate::domain::backend::import_service_schema(&document);
        assert_eq!(operations.len(), 1);
        let operation = &operations[0];
        assert_eq!(operation.success_statuses, vec![200]);
        let output = operation.outputs_by_status.get(&200).expect("enforced");
        // The importer reads the inferred schema as a real output domain: a
        // nil Go slice serialized as `null` violates it, which is the
        // planted-bug class this contract exists to catch.
        assert!(
            output
                .mismatch(&serde_json::Value::Null, "$output")
                .is_some(),
            "a null body must violate the inferred array contract"
        );
        assert!(
            output.mismatch(&serde_json::json!([]), "$output").is_none(),
            "an empty array satisfies it"
        );
        assert!(
            output
                .mismatch(&serde_json::json!([{"id": "a", "price": "9"}]), "$output")
                .is_some(),
            "a string price must violate the inferred field type"
        );
        assert!(
            output
                .mismatch(&serde_json::json!([{"id": "a", "price": 9.5}]), "$output")
                .is_none(),
            "the typed happy path satisfies it, `note` omitted"
        );
    }

    #[test]
    fn observed_and_inferred_provenance_share_one_responses_block() {
        let derived = derived_with_responses();
        let observations = BTreeMap::from([(
            ("get".to_string(), "/items".to_string()),
            Observation {
                status: 200,
                body: Some(serde_json::json!([])),
                effects: Vec::new(),
            },
        )]);
        let yaml = draft_yaml(
            "fixture",
            "gin",
            &derived,
            &ProbePlan::default(),
            &observations,
        )
        .unwrap();
        // One status both sources speak about: the typed schema wins, the
        // comment records both provenances, and the block appears once.
        assert!(
            yaml.contains(
                "# inferred from `listItems` return types in source; also observed live \
                 during init"
            ),
            "{yaml}"
        );
        assert!(
            yaml.contains("# observed live during init: HTTP 200"),
            "{yaml}"
        );
        assert_eq!(yaml.matches("responses:").count(), 1, "{yaml}");
        assert_eq!(yaml.matches("\"200\":").count(), 1, "{yaml}");
        serde_yaml::from_str::<serde_json::Value>(&yaml).expect("valid yaml");
    }

    #[test]
    fn a_query_parameter_read_from_source_becomes_a_named_input() {
        // The gap this closes: `/search?q=` derived as a bare path, so nothing
        // downstream could name `q`, let alone omit it. The name must survive
        // the round-trip into an input domain without claiming a type.
        let mut derived = Derived::default();
        derived
            .routes
            .insert("/search".into(), BTreeSet::from(["get"]));
        derived.handlers.insert(
            ("GET".into(), "/search".into()),
            "get /search inline handler".into(),
        );
        derived.queries.insert(
            "get /search inline handler".into(),
            BTreeMap::from([(
                "q".to_string(),
                crate::workflows::backend_learn::field_facts::FieldFact {
                    evidence: Some("read from the request query string in the handler".into()),
                    ..Default::default()
                },
            )]),
        );
        let yaml = draft_yaml(
            "fixture",
            "express",
            &derived,
            &ProbePlan::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(yaml.contains("in: query"), "{yaml}");
        assert!(yaml.contains("- name: \"q\""), "{yaml}");
        assert!(yaml.contains("required: false"), "{yaml}");
        assert!(
            yaml.contains("read from the request query string in the handler"),
            "the evidence travels with the claim: {yaml}"
        );
        assert!(
            !yaml.contains(
                "in: query\n          required: false\n          \
                            x-reproit-provenance: observed"
            ),
            "a source read is inferred, never observed: {yaml}"
        );

        let document: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
        let operations = crate::domain::backend::import_service_schema(&document);
        assert_eq!(operations.len(), 1);
        let input = operations[0].input.as_ref().expect("q is an input");
        assert!(
            input
                .mismatch(&serde_json::json!({"query": {"q": "shoes"}}), "$input")
                .is_none(),
            "the named query parameter must satisfy the derived input: {input:?}"
        );
        // A query string carries text. Leaving the type open let the generator
        // synthesize objects and nulls, which no query string can carry, so
        // the request failed to build and the operation went unexercised.
        assert!(
            input
                .mismatch(
                    &serde_json::json!({"query": {"q": {"not": "scalar"}}}),
                    "$input"
                )
                .is_some(),
            "a non-scalar query value must not satisfy it: {input:?}"
        );
    }

    #[test]
    fn an_unresolved_named_shape_claims_nothing() {
        let mut derived = derived_with_responses();
        derived.serializers.clear();
        let yaml = draft_yaml(
            "fixture",
            "gin",
            &derived,
            &ProbePlan::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(yaml.contains("type: array"), "{yaml}");
        assert!(
            !yaml.contains("properties"),
            "a name with no surviving declaration must not invent fields: {yaml}"
        );
        serde_yaml::from_str::<serde_json::Value>(&yaml).expect("valid yaml");
    }
}
