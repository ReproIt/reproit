//! Experimental language-independent backend causal contracts.
//!
//! Static adapters, service schemas, and runtime instrumentation all normalize
//! into this module. Static and inferred facts guide exploration. Only declared
//! or schema-owned contracts paired with a concrete runtime witness can produce
//! a finding.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const EVENT_MARKER: &str = "REPROIT:BACKEND ";

mod contracts;
#[allow(unused_imports)]
pub use contracts::{
    AuthorizationDecision, AuthorizationDenyPolicy, AuthorizationPrincipal, BackendConfig,
    BackendInvariant, BackendProofContract, CodecProjection, ConcurrencyPolicy,
    ControlledFailureWitness, FleetInvariant, QueryComparison, QueryFilterContract,
    QueryPaginationContract, QuerySortContract, QuerySortDirection, QuerySortType,
    ResourceConsistency, ResourceCreateContract, ResourceFieldContract, ResourceLifecycleContract,
    ResourceMutationContract, ResourceReadContract, RoundTripCheck,
};

mod config;

mod schema_document;
#[cfg(test)]
use schema_document::graphql_sdl_document;
pub use schema_document::load_service_document;
mod operation;
#[allow(unused_imports)]
pub use operation::{
    Authority, FunctionSummary, IdempotencyResponseReplay, OperationContract, ProgramSummary,
    StaticEffect, ValueSlot,
};

mod domain;
use domain::default_true;
pub use domain::ValueDomain;
#[derive(Clone, Copy)]
struct RedactedMetadata<'a> {
    kind: &'a str,
    length: Option<usize>,
}

fn redacted_metadata(value: &Value) -> Option<RedactedMetadata<'_>> {
    let metadata = value.get("$reproit")?.as_object()?;
    if metadata.get("redacted").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    Some(RedactedMetadata {
        kind: metadata.get("type")?.as_str()?,
        length: metadata
            .get("length")
            .and_then(Value::as_u64)
            .map(|length| length as usize),
    })
}

fn matches_format(format: &str, value: &str) -> bool {
    match format {
        "uuid" => {
            let parts = value.split('-').map(str::len).collect::<Vec<_>>();
            parts == [8, 4, 4, 4, 12] && value.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
        }
        // JSON Schema treats unknown and implementation-defined formats as
        // annotations. Email syntax is deliberately not approximated here:
        // quoted local parts and internationalized domains make simple checks
        // reject valid addresses, violating the zero-false-positive boundary.
        "email" => true,
        "date-time" => chrono::DateTime::parse_from_rfc3339(value).is_ok(),
        "uri" | "url" => {
            static URI: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            URI.get_or_init(|| {
                regex::Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:.+$")
                    .expect("the URI format regex is valid")
            })
            .is_match(value)
        }
        _ => true,
    }
}

mod effect;
pub use effect::{EffectKind, EffectPattern};

mod event;
pub(crate) use event::from_protocol_frames;
pub(crate) use event::parse_runner_events;
pub use event::{parse_events, BackendEvent, BackendEventKind, GraphqlSelection};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackendViolation {
    pub operation: String,
    pub contract_hash: String,
    pub fingerprint: String,
    pub oracle: String,
    pub reason: String,
    pub trace_id: String,
    pub span_id: String,
    pub action_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FrozenBackendGuard {
    pub operations: Vec<OperationContract>,
    #[serde(default)]
    pub invariants: Vec<BackendInvariant>,
    #[serde(default)]
    pub resources: Vec<ResourceLifecycleContract>,
    #[serde(default)]
    pub proofs: Vec<BackendProofContract>,
    pub fingerprints: BTreeSet<String>,
}

impl FrozenBackendGuard {
    pub fn from_findings(config: &BackendConfig, findings: &[Value]) -> Option<Self> {
        let fingerprints = findings
            .iter()
            .filter(|finding| {
                finding.get("oracle").and_then(Value::as_str) == Some("backend-contract")
            })
            .filter_map(|finding| finding.get("fingerprint").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        if fingerprints.is_empty() {
            return None;
        }
        let mut ids = findings
            .iter()
            .filter_map(|finding| finding.get("operation").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();
        let resources = config
            .resources
            .iter()
            .filter(|resource| ids.contains(resource.read.operation.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for resource in &resources {
            ids.insert(resource.create.operation.as_str());
            ids.insert(resource.read.operation.as_str());
            if let Some(update) = &resource.update {
                ids.insert(update.operation.as_str());
            }
            if let Some(delete) = &resource.delete {
                ids.insert(delete.operation.as_str());
            }
        }
        let proofs = config
            .proofs
            .iter()
            .filter(|proof| {
                proof
                    .operation_ids()
                    .iter()
                    .any(|operation| ids.contains(operation))
            })
            .cloned()
            .collect::<Vec<_>>();
        for proof in &proofs {
            ids.extend(proof.operation_ids());
        }
        let invariants = config
            .invariants
            .iter()
            .filter(|invariant| match invariant {
                BackendInvariant::QuerySemantics { operation, .. } => {
                    ids.contains(operation.as_str())
                }
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        for invariant in &invariants {
            if let BackendInvariant::QuerySemantics {
                pagination:
                    Some(QueryPaginationContract {
                        reference_operation: Some(reference),
                        ..
                    }),
                ..
            } = invariant
            {
                ids.insert(reference.as_str());
            }
        }
        let operations = config
            .operations
            .iter()
            .filter(|operation| ids.contains(operation.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        (!operations.is_empty()).then_some(Self {
            operations,
            invariants,
            resources,
            proofs,
            fingerprints,
        })
    }

    pub fn reproduces(&self, log: &str) -> bool {
        let config = BackendConfig {
            enabled: true,
            origins: Vec::new(),
            schemas: Vec::new(),
            operations: self.operations.clone(),
            programs: Vec::new(),
            invariants: self.invariants.clone(),
            resources: self.resources.clone(),
            proofs: self.proofs.clone(),
            fleet: FleetInvariant::default(),
        };
        evaluate(&config, &parse_events(log))
            .iter()
            .any(|violation| self.fingerprints.contains(&violation.fingerprint))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }

    pub fn load(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

mod evaluate;
pub use evaluate::evaluate;
#[cfg(test)]
use evaluate::{
    failed_atomicity_effect_outcome, selection_mismatch, AtomicityEffectOutcome, EffectEvent,
    Invocation,
};

pub fn finding(violation: &BackendViolation) -> Value {
    json!({
        "oracle": "backend-contract",
        "invariant": format!("backend:{}", violation.oracle),
        "kind": violation.oracle,
        "message": violation.reason,
        "operation": violation.operation,
        "contract_hash": violation.contract_hash,
        "fingerprint": violation.fingerprint,
        "trigger": violation.fingerprint,
        "trace_id": violation.trace_id,
        "span_id": violation.span_id,
        "action_index": violation.action_index,
        "frames": [format!("backend:{}", violation.operation)],
    })
}

pub fn write_evidence(
    path: &Path,
    config: &BackendConfig,
    events: &[BackendEvent],
    violations: &[BackendViolation],
) -> std::io::Result<()> {
    if !config.enabled || (events.is_empty() && violations.is_empty()) {
        return Ok(());
    }
    let normalized = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::NormalizedTrace,
        vec![],
        json!({
        "operations": config.operations,
        "resources": config.resources,
        "graph": build_graph(config, events),
        "events": events,
        }),
    )
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let evaluation = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::Evaluation,
        vec![normalized.id.clone()],
        json!({ "violations": violations }),
    )
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let run_hash = hash(&serde_json::to_vec(events)?);
    let graph = reproit_protocol::EvidenceGraph {
        run_id: format!("backend-{}", &run_hash[..16]),
        root: evaluation.id.clone(),
        nodes: vec![normalized, evaluation],
    };
    graph
        .validate()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(path, serde_json::to_vec_pretty(&graph)?)
}

mod graph;
#[allow(unused_imports)]
pub use graph::{
    build_graph, CausalContractGraph, GraphEdge, GraphNode, GraphNodeKind, GraphRelation,
};
mod schema_import;
#[cfg(test)]
use schema_import::import_graphql;
pub use schema_import::{import_openapi, import_service_schema};
mod schema_validation;
pub use schema_validation::{validate_openapi_parameter_uniqueness, BackendSchemaViolation};
mod protocol;
#[allow(unused_imports)]
pub use protocol::{
    validate_http_byte_range, validate_http_conditional_cache, validate_http_redirect_transition,
    validate_http_response_media_type, validate_protocol_lifecycle, validate_websocket_contract,
    HttpExchangeEvidence, ProtocolEvidence, ProtocolLifecycleContract, ProtocolLifecycleEvent,
    ProtocolLifecycleEvidence, ProtocolLifecycleRule, ProtocolViolation, WebSocketContract,
    WebSocketEvidence,
};

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let fields = object
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_default(),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{fields}}}")
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn hash(bytes: &[u8]) -> String {
    crate::domain::hash::sha256_hex(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, span: &str, operation: &str, kind: BackendEventKind) -> BackendEvent {
        BackendEvent {
            sequence,
            trace_id: "trace-a".into(),
            span_id: span.into(),
            action_index: 1,
            parent_span_id: None,
            operation: operation.into(),
            build: None,
            config_contract: None,
            actor: Some("alice".into()),
            tenant: Some("tenant-a".into()),
            idempotency_key: None,
            selections: Vec::new(),
            event: kind,
        }
    }

    fn contract() -> OperationContract {
        OperationContract {
            id: "createMessage".into(),
            authority: Authority::Declared,
            input: None,
            output: Some(ValueDomain::Object {
                required: BTreeSet::from(["id".into()]),
                properties: BTreeMap::from([(
                    "id".into(),
                    ValueDomain::String {
                        min_length: Some(1),
                        max_length: None,
                        pattern: None,
                        format: None,
                        variants: vec![],
                    },
                )]),
                additional: true,
            }),
            outputs_by_status: BTreeMap::new(),
            success_statuses: vec![201],
            read_only: false,
            idempotent: false,
            idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
            tenant_isolated: true,
            promised_effects: vec![EffectPattern {
                kind: EffectKind::Write,
                resource: Some("messages".into()),
                event: None,
                at_least: 1,
                at_most: None,
            }],
        }
    }

    #[test]
    fn hard_oracles_require_concrete_authoritative_witnesses() {
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![contract()],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        let events = vec![
            event(
                1,
                "span-a",
                "createMessage",
                BackendEventKind::Start {
                    input: json!({"body":"hello"}),
                },
            ),
            event(
                2,
                "span-a",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-b".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m1"})),
                    payload: None,
                },
            ),
            event(
                3,
                "span-a",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 2);
        assert!(violations.iter().any(|v| v.oracle == "response-shape"));
        assert!(violations.iter().any(|v| v.oracle == "tenant-isolation"));
    }

    #[test]
    fn inferred_contracts_never_create_findings() {
        let mut inferred = contract();
        inferred.authority = Authority::Inferred;
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![inferred],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        let events = vec![
            event(
                1,
                "span-a",
                "createMessage",
                BackendEventKind::Start { input: Value::Null },
            ),
            event(
                2,
                "span-a",
                "createMessage",
                BackendEventKind::Return {
                    output: Value::Null,
                    // Even a status outside this inferred operation's imported
                    // shape is guidance only; inferred facts never become hard
                    // response-status findings.
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn imports_openapi_operations_and_resolves_schema_references() {
        let document = json!({
            "openapi":"3.1.0",
            "paths":{"/messages":{"post":{
                "operationId":"createMessage",
                "responses": {
                    "201": {"content": {"application/json": {
                        "schema": {"$ref": "#/components/schemas/Message"}
                    }}}
                }
            }}},
            "components": {"schemas": {"Message": {
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string", "format": "uuid"}}
            }}}
        });
        let operations = import_openapi(&document);
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].authority, Authority::Schema);
        assert_eq!(operations[0].success_statuses, [201]);
        assert!(operations[0]
            .output
            .as_ref()
            .unwrap()
            .mismatch(&json!({}), "$output")
            .is_some());
    }

    #[test]
    fn openapi_30_nullable_accepts_null_without_weakening_non_null_shapes() {
        let document = json!({
            "openapi":"3.0.3",
            "paths":{"/value":{"get":{
                "operationId":"getValue",
                "responses":{"200":{"content":{"application/json":{"schema":{
                    "type":"object",
                    "required":["nullable","strict"],
                    "properties":{
                        "nullable":{"type":"string","nullable":true},
                        "strict":{"type":"string"}
                    }
                }}}}}
            }}}
        });
        let output = import_openapi(&document).pop().unwrap().output.unwrap();
        assert!(output
            .mismatch(&json!({"nullable":null,"strict":"ok"}), "$output")
            .is_none());
        assert!(output
            .mismatch(&json!({"nullable":"ok","strict":"ok"}), "$output")
            .is_none());
        assert!(output
            .mismatch(&json!({"nullable":7,"strict":"ok"}), "$output")
            .is_some());
        assert!(output
            .mismatch(&json!({"nullable":null,"strict":null}), "$output")
            .is_some());
    }

    #[test]
    fn openapi_30_nullable_wraps_refs_and_composed_schemas() {
        let document = json!({
            "openapi":"3.0.3",
            "paths":{"/value":{"get":{
                "operationId":"getValue",
                "responses":{"200":{"content":{"application/json":{"schema":{
                    "type":"object",
                    "required":["refValue","allValue","oneValue","anyValue"],
                    "properties":{
                        "refValue":{"$ref":"#/components/schemas/NullableName"},
                        "allValue": {
                            "nullable": true,
                            "allOf": [{"$ref": "#/components/schemas/StrictObject"}]
                        },
                        "oneValue":{"nullable":true,"oneOf":[{"type":"string"},{"type":"integer"}]},
                        "anyValue": {
                            "nullable": true,
                            "anyOf": [
                                {"type": "boolean"},
                                {"type": "array", "items": {"type": "string"}}
                            ]
                        }
                    }
                }}}}}
            }}},
            "components":{"schemas":{
                "NullableName":{"type":"string","nullable":true},
                "StrictObject": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {"id": {"type": "string"}}
                }
            }}
        });
        let output = import_openapi(&document).pop().unwrap().output.unwrap();
        assert!(output
            .mismatch(
                &json!({"refValue":null,"allValue":null,"oneValue":null,"anyValue":null}),
                "$output"
            )
            .is_none());
        assert!(output
            .mismatch(
                &json!({"refValue":"ok","allValue":{"id":"1"},"oneValue":1,"anyValue":["x"]}),
                "$output"
            )
            .is_none());
        assert!(output
            .mismatch(
                &json!({"refValue":false,"allValue":{},"oneValue":false,"anyValue":"bad"}),
                "$output"
            )
            .is_some());
    }

    #[test]
    fn openapi_31_does_not_treat_legacy_nullable_as_authority() {
        let document = json!({
            "openapi":"3.1.0",
            "paths":{"/value":{"get":{
                "responses":{"200":{"content":{"application/json":{"schema":{
                    "type":"string","nullable":true
                }}}}}
            }}}
        });
        let output = import_openapi(&document).pop().unwrap().output.unwrap();
        assert!(output.mismatch(&json!(null), "$output").is_some());
        assert!(output.mismatch(&json!("ok"), "$output").is_none());
    }

    #[test]
    fn pinned_rspec_openapi_nullable_examples_are_valid_but_bad_types_are_not() {
        let document: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/backend/rspec-openapi-nullable.json"
        )))
        .unwrap();
        let operation = import_openapi(&document).pop().unwrap();
        let output = operation.output.unwrap();
        let example = document
            .pointer("/paths/~1nullable/get/responses/200/content/application~1json/example")
            .unwrap();
        assert!(output.mismatch(example, "$output").is_none());
        let mut invalid = example.clone();
        invalid["label"] = json!(7);
        assert!(output.mismatch(&invalid, "$output").is_some());
    }

    #[test]
    fn openapi_imports_exact_parameters_and_only_safe_media() {
        let document = json!({
            "openapi":"3.1.0",
            "paths":{"/projects/{project}/export":{"post":{
                "operationId":"exportProject",
                "parameters":[
                    {"in":"path","name":"project","required":true,"schema":{"type":"string"}},
                    {
                        "in": "query",
                        "name": "limit",
                        "required": true,
                        "schema": {"type": "integer", "minimum": 1}
                    },
                    {"in":"header","name":"X-Mode","schema":{"type":"string","enum":["safe"]}},
                    {"in":"cookie","name":"session","required":true,"schema":{"type":"string"}},
                    {
                        "in": "query",
                        "name": "filter",
                        "style": "deepObject",
                        "schema": {
                            "type": "object",
                            "properties": {"x": {"type": "string"}}
                        }
                    }
                ],
                "requestBody":{"required":true,"content":{
                    "application/vnd.reproit+json": {"schema": {
                        "type": "object",
                        "required": ["format"],
                        "properties": {"format": {"type": "string"}}
                    }},
                    "application/xml": {"schema": {
                        "type": "object",
                        "required": ["unsafe"],
                        "properties": {"unsafe": {"type": "string"}}
                    }}
                }},
                "responses":{"200":{"content":{
                    "text/plain":{"schema":{"type":"string"}},
                    "application/octet-stream":{"schema":{"type":"string"}}
                }}}
            }}}
        });
        let operation = import_openapi(&document).pop().unwrap();
        let input = operation.input.unwrap();
        assert!(input
            .mismatch(
                &json!({
                    "path":{"project":"p1"},
                    "query":{"limit":1},
                    "headers":{"x-mode":"safe"},
                    "body":{"format":"text"}
                }),
                "$input"
            )
            .is_none());
        assert!(input
            .mismatch(
                &json!({
                    "path":{}, "query":{"limit":1}, "body":{"format":"text"}
                }),
                "$input"
            )
            .is_some());
        assert!(operation
            .output
            .unwrap()
            .mismatch(&json!("ok"), "$output")
            .is_none());
    }

    #[test]
    fn openapi_response_shapes_are_bound_to_the_observed_status() {
        let document = json!({"openapi":"3.1.0","paths":{"/items":{"post":{
            "operationId":"createItem","responses":{
                "200": {"content": {"application/json": {"schema": {
                    "type": "object",
                    "required": ["existing"],
                    "properties": {"existing": {"type": "boolean"}}
                }}}},
                "201": {"content": {"application/json": {"schema": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {"id": {"type": "string"}}
                }}}}
            }
        }}}});
        let operation = import_openapi(&document).pop().unwrap();
        assert!(operation.outputs_by_status[&200]
            .mismatch(&json!({"existing":true}), "$output")
            .is_none());
        assert!(operation.outputs_by_status[&201]
            .mismatch(&json!({"existing":true}), "$output")
            .is_some());
    }

    #[test]
    fn openapi_recursive_references_are_bounded_without_losing_outer_shape() {
        let document = json!({
            "openapi":"3.1.0",
            "paths":{
                "/nodes":{"get":{"operationId":"getNodes","responses":{"200":{"content":{
                    "application/json":{"schema":{"$ref":"#/components/schemas/Node"}}
                }}}}}
            },
            "components":{"schemas":{
                "Node":{"type":"object","required":["name"],"properties":{
                    "name":{"type":"string"},
                    "parent":{"$ref":"#/components/schemas/Node"},
                    "children":{"type":"array","items":{"$ref":"#/components/schemas/Node"}}
                }}
            }}
        });
        let operation = import_openapi(&document).pop().unwrap();
        let output = operation.output.unwrap();
        assert!(output
            .mismatch(
                &json!({"name":"root","parent":null,"children":[]}),
                "$output"
            )
            .is_none());
        assert_eq!(
            output.mismatch(&json!({"parent":null,"children":[]}), "$output"),
            Some("$output.name is required".into())
        );
    }

    #[test]
    fn loads_multifile_openapi_references_hermetically() {
        let root =
            std::env::temp_dir().join(format!("reproit-backend-multifile-{}", std::process::id()));
        std::fs::create_dir_all(root.join("schemas")).unwrap();
        std::fs::write(
            root.join("openapi.yaml"),
            r#"openapi: 3.1.0
paths:
  /users/{id}:
    get:
      operationId: getUser
      parameters:
        - $ref: schemas/parameters.yaml#/Id
      responses:
        "200":
          content:
            application/json:
              schema:
                $ref: schemas/user.yaml#/User
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("schemas/parameters.yaml"),
            r#"Id:
  name: id
  in: path
  required: true
  schema: { type: string }
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("schemas/user.yaml"),
            r#"User:
  type: object
  required: [id, name]
  properties:
    id: { type: string }
    name: { type: string }
"#,
        )
        .unwrap();
        let document = load_service_document(&root.join("openapi.yaml")).unwrap();
        let operation = import_openapi(&document).pop().unwrap();
        assert_eq!(
            operation
                .input
                .as_ref()
                .unwrap()
                .mismatch(&json!({"path":{"id":"u1"}}), "$input"),
            None
        );
        assert_eq!(
            operation
                .output
                .as_ref()
                .unwrap()
                .mismatch(&json!({"id":"u1"}), "$output"),
            Some("$output.name is required".into())
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn imports_graphql_introspection_without_localized_names() {
        let document = json!({"data":{"__schema":{
            "queryType":{"name":"Query"},
            "mutationType":{"name":"Mutation"},
            "types":[
                {"kind": "OBJECT", "name": "Query", "fields": [{
                    "name": "message",
                    "args": [{"name": "id", "type": {
                        "kind": "NON_NULL",
                        "name": null,
                        "ofType": {"kind": "SCALAR", "name": "ID"}
                    }}],
                    "type": {"kind": "OBJECT", "name": "Message"}
                }]},
                {"kind": "OBJECT", "name": "Mutation", "fields": [{
                    "name": "createMessage",
                    "args": [{
                        "name": "body",
                        "type": {"kind": "SCALAR", "name": "String"}
                    }],
                    "type": {"kind": "OBJECT", "name": "Message"}
                }]},
                {"kind": "OBJECT", "name": "Message", "fields": [
                    {"name": "id", "type": {
                        "kind": "NON_NULL",
                        "name": null,
                        "ofType": {"kind": "SCALAR", "name": "ID"}
                    }},
                    {"name": "body", "type": {"kind": "SCALAR", "name": "String"}}
                ]}
            ]
        }}});
        let operations = import_service_schema(&document);
        assert_eq!(operations.len(), 2);
        assert!(
            operations
                .iter()
                .find(|op| op.id == "message")
                .unwrap()
                .read_only
        );
        assert!(
            !operations
                .iter()
                .find(|op| op.id == "createMessage")
                .unwrap()
                .read_only
        );
        let query = operations.iter().find(|op| op.id == "message").unwrap();
        assert!(query
            .input
            .as_ref()
            .unwrap()
            .mismatch(&json!({}), "$input")
            .is_some());
    }

    #[test]
    fn imports_raw_graphql_sdl_without_a_framework_adapter() {
        let document = graphql_sdl_document(
            r#"
              type Query { account(id: ID!): Account }
              type Account { id: ID!, exposure: Float!, limit: Float! }
            "#,
        )
        .unwrap();
        let operations = import_service_schema(&document);
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].id, "account");
        assert!(operations[0].read_only);
        assert!(operations[0].input.is_some());
        assert!(operations[0].output.is_some());
    }

    #[test]
    fn graphql_output_contract_respects_selection_sets_without_losing_type_checks() {
        // Reduced from the open-source Countries GraphQL API. Both Country
        // fields are NON_NULL in the schema, but selecting only `code` is a
        // complete and valid GraphQL response. Until traces carry a normalized
        // selection set, absence of an unselected field cannot be a finding.
        let document = json!({"data":{"__schema":{
            "queryType":{"name":"Query"},
            "types":[
                {"kind":"OBJECT","name":"Query","fields":[{
                    "name":"country",
                    "args": [{"name": "code", "type": {
                        "kind": "NON_NULL",
                        "name": null,
                        "ofType": {"kind": "SCALAR", "name": "String"}
                    }}],
                    "type":{"kind":"OBJECT","name":"Country"}
                }]},
                {"kind":"OBJECT","name":"Country","fields":[
                    {"name": "code", "type": {
                        "kind": "NON_NULL",
                        "name": null,
                        "ofType": {"kind": "SCALAR", "name": "String"}
                    }},
                    {"name": "awsRegion", "type": {
                        "kind": "NON_NULL",
                        "name": null,
                        "ofType": {"kind": "SCALAR", "name": "String"}
                    }}
                ]}
            ]
        }}});
        let operations = import_service_schema(&document);
        let country = operations
            .iter()
            .find(|operation| operation.id == "country")
            .unwrap();
        let input = country.input.as_ref().unwrap();
        assert!(input.mismatch(&json!({"code":"US"}), "$input").is_none());
        assert!(input.mismatch(&json!({}), "$input").is_some());

        let output = country.output.as_ref().unwrap();
        assert!(output.mismatch(&json!({"code":"US"}), "$output").is_none());
        assert!(output.mismatch(&Value::Null, "$output").is_none());
        assert_eq!(
            output.mismatch(&json!({"code": 7}), "$output"),
            Some("$output does not match any allowed variant".into())
        );
        let selected = [GraphqlSelection {
            schema_path: "awsRegion".into(),
            response_path: "region".into(),
            type_condition: None,
        }];
        assert!(selection_mismatch(output, &json!({"code":"US"}), &selected)
            .unwrap()
            .contains("region was selected"));
        assert!(selection_mismatch(
            output,
            &json!({"code":"US","region":"us-east-2"}),
            &selected,
        )
        .is_none());
    }

    #[test]
    fn graphql_union_selection_applies_only_to_the_exact_runtime_type() {
        let document = json!({"data":{"__schema":{
            "queryType":{"name":"Query"},
            "types":[
                {"kind": "OBJECT", "name": "Query", "fields": [{
                    "name": "search",
                    "args": [],
                    "type": {"kind": "UNION", "name": "SearchResult"}
                }]},
                {"kind": "UNION", "name": "SearchResult", "possibleTypes": [
                    {"kind": "OBJECT", "name": "Human"},
                    {"kind": "OBJECT", "name": "Bot"}
                ]},
                {"kind": "OBJECT", "name": "Human", "fields": [{
                    "name": "handle",
                    "type": {"kind": "NON_NULL", "ofType": {
                        "kind": "SCALAR", "name": "String"
                    }}
                }]},
                {"kind": "OBJECT", "name": "Bot", "fields": [{
                    "name": "id",
                    "type": {"kind": "NON_NULL", "ofType": {
                        "kind": "SCALAR", "name": "ID"
                    }}
                }]}
            ]
        }}});
        let operation = import_graphql(&document).pop().unwrap();
        let output = operation.output.unwrap();
        let selected = [GraphqlSelection {
            schema_path: "handle".into(),
            response_path: "name".into(),
            type_condition: Some("Human".into()),
        }];
        assert!(
            selection_mismatch(&output, &json!({"__typename":"Bot","id":"b1"}), &selected,)
                .is_none()
        );
        assert!(
            selection_mismatch(&output, &json!({"__typename":"Human"}), &selected,)
                .unwrap()
                .contains("name was selected")
        );
        assert!(selection_mismatch(
            &output,
            &json!({"__typename":"Human","name":"ada"}),
            &selected,
        )
        .is_none());

        let list = ValueDomain::Array {
            items: Box::new(output.clone()),
            min_items: None,
            max_items: None,
            unique: false,
        };
        assert!(selection_mismatch(
            &list,
            &json!([
                {"__typename":"Bot","id":"b1"},
                {"__typename":"Human","name":7}
            ]),
            &selected,
        )
        .unwrap()
        .contains("$output[1].name"));
    }

    #[test]
    fn imports_protobuf_descriptor_json_as_grpc_operations() {
        let document = json!({"file":[{
            "package":"chat.v1",
            "messageType":[
                {"name":"GetRequest","field":[{"name":"id","type":"TYPE_STRING"}]},
                {"name": "Message", "field": [
                    {"name": "id", "type": "TYPE_STRING"},
                    {"name": "tags", "type": "TYPE_STRING", "label": "LABEL_REPEATED"}
                ]}
            ],
            "service": [{"name": "Chat", "method": [{
                "name": "Get",
                "inputType": ".chat.v1.GetRequest",
                "outputType": ".chat.v1.Message"
            }]}]
        }]});
        let operations = import_service_schema(&document);
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].id, "chat.v1.Chat/Get");
        assert!(operations[0]
            .output
            .as_ref()
            .unwrap()
            .mismatch(&json!({"id":"m1","tags":["a"]}), "$output")
            .is_none());
    }

    #[test]
    fn protobuf_64_bit_domains_follow_exact_protojson_encoding() {
        let signed = ValueDomain::ProtoInteger64 { signed: true };
        let unsigned = ValueDomain::ProtoInteger64 { signed: false };
        for value in [
            json!("-9223372036854775808"),
            json!("9223372036854775807"),
            json!(0),
            json!(9_007_199_254_740_991_i64),
        ] {
            assert!(signed.mismatch(&value, "$value").is_none(), "{value}");
        }
        for value in [
            json!("0"),
            json!("18446744073709551615"),
            json!(9_007_199_254_740_991_u64),
        ] {
            assert!(unsigned.mismatch(&value, "$value").is_none(), "{value}");
        }
        for value in [
            json!("01"),
            json!("-0"),
            json!("1.0"),
            json!("9223372036854775808"),
            json!(9_007_199_254_740_992_u64),
        ] {
            assert!(signed.mismatch(&value, "$value").is_some(), "{value}");
        }
        for value in [
            json!("-1"),
            json!("18446744073709551616"),
            json!("0x10"),
            json!(9_007_199_254_740_992_u64),
        ] {
            assert!(unsigned.mismatch(&value, "$value").is_some(), "{value}");
        }

        let repeated = ValueDomain::Array {
            items: Box::new(unsigned),
            min_items: None,
            max_items: None,
            unique: false,
        };
        assert!(repeated
            .mismatch(&json!(["0", "18446744073709551615"]), "$values")
            .is_none());
        assert!(repeated.mismatch(&json!(["0", "-1"]), "$values").is_some());
    }

    #[test]
    fn graph_joins_runtime_effects_to_declared_operations() {
        let mut config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![contract()],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        config.programs.push(ProgramSummary {
            language: "rust".into(),
            build: Some("abc123".into()),
            functions: vec![FunctionSummary {
                id: "handlers::create_message".into(),
                name: "create_message".into(),
                source: Some("src/handlers.rs:42".into()),
                operation: Some("createMessage".into()),
                inputs: vec![ValueSlot {
                    name: "body".into(),
                    domain: ValueDomain::String {
                        min_length: Some(1),
                        max_length: Some(4000),
                        pattern: None,
                        format: None,
                        variants: vec![],
                    },
                }],
                output: Some(ValueDomain::Resource {
                    resource: "message".into(),
                }),
                calls: vec!["repository::insert_message".into()],
                effects: vec![StaticEffect {
                    kind: EffectKind::Write,
                    resource: Some("messages".into()),
                    event: None,
                }],
                requires: vec!["actor.member_of(room)".into()],
                ensures: vec!["message.exists".into()],
                authority: Authority::Inferred,
            }],
        });
        let events = vec![event(
            1,
            "span-a",
            "createMessage",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("messages".into()),
                key: Some("m1".into()),
                tenant: Some("tenant-a".into()),
                event: None,
                before: None,
                after: None,
                payload: None,
            },
        )];
        let graph = build_graph(&config, &events);
        assert!(graph.nodes.contains_key("operation:createMessage"));
        assert!(graph
            .nodes
            .contains_key("function:handlers::create_message"));
        assert!(graph
            .nodes
            .contains_key("function:repository::insert_message"));
        assert!(graph.nodes.contains_key("resource:messages"));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.relation == GraphRelation::Writes));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.relation == GraphRelation::Implements));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.relation == GraphRelation::Calls));
    }

    #[test]
    fn read_only_and_missing_effect_oracles_are_exact() {
        let mut read = contract();
        read.id = "getMessage".into();
        read.read_only = true;
        read.promised_effects = vec![EffectPattern {
            kind: EffectKind::Read,
            resource: Some("messages".into()),
            event: None,
            at_least: 1,
            at_most: None,
        }];
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![read],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        let events = vec![
            event(
                1,
                "read",
                "getMessage",
                BackendEventKind::Start { input: json!("m1") },
            ),
            event(
                2,
                "read",
                "getMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"seen":true})),
                    payload: None,
                },
            ),
            event(
                3,
                "read",
                "getMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 2);
        assert!(violations.iter().any(|v| v.oracle == "read-only-mutation"));
        assert!(violations.iter().any(|v| v.oracle == "missing-effect"));
    }

    #[test]
    fn incomplete_effect_telemetry_cannot_create_an_absence_finding() {
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![contract()],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        let events = vec![
            event(
                1,
                "incomplete",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "incomplete",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: false,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn upper_effect_bound_confirms_duplicate_side_effects() {
        let mut create = contract();
        create.promised_effects[0].at_most = Some(1);
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![create],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        let events = vec![
            event(
                1,
                "duplicate",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "duplicate",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m1"})),
                    payload: None,
                },
            ),
            event(
                3,
                "duplicate",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m2".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m2"})),
                    payload: None,
                },
            ),
            event(
                4,
                "duplicate",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "excess-effect");
    }

    #[test]
    fn idempotency_compares_persistent_effects_for_the_same_actor_and_tenant() {
        let mut create = contract();
        create.idempotent = true;
        create.output = None;
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![create],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        let invocation = |sequence, span: &str, key: &str| {
            let mut start = event(
                sequence,
                span,
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            );
            start.idempotency_key = Some("same-key".into());
            vec![
                start,
                event(
                    sequence + 1,
                    span,
                    "createMessage",
                    BackendEventKind::Effect {
                        effect: EffectKind::Write,
                        resource: Some("messages".into()),
                        key: Some(key.into()),
                        tenant: Some("tenant-a".into()),
                        event: None,
                        before: None,
                        after: Some(json!({"id":key})),
                        payload: None,
                    },
                ),
                event(
                    sequence + 2,
                    span,
                    "createMessage",
                    BackendEventKind::Return {
                        output: json!({"id":key}),
                        status: Some(201),
                        success: true,
                        effects_complete: true,
                    },
                ),
            ]
        };
        let events = invocation(1, "one", "m1")
            .into_iter()
            .chain(invocation(4, "two", "m2"))
            .collect::<Vec<_>>();
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "idempotency");
    }

    #[test]
    fn idempotent_cached_retry_without_a_second_effect_is_clean() {
        let mut create = contract();
        create.idempotent = true;
        let mut config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![create],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        let mut first = event(
            1,
            "one",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        );
        first.idempotency_key = Some("same-key".into());
        let mut retry = event(
            4,
            "two",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        );
        retry.idempotency_key = Some("same-key".into());
        let events = vec![
            first,
            event(
                2,
                "one",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m1"})),
                    payload: None,
                },
            ),
            event(
                3,
                "one",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
            retry,
            event(
                5,
                "two",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m1"})),
                    payload: None,
                },
            ),
            event(
                6,
                "two",
                "createMessage",
                BackendEventKind::Return {
                    // Generic idempotency does not promise byte-identical
                    // responses. A cached retry may use a different success
                    // status/body while preserving the same final effect.
                    output: json!({"id":"different-response-id"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert!(violations.is_empty(), "{violations:#?}");

        // Byte-identical response replay is a stronger, explicit contract.
        config.operations[0].idempotency_response_replay = IdempotencyResponseReplay::Exact;
        assert!(evaluate(&config, &events)
            .iter()
            .any(|violation| violation.oracle == "idempotency"));

        // A reused key with different request input is caller misuse, not proof
        // that an identical request violated idempotency.
        let mut different_request = events;
        different_request[3].event = BackendEventKind::Start {
            input: json!({"different":true}),
        };
        assert!(!evaluate(&config, &different_request)
            .iter()
            .any(|violation| violation.oracle == "idempotency"));
    }

    #[test]
    fn accepted_input_outside_declared_domain_is_a_hard_finding() {
        let mut create = contract();
        create.input = Some(ValueDomain::Object {
            required: BTreeSet::from(["body".into()]),
            properties: BTreeMap::from([(
                "body".into(),
                ValueDomain::String {
                    min_length: Some(1),
                    max_length: None,
                    pattern: None,
                    format: None,
                    variants: vec![],
                },
            )]),
            additional: true,
        });
        create.promised_effects.clear();
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![create],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        let events = vec![
            event(
                1,
                "invalid",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "invalid",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "accepted-invalid-input");
    }

    #[test]
    fn schema_formats_do_not_reject_valid_edge_cases() {
        let unbounded = ValueDomain::Integer {
            min: None,
            max: None,
        };
        assert!(unbounded.mismatch(&json!(u64::MAX), "$value").is_none());
        assert!(matches_format("date-time", "2026-07-13T03:10:00-07:00"));
        assert!(matches_format("uri", "mailto:person@example.com"));
        assert!(matches_format("email", "\"quoted.local\"@example.test"));
    }

    #[test]
    fn redacted_metadata_preserves_type_and_length_without_content_claims() {
        let secret = json!({"$reproit":{"redacted":true,"type":"string","length":8}});
        let domain = ValueDomain::String {
            min_length: Some(8),
            max_length: Some(12),
            pattern: Some("^visible-content$".into()),
            format: Some("email".into()),
            variants: vec!["visible-content".into()],
        };
        assert!(domain.mismatch(&secret, "$secret").is_none());
        let short = json!({"$reproit":{"redacted":true,"type":"string","length":2}});
        assert_eq!(
            domain.mismatch(&short, "$secret"),
            Some("$secret is shorter than its minimum".into())
        );
        let wrong = json!({"$reproit":{"redacted":true,"type":"boolean"}});
        assert_eq!(
            domain.mismatch(&wrong, "$secret"),
            Some("$secret must be string".into())
        );
    }

    #[test]
    fn marker_parser_abstains_when_recognized_evidence_is_malformed() {
        let log = concat!(
            "unrelated output\n",
            "REPROIT:BACKEND not-json\n",
            "flutter: REPROIT:BACKEND \
             {\"sequence\":1,\"traceId\":\"t\",\"spanId\":\"s\",\"operation\":\"op\",\"kind\":\"\
             start\",\"input\":{}}\n"
        );
        let events = parse_events(log);
        assert!(events.is_empty());
    }

    #[test]
    fn validation_fixture_loads_and_merges_declared_and_schema_contracts() {
        let mut config: BackendConfig = serde_yaml::from_str(include_str!(
            "../../../../../validation/backend/backend-contract.yaml"
        ))
        .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        config.load_schemas(&root).unwrap();
        assert_eq!(config.operations.len(), 1);
        let operation = &config.operations[0];
        assert_eq!(operation.authority, Authority::Declared);
        assert_eq!(operation.success_statuses, [201]);
        assert!(operation.input.is_some());
        assert!(operation.output.is_some());
        assert_eq!(operation.promised_effects.len(), 2);
        assert_eq!(config.programs.len(), 1);
    }

    #[test]
    fn adversarial_service_fixtures_have_zero_clean_false_positives() {
        let mut config: BackendConfig = serde_yaml::from_str(include_str!(
            "../../../../../validation/backend/backend-contract.yaml"
        ))
        .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        config.load_schemas(&root).unwrap();

        let clean = evaluate(
            &config,
            &parse_events(include_str!(
                "../../../../../validation/backend/clean.ndjson"
            )),
        );
        assert!(clean.is_empty(), "clean fixture produced {clean:?}");

        for (log, expected, action) in [
            (
                include_str!("../../../../../validation/backend/broken-response.ndjson"),
                "response-shape",
                2,
            ),
            (
                include_str!("../../../../../validation/backend/broken-tenant.ndjson"),
                "tenant-isolation",
                3,
            ),
            (
                include_str!("../../../../../validation/backend/broken-duplicate.ndjson"),
                "excess-effect",
                4,
            ),
        ] {
            let violations = evaluate(&config, &parse_events(log));
            assert_eq!(
                violations.len(),
                1,
                "expected {expected}, got {violations:?}"
            );
            assert_eq!(violations[0].oracle, expected);
            assert_eq!(violations[0].action_index, action);
        }
    }

    #[test]
    fn reproit_cloud_schema_and_trace_contracts_catch_json_drift() {
        let mut config: BackendConfig = serde_yaml::from_str(include_str!(
            "../../../../../validation/backend/cloud-contract.yaml"
        ))
        .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        config.load_schemas(&root).unwrap();

        assert_eq!(config.operations.len(), 5);
        let project = config
            .operations
            .iter()
            .find(|operation| operation.id == "cloudCreateProject")
            .unwrap();
        assert_eq!(project.authority, Authority::Declared);
        assert_eq!(project.success_statuses, [201]);
        assert!(project.input.is_some());
        assert!(project.output.is_some());
        assert!(project.tenant_isolated);

        let clean = evaluate(
            &config,
            &parse_events(include_str!(
                "../../../../../validation/backend/cloud-clean.ndjson"
            )),
        );
        assert!(clean.is_empty(), "cloud clean trace produced {clean:?}");
        let live_signup = evaluate(
            &config,
            &parse_events(include_str!(
                "../../../../../validation/backend/cloud-live-signup-clean.ndjson"
            )),
        );
        assert!(
            live_signup.is_empty(),
            "live Cloud signup trace produced {live_signup:?}"
        );

        for (log, expected, action) in [
            (
                include_str!("../../../../../validation/backend/cloud-broken-shape.ndjson"),
                "response-shape",
                8,
            ),
            (
                include_str!("../../../../../validation/backend/cloud-broken-input.ndjson"),
                "accepted-invalid-input",
                9,
            ),
            (
                include_str!("../../../../../validation/backend/cloud-broken-status.ndjson"),
                "response-status",
                10,
            ),
            (
                include_str!("../../../../../validation/backend/cloud-live-signup-broken.ndjson"),
                "response-shape",
                3,
            ),
        ] {
            let violations = evaluate(&config, &parse_events(log));
            assert_eq!(
                violations.len(),
                1,
                "expected {expected}, got {violations:?}"
            );
            assert_eq!(violations[0].oracle, expected);
            assert_eq!(violations[0].action_index, action);
        }
    }

    #[test]
    fn frozen_guard_preserves_exact_backend_violation_across_trace_positions() {
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![contract()],
            programs: vec![],
            invariants: vec![],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        };
        let original = vec![
            event(
                1,
                "span-a",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "span-a",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m1"})),
                    payload: None,
                },
            ),
            event(
                3,
                "span-a",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let findings = evaluate(&config, &original)
            .iter()
            .map(finding)
            .collect::<Vec<_>>();
        let guard = FrozenBackendGuard::from_findings(&config, &findings).unwrap();
        let mut moved = original;
        for event in &mut moved {
            event.sequence += 40;
            event.trace_id = "different-trace".into();
            event.span_id = "different-span".into();
        }
        let log = moved
            .iter()
            .map(|event| format!("{EVENT_MARKER}{}", serde_json::to_string(event).unwrap()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(guard.reproduces(&log));
    }

    #[test]
    fn authored_invariants_require_a_successful_runtime_witness() {
        let mut config = BackendConfig {
            enabled: true,
            operations: vec![contract()],
            invariants: vec![BackendInvariant::Range {
                operation: "createMessage".into(),
                path: "$.balance".into(),
                min: Some(0.0),
                max: None,
            }],
            ..BackendConfig::default()
        };
        config.operations[0].output = None;
        let events = vec![
            event(
                1,
                "range",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "range",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"balance": -1}),
                    status: Some(201),
                    success: true,
                    effects_complete: false,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "authored-invariant");

        config.invariants.clear();
        assert!(evaluate(&config, &events).is_empty());
    }

    #[test]
    fn financial_transition_and_fleet_invariants_are_structural() {
        let mut operation = contract();
        operation.output = None;
        let config = BackendConfig {
            enabled: true,
            operations: vec![operation],
            invariants: vec![
                BackendInvariant::Conserved {
                    operation: "createMessage".into(),
                    left_path: "$.ledger.debits".into(),
                    right_path: "$.ledger.credits".into(),
                },
                BackendInvariant::Bounded {
                    operation: "createMessage".into(),
                    value_path: "$.account.exposure".into(),
                    limit_path: "$.account.limit".into(),
                },
                BackendInvariant::Transition {
                    operation: "createMessage".into(),
                    path: "$.status".into(),
                    from: "pending".into(),
                    to: vec!["accepted".into(), "rejected".into()],
                },
            ],
            fleet: FleetInvariant {
                same_build: true,
                same_config_contract: true,
            },
            ..BackendConfig::default()
        };
        let mut start = event(
            1,
            "finance",
            "createMessage",
            BackendEventKind::Start {
                input: json!({"status":"pending"}),
            },
        );
        start.build = Some("build-a".into());
        start.config_contract = Some("contract-a".into());
        let mut returned = event(
            2,
            "finance",
            "createMessage",
            BackendEventKind::Return {
                output: json!({
                    "status":"cancelled",
                    "ledger":{"debits":10,"credits":9},
                    "account":{"exposure":11,"limit":10}
                }),
                status: Some(201),
                success: true,
                effects_complete: false,
            },
        );
        returned.build = Some("build-b".into());
        returned.config_contract = Some("contract-b".into());
        let violations = evaluate(&config, &[start, returned]);
        assert_eq!(
            violations
                .iter()
                .filter(|violation| violation.oracle == "authored-invariant")
                .count(),
            3
        );
        assert_eq!(
            violations
                .iter()
                .filter(|violation| violation.oracle == "fleet-consistency")
                .count(),
            2
        );
    }

    #[test]
    fn declarative_backend_invariant_yaml_is_language_independent() {
        let config: BackendConfig = serde_yaml::from_str(
            r#"
enabled: true
invariants:
  - unique: order.id
  - idempotent: submitOrder
  - conserved: ledger.debits == ledger.credits
  - bounded: account.exposure <= account.limit
  - transition: pending -> accepted | rejected
fleet:
  same_build: true
  same_config_contract: true
"#,
        )
        .unwrap();
        assert_eq!(config.invariants.len(), 5);
        assert!(config.fleet.same_build);
        assert!(config.fleet.same_config_contract);
        assert!(matches!(
            &config.invariants[2],
            BackendInvariant::Conserved { left_path, right_path, .. }
                if left_path == "$.ledger.debits" && right_path == "$.ledger.credits"
        ));
        for invariant in &config.invariants {
            let encoded = serde_json::to_value(invariant).unwrap();
            let decoded: BackendInvariant = serde_json::from_value(encoded).unwrap();
            assert_eq!(&decoded, invariant);
        }
    }

    #[test]
    fn unique_invariant_walks_arrays_structurally() {
        let mut operation = contract();
        operation.output = None;
        let config = BackendConfig {
            enabled: true,
            operations: vec![operation],
            invariants: vec![BackendInvariant::Unique {
                operation: "createMessage".into(),
                path: "$.orders.id".into(),
            }],
            ..BackendConfig::default()
        };
        let events = vec![
            event(
                1,
                "unique",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "unique",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"orders":[{"id":"same"},{"id":"same"}]}),
                    status: Some(201),
                    success: true,
                    effects_complete: false,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "authored-invariant");
    }

    #[test]
    fn imports_raw_proto_with_nested_messages() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("reproit-proto-{}-{}", std::process::id(), nonce));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("service.proto");
        std::fs::write(
            &path,
            r#"syntax = "proto3";
package reproit.validation;
message Envelope {
  message Payload { string name = 1; }
  Payload payload = 1;
}
message Reply { string value = 1; }
service Nested { rpc Send(Envelope) returns (Reply); }
"#,
        )
        .unwrap();
        let document = load_service_document(&path).unwrap();
        let operations = import_service_schema(&document);
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].id, "reproit.validation.Nested/Send");
        let input = operations[0].input.as_ref().unwrap();
        let ValueDomain::Object { properties, .. } = input else {
            panic!("expected message object");
        };
        assert!(matches!(
            properties.get("payload"),
            Some(ValueDomain::Object { .. })
        ));
    }

    fn lifecycle_operation(id: &str, status: u16) -> OperationContract {
        OperationContract {
            id: id.into(),
            authority: Authority::Declared,
            input: None,
            output: None,
            outputs_by_status: BTreeMap::new(),
            success_statuses: vec![status],
            read_only: id == "getOrder",
            idempotent: false,
            idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
            tenant_isolated: false,
            promised_effects: vec![],
        }
    }

    fn lifecycle_resource(consistency: ResourceConsistency) -> ResourceLifecycleContract {
        ResourceLifecycleContract {
            name: "order".into(),
            consistency,
            create: ResourceCreateContract {
                operation: "createOrder".into(),
                output_identity_path: "$.id".into(),
            },
            read: ResourceReadContract {
                operation: "getOrder".into(),
                input_identity_path: "$.id".into(),
                output_identity_path: "$.id".into(),
                absent_statuses: vec![404],
            },
            update: Some(ResourceMutationContract {
                operation: "updateOrder".into(),
                input_identity_path: "$.id".into(),
            }),
            delete: Some(ResourceMutationContract {
                operation: "deleteOrder".into(),
                input_identity_path: "$.id".into(),
            }),
            fields: vec![ResourceFieldContract {
                read_output_path: "$.status".into(),
                create_output_path: Some("$.status".into()),
                update_input_path: Some("$.status".into()),
            }],
        }
    }

    fn lifecycle_config(consistency: ResourceConsistency) -> BackendConfig {
        BackendConfig {
            enabled: true,
            operations: vec![
                lifecycle_operation("createOrder", 201),
                lifecycle_operation("getOrder", 200),
                lifecycle_operation("updateOrder", 200),
                lifecycle_operation("deleteOrder", 204),
            ],
            resources: vec![lifecycle_resource(consistency)],
            ..BackendConfig::default()
        }
    }

    #[test]
    fn lifecycle_contract_yaml_is_language_independent_and_strict() {
        let config: BackendConfig = serde_yaml::from_str(
            r#"
enabled: true
resources:
  - name: order
    consistency: strong
    create: { operation: createOrder, outputIdentityPath: $.id }
    read:
      operation: getOrder
      inputIdentityPath: $.path.id
      outputIdentityPath: $.id
      absentStatuses: [404]
    update: { operation: updateOrder, inputIdentityPath: $.path.id }
    delete: { operation: deleteOrder, inputIdentityPath: $.path.id }
    fields:
      - createOutputPath: $.status
        updateInputPath: $.body.status
        readOutputPath: $.status
"#,
        )
        .unwrap();
        assert_eq!(config.resources.len(), 1);
        assert_eq!(config.resources[0].consistency, ResourceConsistency::Strong);
        assert_eq!(config.resources[0].read.absent_statuses, [404]);
        assert!(serde_yaml::from_str::<BackendConfig>(
            r#"
enabled: true
resources:
  - name: order
    consistency: guessed
    create: { operation: createOrder, outputIdentityPath: $.id }
    read: { operation: getOrder, inputIdentityPath: $.id, outputIdentityPath: $.id }
"#,
        )
        .is_err());
    }

    fn invocation_events(
        sequence: u64,
        span: &str,
        operation: &str,
        input: Value,
        status: u16,
        success: bool,
        output: Value,
    ) -> Vec<BackendEvent> {
        vec![
            event(sequence, span, operation, BackendEventKind::Start { input }),
            event(
                sequence + 1,
                span,
                operation,
                BackendEventKind::Return {
                    output,
                    status: Some(status),
                    success,
                    effects_complete: false,
                },
            ),
        ]
    }

    #[test]
    fn lifecycle_proves_create_and_update_read_contradictions() {
        let config = lifecycle_config(ResourceConsistency::Strong);
        let mut create_read = invocation_events(
            1,
            "create",
            "createOrder",
            json!({"status":"pending"}),
            201,
            true,
            json!({"id":"o1","status":"pending"}),
        );
        create_read.extend(invocation_events(
            3,
            "read",
            "getOrder",
            json!({"id":"o1"}),
            200,
            true,
            json!({"id":"o1","status":"cancelled"}),
        ));
        assert!(evaluate(&config, &create_read)
            .iter()
            .any(|violation| violation.oracle == "resource-state"));

        let mut update_read = invocation_events(
            1,
            "create",
            "createOrder",
            json!({}),
            201,
            true,
            json!({"id":"o1","status":"pending"}),
        );
        update_read.extend(invocation_events(
            3,
            "update",
            "updateOrder",
            json!({"id":"o1","status":"accepted"}),
            200,
            true,
            json!({"id":"o1"}),
        ));
        update_read.extend(invocation_events(
            5,
            "read",
            "getOrder",
            json!({"id":"o1"}),
            200,
            true,
            json!({"id":"o1","status":"pending"}),
        ));
        assert!(evaluate(&config, &update_read)
            .iter()
            .any(|violation| violation.oracle == "resource-state"));
    }

    #[test]
    fn lifecycle_proves_missing_create_and_visible_delete() {
        let config = lifecycle_config(ResourceConsistency::Strong);
        let mut missing = invocation_events(
            1,
            "create",
            "createOrder",
            json!({}),
            201,
            true,
            json!({"id":"o1","status":"pending"}),
        );
        missing.extend(invocation_events(
            3,
            "read",
            "getOrder",
            json!({"id":"o1"}),
            404,
            false,
            json!({}),
        ));
        assert!(evaluate(&config, &missing)
            .iter()
            .any(|violation| violation.oracle == "resource-create-missing"));

        let mut visible = invocation_events(
            1,
            "create",
            "createOrder",
            json!({}),
            201,
            true,
            json!({"id":"o1","status":"pending"}),
        );
        visible.extend(invocation_events(
            3,
            "delete",
            "deleteOrder",
            json!({"id":"o1"}),
            204,
            true,
            Value::Null,
        ));
        visible.extend(invocation_events(
            5,
            "read",
            "getOrder",
            json!({"id":"o1"}),
            200,
            true,
            json!({"id":"o1","status":"pending"}),
        ));
        assert!(evaluate(&config, &visible)
            .iter()
            .any(|violation| violation.oracle == "resource-delete-visible"));
    }

    #[test]
    fn lifecycle_abstains_for_eventual_or_ambiguous_identity() {
        let mut events = invocation_events(
            1,
            "create",
            "createOrder",
            json!({}),
            201,
            true,
            json!({"id":["o1"],"status":"pending"}),
        );
        events.extend(invocation_events(
            3,
            "read",
            "getOrder",
            json!({"id":"o1"}),
            404,
            false,
            json!({}),
        ));
        assert!(evaluate(&lifecycle_config(ResourceConsistency::Strong), &events).is_empty());

        let mut eventual = events;
        eventual[1].event = BackendEventKind::Return {
            output: json!({"id":"o1","status":"pending"}),
            status: Some(201),
            success: true,
            effects_complete: false,
        };
        assert!(evaluate(&lifecycle_config(ResourceConsistency::Eventual), &eventual).is_empty());
    }

    fn query_operation(id: &str) -> OperationContract {
        let mut operation = contract();
        operation.id = id.into();
        operation.output = None;
        operation.success_statuses = vec![200];
        operation.read_only = true;
        operation.idempotent = false;
        operation.tenant_isolated = false;
        operation.promised_effects.clear();
        operation
    }

    fn query_invariant(consistency: ResourceConsistency) -> BackendInvariant {
        BackendInvariant::QuerySemantics {
            operation: "listItems".into(),
            items_path: "$.items".into(),
            identity_path: "$.id".into(),
            consistency,
            filters: vec![QueryFilterContract {
                input_path: "$.status".into(),
                item_path: "$.status".into(),
                comparison: QueryComparison::Equal,
            }],
            sort: Some(QuerySortContract {
                item_path: "$.rank".into(),
                direction: QuerySortDirection::Ascending,
                value_type: QuerySortType::Number,
            }),
            limit_input_path: Some("$.limit".into()),
            pagination: Some(QueryPaginationContract {
                cursor_input_path: "$.cursor".into(),
                next_cursor_output_path: "$.nextCursor".into(),
                snapshot_input_path: "$.snapshot".into(),
                reference_operation: Some("listAllItems".into()),
            }),
        }
    }

    fn query_config(consistency: ResourceConsistency) -> BackendConfig {
        BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![
                query_operation("listItems"),
                query_operation("listAllItems"),
            ],
            programs: vec![],
            invariants: vec![query_invariant(consistency)],
            resources: vec![],
            proofs: vec![],
            fleet: FleetInvariant::default(),
        }
    }

    fn query_invocation(
        sequence: u64,
        span: &str,
        operation: &str,
        input: Value,
        output: Value,
    ) -> Vec<BackendEvent> {
        vec![
            event(sequence, span, operation, BackendEventKind::Start { input }),
            event(
                sequence + 1,
                span,
                operation,
                BackendEventKind::Return {
                    output,
                    status: Some(200),
                    success: true,
                    effects_complete: true,
                },
            ),
        ]
    }

    #[test]
    fn query_filter_sort_and_limit_need_authored_typed_contradictions() {
        let config = query_config(ResourceConsistency::Strong);
        let input = json!({"status":"open","limit":2,"snapshot":"r1","cursor":null});
        let clean = query_invocation(
            1,
            "clean",
            "listItems",
            input.clone(),
            json!({"items":[
                {"id":"a","status":"open","rank":1},
                {"id":"b","status":"open","rank":2}
            ],"nextCursor":null}),
        );
        assert!(evaluate(&config, &clean).is_empty());

        let filter = query_invocation(
            1,
            "filter",
            "listItems",
            input.clone(),
            json!({"items":[{"id":"a","status":"closed","rank":1}],"nextCursor":null}),
        );
        assert!(evaluate(&config, &filter)
            .iter()
            .any(|violation| violation.oracle == "authored-invariant"));

        let sort = query_invocation(
            1,
            "sort",
            "listItems",
            input.clone(),
            json!({"items":[
                {"id":"a","status":"open","rank":2},
                {"id":"b","status":"open","rank":1}
            ],"nextCursor":null}),
        );
        assert!(evaluate(&config, &sort)
            .iter()
            .any(|violation| violation.reason.contains("Ascending order")));

        let limit = query_invocation(
            1,
            "limit",
            "listItems",
            input,
            json!({"items":[
                {"id":"a","status":"open","rank":1},
                {"id":"b","status":"open","rank":2},
                {"id":"c","status":"open","rank":3}
            ],"nextCursor":null}),
        );
        assert!(evaluate(&config, &limit)
            .iter()
            .any(|violation| violation.reason.contains("exceeded the authored limit")));
    }

    #[test]
    fn query_semantics_abstain_when_types_paths_or_snapshot_are_unknown() {
        let config = query_config(ResourceConsistency::Strong);
        let missing_typed_sort = query_invocation(
            1,
            "missing",
            "listItems",
            json!({"status":"open","limit":2,"snapshot":"r1","cursor":null}),
            json!({"items":[{"id":"a","status":"open","rank":"first"}],"nextCursor":null}),
        );
        assert!(evaluate(&config, &missing_typed_sort).is_empty());

        let mut pages = query_invocation(
            1,
            "one",
            "listItems",
            json!({"status":"open","limit":1,"cursor":null}),
            json!({"items":[{"id":"a","status":"open","rank":1}],"nextCursor":"c1"}),
        );
        pages.extend(query_invocation(
            3,
            "two",
            "listItems",
            json!({"status":"open","limit":1,"cursor":"c1"}),
            json!({"items":[{"id":"a","status":"open","rank":1}],"nextCursor":null}),
        ));
        assert!(evaluate(&config, &pages).is_empty());
        assert!(evaluate(&query_config(ResourceConsistency::Eventual), &pages).is_empty());
    }

    #[test]
    fn pinned_pagination_proves_duplicate_cursor_and_reference_failures() {
        let config = query_config(ResourceConsistency::Strong);
        let page = |sequence, span: &str, cursor: Value, id: &str, next: Value| {
            query_invocation(
                sequence,
                span,
                "listItems",
                json!({"status":"open","limit":1,"snapshot":"r1","cursor":cursor}),
                json!({"items":[{"id":id,"status":"open","rank":sequence}],"nextCursor":next}),
            )
        };

        let mut clean = page(1, "clean-one", Value::Null, "a", json!("c1"));
        clean.extend(page(3, "clean-two", json!("c1"), "b", Value::Null));
        clean.extend(query_invocation(
            5,
            "clean-reference",
            "listAllItems",
            json!({"status":"open","snapshot":"r1"}),
            json!({"items":[
                {"id":"a","status":"open","rank":1},
                {"id":"b","status":"open","rank":3}
            ]}),
        ));
        assert!(evaluate(&config, &clean).is_empty());

        let mut duplicate = page(1, "one", Value::Null, "a", json!("c1"));
        duplicate.extend(page(3, "two", json!("c1"), "a", Value::Null));
        assert!(evaluate(&config, &duplicate)
            .iter()
            .any(|violation| violation.reason.contains("duplicate identity")));

        let mut looped = page(1, "one", Value::Null, "a", json!("c1"));
        looped.extend(page(3, "two", json!("c1"), "b", json!("c1")));
        assert!(evaluate(&config, &looped)
            .iter()
            .any(|violation| violation.reason.contains("without progress")));

        let mut mismatch = page(1, "one", Value::Null, "a", json!("c1"));
        mismatch.extend(page(3, "two", json!("c1"), "b", Value::Null));
        mismatch.extend(query_invocation(
            5,
            "reference",
            "listAllItems",
            json!({"status":"open","snapshot":"r1"}),
            json!({"items":[
                {"id":"a","status":"open","rank":1},
                {"id":"c","status":"open","rank":3}
            ]}),
        ));
        let violations = evaluate(&config, &mismatch);
        assert!(violations
            .iter()
            .any(|violation| violation.oracle == "query-pagination-reference"));
        let findings = violations.iter().map(finding).collect::<Vec<_>>();
        let guard = FrozenBackendGuard::from_findings(&config, &findings).unwrap();
        let log = mismatch
            .iter()
            .map(|event| format!("{EVENT_MARKER}{}", serde_json::to_string(event).unwrap()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(guard.reproduces(&log));
    }

    #[test]
    fn query_contract_yaml_is_language_independent_and_strict() {
        let config: BackendConfig = serde_yaml::from_str(
            r#"
enabled: true
invariants:
  - kind: query-semantics
    operation: listOrders
    itemsPath: $.items
    identityPath: $.id
    consistency: strong
    filters:
      - inputPath: $.query.status
        itemPath: $.status
        comparison: equal
    sort:
      itemPath: $.createdAt
      direction: descending
      valueType: string
    limitInputPath: $.query.limit
    pagination:
      cursorInputPath: $.query.cursor
      nextCursorOutputPath: $.nextCursor
      snapshotInputPath: $.query.revision
      referenceOperation: listAllOrders
"#,
        )
        .unwrap();
        assert!(matches!(
            config.invariants.as_slice(),
            [BackendInvariant::QuerySemantics { .. }]
        ));
        assert!(serde_yaml::from_str::<BackendConfig>(
            r#"
enabled: true
invariants:
  - kind: query-semantics
    operation: listOrders
    itemsPath: $.items
    identityPath: $.id
    filters:
      - inputPath: $.query.status
        itemPath: $.status
        comparison: contains
"#,
        )
        .is_err());
        assert!(serde_yaml::from_str::<BackendConfig>(
            r#"
enabled: true
invariants:
  - kind: query-semantics
    operation: listOrders
    itemsPath: $.items
    identityPath: $.id
    inferredParameterNames: true
"#,
        )
        .is_err());
    }

    #[test]
    fn query_semantics_abstain_for_inferred_contracts_and_mixed_sessions() {
        let mut config = query_config(ResourceConsistency::Strong);
        config.operations[0].authority = Authority::Inferred;
        let bad_filter = query_invocation(
            1,
            "filter",
            "listItems",
            json!({"status":"open","limit":1,"snapshot":"r1","cursor":null}),
            json!({"items":[{"id":"a","status":"closed","rank":1}],"nextCursor":null}),
        );
        assert!(evaluate(&config, &bad_filter).is_empty());

        let config = query_config(ResourceConsistency::Strong);
        let mut mixed = query_invocation(
            1,
            "one",
            "listItems",
            json!({"status":"open","limit":1,"snapshot":"r1","cursor":null}),
            json!({"items":[{"id":"a","status":"open","rank":1}],"nextCursor":"c1"}),
        );
        mixed.extend(query_invocation(
            3,
            "other-query",
            "listItems",
            json!({"status":"closed","limit":1,"snapshot":"r1","cursor":"c1"}),
            json!({"items":[{"id":"a","status":"closed","rank":1}],"nextCursor":null}),
        ));
        assert!(evaluate(&config, &mixed).is_empty());
    }

    fn proof_operation(id: &str) -> OperationContract {
        let mut operation = query_operation(id);
        operation.success_statuses = vec![200];
        operation.read_only = false;
        operation
    }

    fn principal_event(
        sequence: u64,
        span: &str,
        operation: &str,
        actor: &str,
        tenant: &str,
        kind: BackendEventKind,
    ) -> BackendEvent {
        let mut event = event(sequence, span, operation, kind);
        event.actor = Some(actor.into());
        event.tenant = Some(tenant.into());
        event
    }

    fn return_kind(output: Value, status: u16, success: bool, complete: bool) -> BackendEventKind {
        BackendEventKind::Return {
            output,
            status: Some(status),
            success,
            effects_complete: complete,
        }
    }

    fn effect_invocation(events: &[BackendEvent]) -> Invocation<'_> {
        let effects = events
            .iter()
            .filter_map(|event| match &event.event {
                BackendEventKind::Effect {
                    effect,
                    resource,
                    key,
                    tenant,
                    event: emitted,
                    before,
                    after,
                    ..
                } => Some(EffectEvent {
                    event,
                    effect: *effect,
                    resource: resource.as_deref(),
                    key: key.as_deref(),
                    tenant: tenant.as_deref(),
                    emitted: emitted.as_deref(),
                    before: before.as_ref(),
                    after: after.as_ref(),
                }),
                _ => None,
            })
            .collect();
        Invocation {
            effects,
            ..Invocation::default()
        }
    }

    #[test]
    fn authorization_matrix_accepts_authored_denials_and_proves_data_disclosure() {
        let proof = BackendProofContract::AuthorizationMatrix {
            operation: "getOrder".into(),
            identity_input_path: "$.id".into(),
            snapshot_input_path: "$.revision".into(),
            consistency: ResourceConsistency::Strong,
            principals: vec![
                AuthorizationPrincipal {
                    actor: "alice".into(),
                    tenant: "tenant-a".into(),
                    decision: AuthorizationDecision::Allow,
                },
                AuthorizationPrincipal {
                    actor: "bob".into(),
                    tenant: "tenant-b".into(),
                    decision: AuthorizationDecision::Deny,
                },
            ],
            deny: AuthorizationDenyPolicy {
                statuses: vec![401, 403, 404],
                redacted_output_paths: vec!["$.secret".into()],
            },
        };
        let config = BackendConfig {
            enabled: true,
            operations: vec![proof_operation("getOrder")],
            proofs: vec![proof],
            ..BackendConfig::default()
        };
        let input = json!({"id":"o1","revision":"r1"});
        let allowed = vec![
            principal_event(
                1,
                "allow",
                "getOrder",
                "alice",
                "tenant-a",
                BackendEventKind::Start {
                    input: input.clone(),
                },
            ),
            principal_event(
                2,
                "allow",
                "getOrder",
                "alice",
                "tenant-a",
                return_kind(json!({"secret":"value"}), 200, true, true),
            ),
        ];
        let mut denied = allowed.clone();
        denied.extend([
            principal_event(
                3,
                "deny",
                "getOrder",
                "bob",
                "tenant-b",
                BackendEventKind::Start {
                    input: input.clone(),
                },
            ),
            principal_event(
                4,
                "deny",
                "getOrder",
                "bob",
                "tenant-b",
                return_kind(json!({"secret":"leaked"}), 200, true, true),
            ),
        ]);
        assert!(evaluate(&config, &denied)
            .iter()
            .any(|violation| violation.oracle == "authorization-matrix"));

        let mut hidden = allowed.clone();
        hidden.extend([
            principal_event(
                3,
                "deny",
                "getOrder",
                "bob",
                "tenant-b",
                BackendEventKind::Start {
                    input: input.clone(),
                },
            ),
            principal_event(
                4,
                "deny",
                "getOrder",
                "bob",
                "tenant-b",
                return_kind(json!({}), 404, false, true),
            ),
        ]);
        assert!(evaluate(&config, &hidden).is_empty());

        let mut redacted = allowed;
        redacted.extend([
            principal_event(
                3,
                "deny",
                "getOrder",
                "bob",
                "tenant-b",
                BackendEventKind::Start { input },
            ),
            principal_event(
                4,
                "deny",
                "getOrder",
                "bob",
                "tenant-b",
                return_kind(json!({"secret":null}), 200, true, true),
            ),
        ]);
        assert!(evaluate(&config, &redacted).is_empty());
    }

    #[test]
    fn transaction_atomicity_proves_partial_commit_and_accepts_exact_rollback() {
        let proof = BackendProofContract::TransactionAtomicity {
            operation: "transfer".into(),
            identity_input_path: "$.account".into(),
            snapshot_input_path: "$.revision".into(),
            consistency: ResourceConsistency::Strong,
            failure: ControlledFailureWitness {
                input_path: "$.failAt".into(),
                value: json!("after-debit"),
                statuses: vec![409],
            },
            durable_effects: vec![EffectPattern {
                kind: EffectKind::Write,
                resource: Some("ledger".into()),
                event: None,
                at_least: 0,
                at_most: None,
            }],
        };
        let config = BackendConfig {
            enabled: true,
            operations: vec![proof_operation("transfer")],
            proofs: vec![proof],
            ..BackendConfig::default()
        };
        let start = event(
            1,
            "tx",
            "transfer",
            BackendEventKind::Start {
                input: json!({"account":"a1","revision":"r1","failAt":"after-debit"}),
            },
        );
        let mut partial_write = event(
            2,
            "tx",
            "transfer",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("ledger".into()),
                key: Some("entry-1".into()),
                tenant: None,
                event: None,
                before: Some(json!({"amount":20})),
                after: Some(json!({"amount":10})),
                payload: None,
            },
        );
        partial_write.action_index = 1;
        let failed = event(
            3,
            "tx",
            "transfer",
            return_kind(json!({}), 409, false, true),
        );
        let partial = vec![start.clone(), partial_write.clone(), failed.clone()];
        let BackendProofContract::TransactionAtomicity {
            durable_effects, ..
        } = &config.proofs[0]
        else {
            unreachable!()
        };
        assert!(matches!(
            failed_atomicity_effect_outcome(&effect_invocation(&partial), durable_effects),
            AtomicityEffectOutcome::Violation(_)
        ));
        let findings = evaluate(&config, &partial);
        let violation = findings
            .iter()
            .find(|violation| violation.oracle == "transaction-atomicity")
            .expect("partial commit should be proven");
        assert_eq!(violation.action_index, 1);

        let rollback = event(
            3,
            "tx",
            "transfer",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("ledger".into()),
                key: Some("entry-1".into()),
                tenant: None,
                event: None,
                before: Some(json!({"amount":10})),
                after: Some(json!({"amount":20})),
                payload: None,
            },
        );
        let mut rolled_back_return = failed.clone();
        rolled_back_return.sequence = 4;
        let rolled_back = vec![
            start.clone(),
            partial_write.clone(),
            rollback,
            rolled_back_return,
        ];
        assert!(matches!(
            failed_atomicity_effect_outcome(&effect_invocation(&rolled_back), durable_effects),
            AtomicityEffectOutcome::Satisfied
        ));
        assert!(evaluate(&config, &rolled_back).is_empty());

        assert!(matches!(
            failed_atomicity_effect_outcome(&effect_invocation(&[]), durable_effects),
            AtomicityEffectOutcome::Satisfied
        ));
        assert!(evaluate(&config, &[start.clone(), failed.clone()]).is_empty());

        let mut missing_before = partial_write.clone();
        if let BackendEventKind::Effect { before, .. } = &mut missing_before.event {
            *before = None;
        }
        assert!(matches!(
            failed_atomicity_effect_outcome(
                &effect_invocation(std::slice::from_ref(&missing_before)),
                durable_effects,
            ),
            AtomicityEffectOutcome::Abstain
        ));
        assert!(evaluate(&config, &[start.clone(), missing_before, failed.clone()]).is_empty());

        let incomplete = event(
            3,
            "tx",
            "transfer",
            return_kind(json!({}), 409, false, false),
        );
        assert!(evaluate(&config, &[start.clone(), partial_write, incomplete]).is_empty());

        let finding_values = findings.iter().map(finding).collect::<Vec<_>>();
        let guard = FrozenBackendGuard::from_findings(&config, &finding_values).unwrap();
        assert_eq!(guard.proofs, config.proofs);
        let log = partial
            .iter()
            .map(|event| format!("{EVENT_MARKER}{}", serde_json::to_string(event).unwrap()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(guard.reproduces(&log));

        let mut schema_owned = config;
        schema_owned.operations[0].authority = Authority::Schema;
        assert!(evaluate(&schema_owned, &partial).is_empty());
    }

    fn concurrent_events(second_success: bool, stale_second_write: bool) -> Vec<BackendEvent> {
        let mut events = vec![
            principal_event(
                1,
                "left",
                "updateBalance",
                "alice",
                "tenant-a",
                BackendEventKind::Start {
                    input: json!({"id":"a1","snapshot":"s1","version":1,"delta":1}),
                },
            ),
            principal_event(
                2,
                "right",
                "updateBalance",
                "bob",
                "tenant-a",
                BackendEventKind::Start {
                    input: json!({"id":"a1","snapshot":"s1","version":1,"delta":1}),
                },
            ),
            principal_event(
                3,
                "left",
                "updateBalance",
                "alice",
                "tenant-a",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("accounts".into()),
                    key: Some("a1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: Some(json!({"balance":10})),
                    after: Some(json!({"balance":11})),
                    payload: None,
                },
            ),
            principal_event(
                4,
                "left",
                "updateBalance",
                "alice",
                "tenant-a",
                return_kind(json!({"version":2}), 200, true, true),
            ),
        ];
        if second_success {
            events.push(principal_event(
                5,
                "right",
                "updateBalance",
                "bob",
                "tenant-a",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("accounts".into()),
                    key: Some("a1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: Some(json!({"balance":if stale_second_write {10} else {11}})),
                    after: Some(json!({"balance":if stale_second_write {11} else {12}})),
                    payload: None,
                },
            ));
        }
        events.push(principal_event(
            6,
            "right",
            "updateBalance",
            "bob",
            "tenant-a",
            if second_success {
                return_kind(json!({"version":2}), 200, true, true)
            } else {
                return_kind(json!({}), 409, false, true)
            },
        ));
        events
    }

    #[test]
    fn concurrency_contracts_prove_double_commit_and_lost_conservation() {
        let base = BackendConfig {
            enabled: true,
            operations: vec![proof_operation("updateBalance")],
            ..BackendConfig::default()
        };
        let optimistic = BackendProofContract::ConcurrentUpdate {
            operation: "updateBalance".into(),
            identity_input_path: "$.id".into(),
            snapshot_input_path: "$.snapshot".into(),
            consistency: ResourceConsistency::Strong,
            policy: ConcurrencyPolicy::OptimisticVersion {
                resource: "accounts".into(),
                version_input_path: "$.version".into(),
                conflict_statuses: vec![409, 412],
            },
        };
        let mut config = base.clone();
        config.proofs = vec![optimistic];
        assert!(evaluate(&config, &concurrent_events(true, true))
            .iter()
            .any(|violation| violation.oracle == "concurrent-update"));
        let conflict = evaluate(&config, &concurrent_events(false, false));
        assert!(conflict.is_empty(), "{conflict:?}");

        config.proofs = vec![BackendProofContract::ConcurrentUpdate {
            operation: "updateBalance".into(),
            identity_input_path: "$.id".into(),
            snapshot_input_path: "$.snapshot".into(),
            consistency: ResourceConsistency::Strong,
            policy: ConcurrencyPolicy::Conserved {
                resource: "accounts".into(),
                delta_input_path: "$.delta".into(),
                before_path: "$.balance".into(),
                after_path: "$.balance".into(),
            },
        }];
        assert!(evaluate(&config, &concurrent_events(true, true))
            .iter()
            .any(|violation| violation.oracle == "concurrent-conservation"));
        assert!(evaluate(&config, &concurrent_events(true, false)).is_empty());
    }

    #[test]
    fn round_trip_integrity_is_typed_exact_and_frozen_for_replay() {
        let proof = BackendProofContract::ResourceRoundTrip {
            write_operation: "putBlob".into(),
            read_operation: "getBlob".into(),
            write_identity_output_path: "$.id".into(),
            read_identity_input_path: "$.id".into(),
            write_snapshot_output_path: "$.revision".into(),
            read_snapshot_input_path: "$.revision".into(),
            consistency: ResourceConsistency::Strong,
            checks: vec![
                RoundTripCheck::Exact {
                    write_input_path: "$.content".into(),
                    read_output_path: "$.content".into(),
                },
                RoundTripCheck::Utf8Sha256 {
                    write_input_path: "$.content".into(),
                    read_hash_output_path: "$.sha256".into(),
                },
                RoundTripCheck::ByteSize {
                    write_input_path: "$.content".into(),
                    read_size_output_path: "$.size".into(),
                },
                RoundTripCheck::MediaType {
                    write_input_path: "$.mediaType".into(),
                    read_output_path: "$.mediaType".into(),
                },
            ],
        };
        let config = BackendConfig {
            enabled: true,
            operations: vec![proof_operation("putBlob"), proof_operation("getBlob")],
            proofs: vec![proof],
            ..BackendConfig::default()
        };
        let mut events = query_invocation(
            1,
            "write",
            "putBlob",
            json!({"content":"hello","mediaType":"text/plain"}),
            json!({"id":"b1","revision":"r1"}),
        );
        events.extend(query_invocation(
            3,
            "read",
            "getBlob",
            json!({"id":"b1","revision":"r1"}),
            json!({
                "content":"hell0",
                "sha256":hash(b"hello"),
                "size":5,
                "mediaType":"text/plain"
            }),
        ));
        let violations = evaluate(&config, &events);
        assert!(violations
            .iter()
            .any(|violation| violation.oracle == "resource-round-trip"));
        let findings = violations.iter().map(finding).collect::<Vec<_>>();
        let guard = FrozenBackendGuard::from_findings(&config, &findings).unwrap();
        assert_eq!(guard.proofs.len(), 1);
        let log = events
            .iter()
            .map(|event| format!("{EVENT_MARKER}{}", serde_json::to_string(event).unwrap()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(guard.reproduces(&log));

        let clean_hash = hash(b"hello");
        if let BackendEventKind::Return { output, .. } = &mut events[3].event {
            *output = json!({
                "content":"hello",
                "sha256":clean_hash,
                "size":5,
                "mediaType":"text/plain"
            });
        }
        assert!(evaluate(&config, &events).is_empty());
    }

    #[test]
    fn proof_contract_yaml_is_strict_and_transport_independent() {
        let config: BackendConfig = serde_yaml::from_str(
            r#"
enabled: true
proofs:
  - kind: authorization-matrix
    operation: GetOrder
    identityInputPath: $.request.id
    snapshotInputPath: $.request.revision
    consistency: strong
    principals:
      - { actor: owner, tenant: team-a, decision: allow }
      - { actor: outsider, tenant: team-b, decision: deny }
    deny:
      statuses: [401, 403, 404]
      redactedOutputPaths: [$.response.secret]
  - kind: transaction-atomicity
    operation: Transfer
    identityInputPath: $.request.account
    snapshotInputPath: $.request.revision
    consistency: strong
    failure:
      inputPath: $.request.failAt
      value: after-debit
      statuses: [409]
    durableEffects:
      - { kind: write, resource: ledger, atLeast: 0 }
  - kind: concurrent-update
    operation: UpdateOrder
    identityInputPath: $.request.id
    snapshotInputPath: $.request.snapshot
    consistency: strong
    policy:
      kind: optimistic-version
      resource: orders
      versionInputPath: $.request.version
      conflictStatuses: [409, 412]
  - kind: resource-round-trip
    writeOperation: PutBlob
    readOperation: GetBlob
    writeIdentityOutputPath: $.response.id
    readIdentityInputPath: $.request.id
    writeSnapshotOutputPath: $.response.revision
    readSnapshotInputPath: $.request.revision
    consistency: strong
    checks:
      - { kind: exact, writeInputPath: $.request.name, readOutputPath: $.response.name }
"#,
        )
        .unwrap();
        assert_eq!(config.proofs.len(), 4);
        assert!(serde_yaml::from_str::<BackendConfig>(
            r#"
enabled: true
proofs:
  - kind: authorization-matrix
    operation: GetOrder
    identityInputPath: $.id
    snapshotInputPath: $.revision
    consistency: strong
    guessedRole: admin
    principals: []
    deny: { statuses: [403] }
"#,
        )
        .is_err());
    }

    #[test]
    fn local_atomicity_yaml_fixture_is_proven_and_frozen_for_replay() {
        let config: BackendConfig = serde_yaml::from_str(include_str!(
            "../../../../../validation/backend/atomicity-contract.yaml"
        ))
        .unwrap();
        let log = include_str!("../../../../../validation/backend/atomicity-partial-commit.ndjson");
        let violations = evaluate(&config, &parse_events(log));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "transaction-atomicity");
        assert_eq!(violations[0].action_index, 3);

        let findings = violations.iter().map(finding).collect::<Vec<_>>();
        let guard = FrozenBackendGuard::from_findings(&config, &findings).unwrap();
        assert_eq!(guard.operations, config.operations);
        assert_eq!(guard.proofs, config.proofs);
        assert!(guard.reproduces(log));
    }

    #[test]
    fn openapi_parameter_uniqueness_resolves_refs_and_allows_operation_override() {
        let document = json!({
            "openapi": "3.1.0",
            "components": { "parameters": {
                "Id": {
                    "name": "id",
                    "in": "path",
                    "required": true,
                    "schema": {"type": "string"}
                },
                "IdAlias": { "$ref": "#/components/parameters/Id" }
            }},
            "paths": { "/items/{id}": {
                "parameters": [{ "$ref": "#/components/parameters/Id" }],
                "get": {
                    "operationId": "getItem",
                    "parameters": [{ "$ref": "#/components/parameters/IdAlias" }],
                    "responses": { "200": { "description": "ok" } }
                }
            }}
        });
        assert!(validate_openapi_parameter_uniqueness(&document).is_empty());
    }

    #[test]
    fn openapi_parameter_uniqueness_reports_only_duplicates_in_one_list() {
        let document = json!({
            "openapi": "3.1.0",
            "components": { "parameters": {
                "Q": { "name": "q", "in": "query", "schema": { "type": "string" } }
            }},
            "paths": { "/search": { "get": {
                "operationId": "search",
                "parameters": [
                    { "$ref": "#/components/parameters/Q" },
                    { "name": "q", "in": "query", "schema": { "type": "integer" } }
                ],
                "responses": { "200": { "description": "ok" } }
            }}}
        });
        let violations = validate_openapi_parameter_uniqueness(&document);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].operation, "search");
        assert_eq!(violations[0].oracle, "openapi-parameter-uniqueness");
        assert_eq!(violations[0].pointer, "/paths/~1search/get/parameters/1");

        let distinct_locations = json!({
            "openapi": "3.1.0",
            "paths": { "/search": { "get": {
                "parameters": [
                    { "name": "q", "in": "query" },
                    { "name": "q", "in": "header" }
                ]
            }}}
        });
        assert!(validate_openapi_parameter_uniqueness(&distinct_locations).is_empty());
    }

    #[test]
    fn openapi_parameter_uniqueness_abstains_on_unresolved_and_cyclic_refs() {
        let document = json!({
            "openapi": "3.1.0",
            "components": { "parameters": {
                "A": { "$ref": "#/components/parameters/B" },
                "B": { "$ref": "#/components/parameters/A" }
            }},
            "paths": { "/safe": { "get": {
                "parameters": [
                    { "$ref": "#/components/parameters/A" },
                    { "$ref": "#/components/parameters/Missing" }
                ]
            }}}
        });
        assert!(validate_openapi_parameter_uniqueness(&document).is_empty());
    }

    fn exchange(
        method: &str,
        request_headers: &[(&str, &str)],
        request_body: &[u8],
        status: u16,
        response_headers: &[(&str, &str)],
        response_body: &[u8],
    ) -> HttpExchangeEvidence {
        HttpExchangeEvidence {
            request_method: method.into(),
            request_target: "/fixture".into(),
            request_headers: request_headers
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
            request_body: request_body.into(),
            response_status: status,
            response_headers: response_headers
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
            response_body: response_body.into(),
        }
    }

    #[test]
    fn byte_range_requires_and_checks_exact_raw_representation() {
        let full = b"0123456789";
        let valid = exchange(
            "GET",
            &[("Range", "bytes=2-5")],
            &[],
            206,
            &[("Content-Range", "bytes 2-5/10"), ("Content-Length", "4")],
            b"2345",
        );
        assert_eq!(validate_http_byte_range(&valid, full), None);

        let wrong = exchange(
            "GET",
            &[("range", "bytes=-4")],
            &[],
            206,
            &[("content-range", "bytes 6-9/10")],
            b"5678",
        );
        assert_eq!(
            validate_http_byte_range(&wrong, full).unwrap().oracle,
            "http-byte-range"
        );

        let encoded = exchange(
            "GET",
            &[("range", "bytes=0-1")],
            &[],
            206,
            &[
                ("content-range", "bytes 0-1/10"),
                ("content-encoding", "gzip"),
            ],
            b"xx",
        );
        assert_eq!(validate_http_byte_range(&encoded, full), None);

        let ignored = exchange(
            "GET",
            &[("range", "bytes=0-1,4-5")],
            &[],
            206,
            &[("content-range", "bytes 0-1/10")],
            b"01",
        );
        assert_eq!(validate_http_byte_range(&ignored, full), None);

        let full_response = exchange("GET", &[("range", "bytes=0-1")], &[], 200, &[], full);
        assert_eq!(validate_http_byte_range(&full_response, full), None);
    }

    #[test]
    fn redirect_transition_checks_the_observed_follow_up_hop() {
        let redirect = exchange("POST", &[], b"payload", 303, &[("Location", "/next")], &[]);
        let valid = exchange("GET", &[], &[], 200, &[], &[]);
        assert_eq!(validate_http_redirect_transition(&redirect, &valid), None);
        let wrong = exchange("POST", &[], b"payload", 200, &[], &[]);
        assert_eq!(
            validate_http_redirect_transition(&redirect, &wrong)
                .unwrap()
                .oracle,
            "http-redirect-transition"
        );

        let preserve = exchange("PUT", &[], b"payload", 307, &[("location", "/next")], &[]);
        let dropped = exchange("PUT", &[], &[], 200, &[], &[]);
        assert!(validate_http_redirect_transition(&preserve, &dropped).is_some());

        let historical = exchange("POST", &[], b"payload", 302, &[("location", "/next")], &[]);
        let rewritten = exchange("GET", &[], &[], 200, &[], &[]);
        assert_eq!(
            validate_http_redirect_transition(&historical, &rewritten),
            None
        );
        let preserved = exchange("POST", &[], b"payload", 200, &[], &[]);
        assert_eq!(
            validate_http_redirect_transition(&historical, &preserved),
            None
        );

        let non_redirect = exchange("POST", &[], b"payload", 300, &[], &[]);
        assert_eq!(
            validate_http_redirect_transition(&non_redirect, &rewritten),
            None
        );
    }

    #[test]
    fn websocket_checks_only_explicit_route_auth_and_message_contracts() {
        let contract = WebSocketContract {
            route: "/chat".into(),
            allowed_principals: BTreeSet::from(["member".into()]),
            denied_principals: BTreeSet::from(["blocked".into()]),
            allowed_client_messages: vec![ValueDomain::Object {
                required: BTreeSet::from(["text".into()]),
                properties: BTreeMap::from([(
                    "text".into(),
                    ValueDomain::String {
                        min_length: None,
                        max_length: None,
                        pattern: None,
                        format: None,
                        variants: Vec::new(),
                    },
                )]),
                additional: false,
            }],
            allowed_server_messages: Vec::new(),
            denied_close_codes: BTreeSet::from([1011]),
        };
        let evidence = WebSocketEvidence {
            route: "/chat".into(),
            principal: "blocked".into(),
            accepted: true,
            close_code: Some(1011),
            client_messages: vec![json!({"unexpected": true})],
            server_messages: vec![json!({"not": "checked"})],
        };
        let violations = validate_websocket_contract(&contract, &evidence);
        assert_eq!(violations.len(), 3);
        assert!(violations
            .iter()
            .any(|value| value.oracle == "websocket-authorization"));
        assert!(violations
            .iter()
            .any(|value| value.oracle == "websocket-close"));
        assert!(violations
            .iter()
            .any(|value| value.oracle == "websocket-message"));

        let unknown = WebSocketEvidence {
            route: "/other".into(),
            principal: "unknown".into(),
            accepted: true,
            close_code: None,
            client_messages: vec![Value::Null],
            server_messages: Vec::new(),
        };
        assert!(validate_websocket_contract(&contract, &unknown).is_empty());

        let unlisted_principal = WebSocketEvidence {
            route: "/chat".into(),
            principal: "observer".into(),
            accepted: true,
            close_code: None,
            client_messages: Vec::new(),
            server_messages: vec![Value::Null],
        };
        assert!(validate_websocket_contract(&contract, &unlisted_principal).is_empty());
    }

    #[test]
    fn protocol_proofs_flow_through_evaluation_and_frozen_replay() {
        let operation = proof_operation("download");
        let config = BackendConfig {
            enabled: true,
            operations: vec![operation],
            ..BackendConfig::default()
        };
        let event = BackendEvent {
            sequence: 1,
            trace_id: "trace-protocol".into(),
            span_id: "span-protocol".into(),
            action_index: 4,
            parent_span_id: None,
            operation: "download".into(),
            build: None,
            config_contract: None,
            actor: None,
            tenant: None,
            idempotency_key: None,
            selections: Vec::new(),
            event: BackendEventKind::Protocol {
                proof: ProtocolEvidence::HttpByteRange {
                    exchange: exchange(
                        "GET",
                        &[("range", "bytes=1-3")],
                        &[],
                        206,
                        &[("content-range", "bytes 1-3/5")],
                        b"bad",
                    ),
                    authoritative_full_representation: b"abcde".to_vec(),
                },
            },
        };
        let violations = evaluate(&config, std::slice::from_ref(&event));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "http-byte-range");
        assert_eq!(violations[0].action_index, 4);

        let finding = finding(&violations[0]);
        let guard = FrozenBackendGuard::from_findings(&config, &[finding]).unwrap();
        let log = format!("{EVENT_MARKER}{}", serde_json::to_string(&event).unwrap());
        assert!(guard.reproduces(&log));
    }

    #[test]
    fn authored_lifecycle_protocol_flows_through_backend_evaluation() {
        let config = BackendConfig {
            enabled: true,
            operations: vec![proof_operation("worker")],
            ..BackendConfig::default()
        };
        let event = BackendEvent {
            sequence: 1,
            trace_id: "trace-lifecycle".into(),
            span_id: "span-lifecycle".into(),
            action_index: 7,
            parent_span_id: None,
            operation: "worker".into(),
            build: None,
            config_contract: None,
            actor: None,
            tenant: None,
            idempotency_key: None,
            selections: Vec::new(),
            event: BackendEventKind::Protocol {
                proof: ProtocolEvidence::Lifecycle {
                    contract: ProtocolLifecycleContract {
                        scope_kind: "worker".into(),
                        rules: vec![ProtocolLifecycleRule::ForbidAfter {
                            event: "callback".into(),
                            boundary: "worker.close".into(),
                        }],
                    },
                    evidence: ProtocolLifecycleEvidence {
                        scope_kind: "worker".into(),
                        scope_id: "worker-17".into(),
                        complete: true,
                        events: vec![
                            ProtocolLifecycleEvent {
                                sequence: 0,
                                name: "worker.close".into(),
                                scope_id: "worker-17".into(),
                            },
                            ProtocolLifecycleEvent {
                                sequence: 1,
                                name: "callback".into(),
                                scope_id: "worker-17".into(),
                            },
                        ],
                    },
                },
            },
        };

        let violations = evaluate(&config, &[event]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "lifecycle-forbid-after");
        assert_eq!(violations[0].action_index, 7);
    }
}
