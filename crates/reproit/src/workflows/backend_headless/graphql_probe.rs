//! Contract-invalid probes for GraphQL operations.
//!
//! A GraphQL server reports a rejected variable set inside an HTTP 200 body, so
//! the HTTP accept/reject verdict says nothing about it. This module reads the
//! rejection from the envelope instead: `errors` present with a null or absent
//! `data` is the rejection, and `data` that survives an out-of-domain variable
//! set is the accepted-invalid-input candidate. The probe values, the request
//! shape, and the oracle are the same ones the HTTP path uses.

use super::generation::{sample_domain, wrong_typed_value};
use super::replay::has_fingerprint;
use super::request::build_request;
use super::transport::invoke;
use super::types::{Endpoint, Transport};
use super::{
    PassRun, MAX_GENERATED_ARRAY_ITEMS, MAX_GENERATED_STRING_CHARS,
    MAX_INVALID_PROBES_PER_OPERATION,
};
use crate::domain::backend::{self, ValueDomain};
use serde_json::{json, Value};

/// How one probe leaves the declared domain. Recorded on every candidate so a
/// report names the class of invalid input the server accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InvalidClass {
    WrongType,
    EnumOutOfDomain,
    MissingRequired,
    Boundary,
}

impl InvalidClass {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::WrongType => "wrong-type",
            Self::EnumOutOfDomain => "enum-out-of-domain",
            Self::MissingRequired => "missing-required",
            Self::Boundary => "boundary",
        }
    }
}

/// What one GraphQL response says about the variable set that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GraphqlVerdict {
    /// The operation refused the input: a transport-level rejection, or the
    /// GraphQL rejection form (`errors` present, no data).
    Rejected,
    /// The operation returned data and reported no error. For a
    /// contract-invalid variable set this is the accepted-invalid finding.
    Accepted,
    /// Data and errors together. GraphQL nulls a failed field and keeps the
    /// rest of the selection, so this proves neither acceptance nor rejection
    /// of the variable set. It is reported as a candidate, never a finding.
    Partial,
    /// The server failed instead of rejecting. The 5xx oracle owns the verdict.
    Crashed,
    /// Neither data nor errors: not a GraphQL response, so it proves nothing.
    Malformed,
}

/// The verdict for one response. Pure, so the classification is testable
/// without a server. `envelope` is the whole response body, before the `data`
/// unwrap the transport performs for the happy path.
pub(super) fn graphql_verdict(status: u16, envelope: &Value) -> GraphqlVerdict {
    if status >= 500 {
        return GraphqlVerdict::Crashed;
    }
    if !(200..400).contains(&status) {
        return GraphqlVerdict::Rejected;
    }
    let errors = envelope
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty());
    let data = envelope.get("data").is_some_and(|data| !data.is_null());
    match (errors, data) {
        (false, true) => GraphqlVerdict::Accepted,
        (true, true) => GraphqlVerdict::Partial,
        (true, false) => GraphqlVerdict::Rejected,
        (false, false) => GraphqlVerdict::Malformed,
    }
}

/// Contract-invalid variable sets for one GraphQL operation. `domain` is the
/// operation's argument object. Every returned set is proven out-of-domain,
/// deterministic for a given `seed`, and the list is capped at
/// `MAX_INVALID_PROBES_PER_OPERATION`. Classes are interleaved, so a wide
/// argument list cannot spend the whole budget on one class.
pub(super) fn graphql_invalid_probes(
    domain: &ValueDomain,
    seed: u64,
) -> Vec<(InvalidClass, Value)> {
    let ValueDomain::Object {
        required,
        properties,
        ..
    } = domain
    else {
        return Vec::new();
    };
    let base = sample_domain(domain, seed, true, 0);
    let Some(object) = base.as_object() else {
        return Vec::new();
    };
    let mut by_class: Vec<Vec<(InvalidClass, Value)>> = vec![Vec::new(); 4];
    for (name, property) in properties {
        for (class, value) in argument_variants(property, seed) {
            let mut variables = base.clone();
            let Some(target) = variables.as_object_mut() else {
                continue;
            };
            target.insert(name.clone(), value);
            // Send only variable sets the contract provably rejects; anything
            // else would be ordinary valid traffic mislabeled as a probe.
            if domain.mismatch(&variables, "$input").is_some() {
                by_class[class_slot(class)].push((class, variables));
            }
        }
    }
    for name in required {
        if !object.contains_key(name) {
            continue;
        }
        let mut variables = base.clone();
        let Some(target) = variables.as_object_mut() else {
            continue;
        };
        target.remove(name);
        if domain.mismatch(&variables, "$input").is_some() {
            by_class[class_slot(InvalidClass::MissingRequired)]
                .push((InvalidClass::MissingRequired, variables));
        }
    }
    let mut probes = Vec::new();
    let deepest = by_class.iter().map(Vec::len).max().unwrap_or(0);
    for index in 0..deepest {
        for class in &by_class {
            if probes.len() >= MAX_INVALID_PROBES_PER_OPERATION {
                return probes;
            }
            if let Some(probe) = class.get(index) {
                probes.push(probe.clone());
            }
        }
    }
    probes
}

fn class_slot(class: InvalidClass) -> usize {
    match class {
        InvalidClass::WrongType => 0,
        InvalidClass::EnumOutOfDomain => 1,
        InvalidClass::MissingRequired => 2,
        InvalidClass::Boundary => 3,
    }
}

/// The invalid values for one argument. Introspection describes a nullable
/// argument as a union with null, so the constraints to violate live on the
/// non-null member. A non-null value the member rejects also fails the union,
/// and `graphql_invalid_probes` re-checks every candidate against the whole
/// argument object anyway.
fn argument_variants(domain: &ValueDomain, seed: u64) -> Vec<(InvalidClass, Value)> {
    let inner = nullable_inner(domain).unwrap_or(domain);
    let mut variants = Vec::new();
    if let Some(value) = wrong_typed_value(inner, seed) {
        variants.push((InvalidClass::WrongType, value));
    }
    if let ValueDomain::String {
        variants: allowed, ..
    } = inner
    {
        let value = format!("REPROIT_NOT_A_VARIANT_{seed}");
        if !allowed.is_empty() && !allowed.contains(&value) {
            variants.push((InvalidClass::EnumOutOfDomain, Value::String(value)));
        }
    }
    if let Some(value) = boundary_value(inner, seed) {
        variants.push((InvalidClass::Boundary, value));
    }
    variants
}

fn nullable_inner(domain: &ValueDomain) -> Option<&ValueDomain> {
    let ValueDomain::OneOf { variants } = domain else {
        return None;
    };
    if !variants.contains(&ValueDomain::Null) {
        return None;
    }
    let mut concrete = variants
        .iter()
        .filter(|variant| **variant != ValueDomain::Null);
    let first = concrete.next()?;
    concrete.next().is_none().then_some(first)
}

/// A value one step outside a declared bound. Introspection alone carries no
/// bounds, so this class only fires for operations whose contract was enriched
/// (an authored override or a bounded scalar), never on a bare schema import.
fn boundary_value(domain: &ValueDomain, seed: u64) -> Option<Value> {
    match domain {
        ValueDomain::Integer { min, max } => min
            .and_then(|bound| bound.checked_sub(1))
            .or_else(|| max.and_then(|bound| bound.checked_add(1)))
            .map(Value::from),
        ValueDomain::String {
            min_length,
            max_length,
            variants,
            ..
        } if variants.is_empty() => {
            if min_length.is_some_and(|bound| bound > 0) {
                return Some(Value::String(String::new()));
            }
            max_length
                .filter(|bound| *bound < MAX_GENERATED_STRING_CHARS)
                .map(|bound| Value::String("x".repeat(bound + 1)))
        }
        ValueDomain::Array {
            items,
            min_items,
            max_items,
            ..
        } => {
            if min_items.is_some_and(|bound| bound > 0) {
                return Some(Value::Array(Vec::new()));
            }
            let bound = (*max_items)?;
            if bound >= MAX_GENERATED_ARRAY_ITEMS {
                return None;
            }
            Some(Value::Array(
                (0..=bound)
                    .map(|index| sample_domain(items, seed.saturating_add(index as u64), true, 1))
                    .collect(),
            ))
        }
        _ => None,
    }
}

/// Invalid-input probes for every GraphQL operation. Each probe is one POST of
/// `{"query", "variables"}` through the transport the happy path already uses.
/// A GraphQL rejection is the contract's required outcome and stays silent; a
/// response that still carries data, or a 5xx, goes through the same oracle and
/// the same one-shot confirmation as the HTTP probes. Mutations are probed
/// under the run-level mutating-target confirmation, exactly like HTTP
/// mutating probes; this pass adds no exemption of its own.
pub(super) async fn probe_graphql_invalid_inputs(
    client: &reqwest::Client,
    endpoints: &[Endpoint],
    base_url: &str,
    seed: u64,
) -> PassRun {
    let mut run = PassRun::default();
    for endpoint in endpoints {
        if endpoint.transport != Transport::Http || endpoint.response_field.is_none() {
            continue;
        }
        let Some(domain) = endpoint.contract.input.as_ref() else {
            continue;
        };
        for (class, variables) in graphql_invalid_probes(domain, seed) {
            let request = match build_request(endpoint, base_url, variables) {
                Ok(request) => request,
                Err(error) => {
                    run.skipped.push(json!({
                        "operation": endpoint.contract.id,
                        "probeClass": class.as_str(),
                        "reason": error.to_string(),
                    }));
                    continue;
                }
            };
            let result = match invoke(client, endpoint, request.clone()).await {
                Ok(result) => result,
                Err(error) => {
                    run.execution_errors.push(json!({
                        "operation": endpoint.contract.id,
                        "probeClass": class.as_str(),
                        "error": error.to_string(),
                    }));
                    continue;
                }
            };
            run.exercised += 1;
            let verdict = graphql_verdict(result.status, &result.envelope);
            match verdict {
                GraphqlVerdict::Rejected => {
                    run.rejected += 1;
                    continue;
                }
                GraphqlVerdict::Malformed => {
                    run.skipped.push(json!({
                        "operation": endpoint.contract.id,
                        "probeClass": class.as_str(),
                        "reason": "response carried neither GraphQL data nor errors",
                    }));
                    continue;
                }
                GraphqlVerdict::Partial => {
                    run.candidates.push(json!({
                        "operation": endpoint.contract.id,
                        "probeClass": class.as_str(),
                        "reason": "operation returned data and errors together for a \
                                   contract-invalid variable set",
                        "confirmation": "inconclusive: a field error nulls its own field",
                    }));
                    continue;
                }
                GraphqlVerdict::Accepted | GraphqlVerdict::Crashed => {}
            }
            for violation in result.violations {
                let finding = backend::finding(&violation);
                match invoke(client, endpoint, request.clone()).await {
                    Ok(confirmation)
                        if graphql_verdict(confirmation.status, &confirmation.envelope)
                            == verdict
                            && has_fingerprint(&confirmation, &violation.fingerprint) =>
                    {
                        run.findings
                            .push((endpoint.clone(), request.clone(), Vec::new(), finding));
                    }
                    Ok(_) => run.candidates.push(json!({
                        "operation": endpoint.contract.id,
                        "probeClass": class.as_str(),
                        "reason": violation.reason,
                        "confirmation": "did not reproduce exactly",
                    })),
                    Err(error) => run.candidates.push(json!({
                        "operation": endpoint.contract.id,
                        "probeClass": class.as_str(),
                        "reason": violation.reason,
                        "confirmation": format!("confirmation failed: {error}"),
                    })),
                }
            }
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::{graphql_invalid_probes, graphql_verdict, GraphqlVerdict, InvalidClass};
    use crate::domain::backend::ValueDomain;
    use crate::workflows::backend_headless::request::build_request;
    use crate::workflows::backend_headless::schema::graphql_endpoints;
    use serde_json::{json, Value};
    use std::collections::{BTreeMap, BTreeSet};

    /// One query with a required enum argument, a required Int argument, and a
    /// nullable String argument. Introspection carries no bounds, so the
    /// boundary class is exercised by `bounded_domain` below instead.
    fn introspection() -> Value {
        json!({"data":{"__schema":{
            "queryType":{"name":"Query"},
            "mutationType":null,
            "subscriptionType":null,
            "types":[
                {"kind":"OBJECT","name":"Query","fields":[{
                    "name":"orders",
                    "args":[
                        {"name":"status","type":{
                            "kind":"NON_NULL","name":null,
                            "ofType":{"kind":"ENUM","name":"Status","ofType":null}}},
                        {"name":"limit","type":{
                            "kind":"NON_NULL","name":null,
                            "ofType":{"kind":"SCALAR","name":"Int","ofType":null}}},
                        {"name":"cursor","type":{
                            "kind":"SCALAR","name":"String","ofType":null}}
                    ],
                    "type":{"kind":"OBJECT","name":"Order","ofType":null}
                }]},
                {"kind":"ENUM","name":"Status","enumValues":[
                    {"name":"OPEN"},{"name":"CLOSED"}
                ]},
                {"kind":"OBJECT","name":"Order","fields":[
                    {"name":"id","args":[],"type":{
                        "kind":"NON_NULL","name":null,
                        "ofType":{"kind":"SCALAR","name":"ID","ofType":null}}}
                ]}
            ]
        }}})
    }

    fn classes(probes: &[(InvalidClass, Value)]) -> BTreeSet<&'static str> {
        probes.iter().map(|(class, _)| class.as_str()).collect()
    }

    #[test]
    fn graphql_probes_cover_every_invalid_class_the_schema_can_express() {
        let endpoint = graphql_endpoints(&introspection()).pop().unwrap();
        let domain = endpoint.contract.input.clone().unwrap();
        let probes = graphql_invalid_probes(&domain, 7);
        assert!(!probes.is_empty(), "expected GraphQL invalid probes");
        assert_eq!(
            classes(&probes),
            BTreeSet::from(["wrong-type", "enum-out-of-domain", "missing-required"])
        );
        // Every probe is proven out of the declared argument domain.
        for (_, variables) in &probes {
            assert!(
                domain.mismatch(variables, "$input").is_some(),
                "probe {variables} is inside the declared domain"
            );
        }
        // The enum probe keeps every other argument valid.
        let (_, enum_probe) = probes
            .iter()
            .find(|(class, _)| *class == InvalidClass::EnumOutOfDomain)
            .expect("an enum probe");
        assert!(enum_probe["status"].as_str().unwrap().contains("REPROIT"));
        assert!(enum_probe["limit"].is_number());
        // The missing-required probe drops exactly one required argument.
        let (_, missing) = probes
            .iter()
            .find(|(class, _)| *class == InvalidClass::MissingRequired)
            .expect("a missing-required probe");
        let absent = ["status", "limit"]
            .iter()
            .filter(|name| missing.get(**name).is_none())
            .count();
        assert_eq!(absent, 1);
    }

    #[test]
    fn graphql_probes_are_deterministic_and_bounded() {
        let endpoint = graphql_endpoints(&introspection()).pop().unwrap();
        let domain = endpoint.contract.input.clone().unwrap();
        let first = graphql_invalid_probes(&domain, 7);
        assert_eq!(first, graphql_invalid_probes(&domain, 7));
        assert!(first.len() <= super::MAX_INVALID_PROBES_PER_OPERATION);
    }

    /// Bounds reach a GraphQL operation through an authored contract override,
    /// not through introspection, so the boundary class is proven on one.
    fn bounded_domain() -> ValueDomain {
        ValueDomain::Object {
            required: BTreeSet::from(["limit".to_string()]),
            properties: BTreeMap::from([(
                "limit".to_string(),
                ValueDomain::Integer {
                    min: Some(1),
                    max: Some(50),
                },
            )]),
            additional: false,
        }
    }

    #[test]
    fn graphql_probes_step_one_past_a_declared_bound() {
        let probes = graphql_invalid_probes(&bounded_domain(), 3);
        let (_, boundary) = probes
            .iter()
            .find(|(class, _)| *class == InvalidClass::Boundary)
            .expect("a boundary probe");
        assert_eq!(boundary["limit"], json!(0));
    }

    #[test]
    fn graphql_probes_serialize_as_operation_variables() {
        let endpoint = graphql_endpoints(&introspection()).pop().unwrap();
        let domain = endpoint.contract.input.clone().unwrap();
        let (_, variables) = graphql_invalid_probes(&domain, 7).remove(0);
        let request = build_request(
            &endpoint,
            "http://127.0.0.1:9999/graphql",
            variables.clone(),
        )
        .expect("a GraphQL probe request");
        let body = request.body.expect("a GraphQL request body");
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "http://127.0.0.1:9999/graphql");
        assert_eq!(body["variables"], variables);
        assert!(body["query"]
            .as_str()
            .unwrap()
            .contains("query Reproit($status: Status!, $limit: Int!, $cursor: String)"));
    }

    #[test]
    fn a_graphql_errors_response_is_a_rejection_not_an_accepted_invalid_input() {
        let errors = json!({"errors":[{"message":"invalid value for Status"}]});
        assert_eq!(graphql_verdict(200, &errors), GraphqlVerdict::Rejected);
        let null_data = json!({"data":null,"errors":[{"message":"bad"}]});
        assert_eq!(graphql_verdict(200, &null_data), GraphqlVerdict::Rejected);
        // A transport-level refusal is a rejection too.
        assert_eq!(graphql_verdict(400, &json!({})), GraphqlVerdict::Rejected);
    }

    #[test]
    fn graphql_data_for_an_invalid_variable_set_is_the_accepted_candidate() {
        let data = json!({"data":{"orders":{"id":"o1"}}});
        assert_eq!(graphql_verdict(200, &data), GraphqlVerdict::Accepted);
        // Data plus errors proves neither verdict, so it never becomes a
        // finding: a field error nulls its own field inside a live data map.
        let partial = json!({"data":{"orders":null},"errors":[{"message":"partial"}]});
        assert_eq!(graphql_verdict(200, &partial), GraphqlVerdict::Partial);
    }

    #[test]
    fn a_graphql_server_error_and_an_empty_envelope_stay_apart_from_both_verdicts() {
        let crashed = json!({"errors":[{"message":"boom"}]});
        assert_eq!(graphql_verdict(500, &crashed), GraphqlVerdict::Crashed);
        assert_eq!(graphql_verdict(200, &json!({})), GraphqlVerdict::Malformed);
        assert_eq!(
            graphql_verdict(200, &Value::String("not json".into())),
            GraphqlVerdict::Malformed
        );
    }
}
