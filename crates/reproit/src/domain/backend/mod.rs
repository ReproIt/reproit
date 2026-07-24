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
                finding
                    .get("oracle")
                    .and_then(Value::as_str)
                    .is_some_and(is_backend_oracle)
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
pub use evaluate::{pending_obligations, PendingObligation};

/// Whether a finding-level oracle id belongs to the backend contract family:
/// a first-class per-check id ("backend-data-loss", ...) or the legacy
/// umbrella id "backend-contract" still present in old artifacts.
pub fn is_backend_oracle(oracle: &str) -> bool {
    oracle.starts_with("backend-")
}

pub fn finding(violation: &BackendViolation) -> Value {
    // Per-check registry id when the check has one (the evaluate/ family);
    // checks without a registry row (scoped protocol evidence) keep the
    // legacy umbrella id, which downstreams accept as unknown-but-well-formed.
    let per_check = format!("backend-{}", violation.oracle);
    let oracle = match crate::domain::oracle::Oracle::parse(&per_check) {
        Some(oracle) => oracle.as_str(),
        None => "backend-contract",
    };
    json!({
        "oracle": oracle,
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
mod tests;
