//! Bounded OpenTelemetry semantic-convention bridge.
//!
//! Spans establish topology and causality. They do not claim replay payloads
//! or durable effects. An observation exists only when instrumentation emits
//! an explicit `reproit.failure.signature` annotation.

use crate::{
    CaptureBatch, CaptureCapability, CaptureCapabilityKind, CaptureCompleteness, CaptureEmitter,
    CaptureEmitterKind, CaptureEvent, CaptureEventKind, CapturedValue, DependencyOperation,
    DeploymentIdentity, EvidencePolicy, FailureRecord, ObservationAuthority, ObservationKind,
    ProtocolError, ReasonCode, TriggerKind, CAPTURE_BATCH_VERSION, MAX_CAPTURE_EVENTS,
    MAX_CONTEXT_BYTES, MAX_TEXT_BYTES,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_SPAN_ATTRIBUTES: usize = 128;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenTelemetryBatch {
    pub batch_id: String,
    pub project_id: String,
    pub session_id: String,
    pub emitter_id: String,
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentIdentity>,
    pub observed_at: String,
    pub policy: EvidencePolicy,
    pub spans: Vec<OpenTelemetrySpan>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenTelemetrySpan {
    pub trace_id: String,
    pub span_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: OpenTelemetrySpanKind,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    #[serde(default)]
    pub attributes: BTreeMap<String, Value>,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum OpenTelemetrySpanKind {
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

pub fn bridge_open_telemetry(input: OpenTelemetryBatch) -> Result<CaptureBatch, ProtocolError> {
    validate_input_bounds(&input)?;
    let order = causal_order(&input.spans)?;
    let first_start = input
        .spans
        .iter()
        .map(|span| span.start_time_unix_nano)
        .min()
        .unwrap_or(0);
    let mut primary_events = BTreeMap::new();
    let mut events = Vec::new();
    for index in order {
        let span = &input.spans[index];
        let parent = span.parent_span_id.as_ref().and_then(|parent| {
            primary_events
                .get(&(span.trace_id.as_str(), parent.as_str()))
                .cloned()
        });
        let primary_id = format!("otel_{}", span.span_id);
        let monotonic_ns = span
            .start_time_unix_nano
            .saturating_sub(first_start)
            .saturating_add(1);
        events.push(capture_event(
            primary_id.clone(),
            events.len() + 1,
            monotonic_ns,
            span,
            parent.into_iter().collect(),
            semantic_event(span),
        ));
        primary_events.insert(
            (span.trace_id.as_str(), span.span_id.as_str()),
            primary_id.clone(),
        );
        if let Some(failure) = explicit_failure(span) {
            events.push(capture_event(
                format!("{primary_id}_failure"),
                events.len() + 1,
                monotonic_ns.saturating_add(1),
                span,
                vec![primary_id],
                CaptureEventKind::Observation { failure },
            ));
        }
    }
    if events.len() > MAX_CAPTURE_EVENTS {
        return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
    }
    let batch = CaptureBatch {
        version: CAPTURE_BATCH_VERSION,
        batch_id: input.batch_id,
        project_id: input.project_id,
        session_id: input.session_id,
        emitter: CaptureEmitter {
            id: input.emitter_id,
            kind: CaptureEmitterKind::TelemetryAdapter,
            component: input.component,
            runtime: input.runtime,
            parent_id: None,
        },
        deployment: input.deployment,
        observed_at: input.observed_at,
        policy: input.policy,
        capabilities: capabilities(&input.spans),
        events,
        artifacts: Vec::new(),
    };
    batch.validate()?;
    Ok(batch)
}

fn validate_input_bounds(input: &OpenTelemetryBatch) -> Result<(), ProtocolError> {
    if input.spans.is_empty() || input.spans.len() > MAX_CAPTURE_EVENTS {
        return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
    }
    for span in &input.spans {
        if span.attributes.len() > MAX_SPAN_ATTRIBUTES
            || span.start_time_unix_nano > span.end_time_unix_nano
            || span.name.is_empty()
            || span.name.len() > MAX_TEXT_BYTES
            || !valid_otel_id(&span.trace_id, 32)
            || !valid_otel_id(&span.span_id, 16)
            || span
                .parent_span_id
                .as_deref()
                .is_some_and(|parent| !valid_otel_id(parent, 16))
        {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        let encoded = serde_json::to_vec(&span.attributes)
            .map_err(|_| ProtocolError::new(ReasonCode::InvalidEvent))?;
        if encoded.len() > MAX_CONTEXT_BYTES {
            return Err(ProtocolError::new(ReasonCode::FrameTooLarge));
        }
    }
    Ok(())
}

fn valid_otel_id(value: &str, expected_bytes: usize) -> bool {
    value.len() == expected_bytes
        && value.bytes().any(|byte| byte != b'0')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn causal_order(spans: &[OpenTelemetrySpan]) -> Result<Vec<usize>, ProtocolError> {
    let mut index = BTreeMap::new();
    for (position, span) in spans.iter().enumerate() {
        if index
            .insert((span.trace_id.as_str(), span.span_id.as_str()), position)
            .is_some()
        {
            return Err(ProtocolError::new(ReasonCode::InvalidSequence));
        }
    }
    let mut indegree = vec![0usize; spans.len()];
    let mut children = vec![Vec::new(); spans.len()];
    for (position, span) in spans.iter().enumerate() {
        let Some(parent) = span.parent_span_id.as_deref() else {
            continue;
        };
        if parent == span.span_id {
            return Err(ProtocolError::new(ReasonCode::InvalidSequence));
        }
        if let Some(parent_position) = index.get(&(span.trace_id.as_str(), parent)) {
            indegree[position] = 1;
            children[*parent_position].push(position);
        }
    }
    let mut ready = BTreeSet::new();
    for (position, span) in spans.iter().enumerate() {
        if indegree[position] == 0 {
            ready.insert(order_key(span, position));
        }
    }
    let mut order = Vec::with_capacity(spans.len());
    while let Some(key) = ready.pop_first() {
        let position = key.3;
        order.push(position);
        for child in &children[position] {
            indegree[*child] -= 1;
            if indegree[*child] == 0 {
                ready.insert(order_key(&spans[*child], *child));
            }
        }
    }
    if order.len() != spans.len() {
        return Err(ProtocolError::new(ReasonCode::InvalidSequence));
    }
    Ok(order)
}

fn order_key(span: &OpenTelemetrySpan, position: usize) -> (u64, String, String, usize) {
    (
        span.start_time_unix_nano,
        span.trace_id.clone(),
        span.span_id.clone(),
        position,
    )
}

fn capture_event(
    id: String,
    sequence: usize,
    monotonic_ns: u64,
    span: &OpenTelemetrySpan,
    causal_parent_ids: Vec<String>,
    event: CaptureEventKind,
) -> CaptureEvent {
    CaptureEvent {
        id,
        sequence: sequence as u64,
        monotonic_ns,
        wall_time: None,
        process_id: None,
        thread_id: None,
        actor: None,
        causal_parent_ids,
        trace_id: Some(span.trace_id.clone()),
        span_id: Some(span.span_id.clone()),
        event,
    }
}

fn semantic_event(span: &OpenTelemetrySpan) -> CaptureEventKind {
    if is_server(span) {
        return server_trigger(span);
    }
    if is_dependency(span) {
        return dependency(span);
    }
    CaptureEventKind::Checkpoint {
        name: "otel-span".into(),
        attributes: structural_shape(span, "internal"),
    }
}

fn is_server(span: &OpenTelemetrySpan) -> bool {
    matches!(
        span.kind,
        OpenTelemetrySpanKind::Server | OpenTelemetrySpanKind::Consumer
    )
}

fn is_dependency(span: &OpenTelemetrySpan) -> bool {
    matches!(
        span.kind,
        OpenTelemetrySpanKind::Client | OpenTelemetrySpanKind::Producer
    )
}

fn server_trigger(span: &OpenTelemetrySpan) -> CaptureEventKind {
    let (trigger, convention) = if has(span, "messaging.system") {
        (TriggerKind::Message, "messaging")
    } else if has(span, "rpc.system") {
        (TriggerKind::RpcRequest, "rpc")
    } else {
        (TriggerKind::HttpRequest, "http")
    };
    CaptureEventKind::Trigger {
        trigger,
        subject: semantic_subject(span, convention),
        value: Some(CapturedValue::Structural {
            shape: structural_shape(span, convention),
        }),
    }
}

fn dependency(span: &OpenTelemetrySpan) -> CaptureEventKind {
    let (system, operation) = if has(span, "db.system") || has(span, "db.system.name") {
        ("database", DependencyOperation::Call)
    } else if has(span, "messaging.system") {
        ("messaging", DependencyOperation::Publish)
    } else if has(span, "rpc.system") {
        ("rpc", DependencyOperation::Call)
    } else if has(span, "http.request.method") || has(span, "http.method") {
        ("http", DependencyOperation::Call)
    } else {
        ("service", DependencyOperation::Call)
    };
    CaptureEventKind::Dependency {
        system: system.into(),
        operation,
        subject: semantic_subject(span, system),
        value: Some(CapturedValue::Structural {
            shape: structural_shape(span, system),
        }),
    }
}

fn semantic_subject(span: &OpenTelemetrySpan, convention: &str) -> String {
    const KEYS: [&str; 9] = [
        "http.route",
        "url.template",
        "rpc.service",
        "rpc.method",
        "db.system.name",
        "db.operation.name",
        "messaging.destination.name",
        "peer.service",
        "server.address",
    ];
    let details = KEYS
        .iter()
        .filter_map(|key| string_attribute(span, key))
        .take(3)
        .collect::<Vec<_>>();
    if details.is_empty() {
        format!("{convention}:{}", span.name)
    } else {
        format!("{convention}:{}", details.join(":"))
    }
}

fn structural_shape(span: &OpenTelemetrySpan, convention: &str) -> Value {
    json!({
        "semanticConvention": convention,
        "spanKind": span.kind,
        "attributeKeys": span.attributes.keys().collect::<Vec<_>>(),
        "rawAttributeValuesRetained": false,
        "semanticIdentityRetained": true,
        "durableEffectComplete": false,
    })
}

fn explicit_failure(span: &OpenTelemetrySpan) -> Option<FailureRecord> {
    let signature = string_attribute(span, "reproit.failure.signature")?;
    let summary = string_attribute(span, "reproit.failure.summary")
        .unwrap_or_else(|| format!("failure diagnosed at span {}", span.name));
    Some(FailureRecord {
        observation: observation_kind(
            string_attribute(span, "reproit.failure.observation").as_deref(),
        ),
        authority: ObservationAuthority::RuntimeDiagnosis,
        summary,
        signature: Some(signature),
        observation_point: Some(format!("otel:{}/{}", span.trace_id, span.span_id)),
        artifact_ids: Vec::new(),
    })
}

fn observation_kind(value: Option<&str>) -> ObservationKind {
    match value {
        Some("crash") => ObservationKind::Crash,
        Some("exit") => ObservationKind::Exit,
        Some("hang") => ObservationKind::Hang,
        Some("contract-violation") => ObservationKind::ContractViolation,
        Some("data-corruption") => ObservationKind::DataCorruption,
        Some("performance") => ObservationKind::Performance,
        Some("diagnostic") => ObservationKind::Diagnostic,
        _ => ObservationKind::Exception,
    }
}

fn capabilities(spans: &[OpenTelemetrySpan]) -> Vec<CaptureCapability> {
    let mut kinds = BTreeSet::new();
    for span in spans {
        if has(span, "db.system") || has(span, "db.system.name") {
            kinds.insert(CaptureCapabilityKind::Database);
        }
        if has(span, "messaging.system") {
            kinds.insert(CaptureCapabilityKind::Queue);
        }
        if has(span, "rpc.system") {
            kinds.insert(CaptureCapabilityKind::Rpc);
        }
        if has(span, "http.request.method") || has(span, "http.method") {
            kinds.insert(CaptureCapabilityKind::Http);
        }
    }
    let mut capabilities = vec![CaptureCapability {
        capability: CaptureCapabilityKind::OpenTelemetry,
        completeness: CaptureCompleteness::Complete,
        detail: None,
    }];
    capabilities.extend(kinds.into_iter().map(|capability| CaptureCapability {
        capability,
        completeness: CaptureCompleteness::Partial,
        detail: Some(
            "semantic spans prove topology, not replay payloads or durable completion".into(),
        ),
    }));
    capabilities
}

fn has(span: &OpenTelemetrySpan, key: &str) -> bool {
    span.attributes.contains_key(key)
}

fn string_attribute(span: &OpenTelemetrySpan, key: &str) -> Option<String> {
    span.attributes.get(key)?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compile_capture_failure, AssessmentStatus, CaptureAssessmentScope, ConsentClass,
        DependencyKind, RequirementKind,
    };

    fn span(id: &str, parent: Option<&str>, kind: OpenTelemetrySpanKind) -> OpenTelemetrySpan {
        OpenTelemetrySpan {
            trace_id: "0123456789abcdef0123456789abcdef".into(),
            span_id: id.into(),
            parent_span_id: parent.map(str::to_string),
            name: "request".into(),
            kind,
            start_time_unix_nano: if parent.is_some() { 2 } else { 1 },
            end_time_unix_nano: 3,
            attributes: BTreeMap::new(),
        }
    }

    fn input(spans: Vec<OpenTelemetrySpan>) -> OpenTelemetryBatch {
        OpenTelemetryBatch {
            batch_id: "otel_batch".into(),
            project_id: "shop".into(),
            session_id: "session-1".into(),
            emitter_id: "otel-bridge".into(),
            component: "checkout".into(),
            runtime: Some("rust".into()),
            deployment: None,
            observed_at: "2026-08-03T12:00:00Z".into(),
            policy: EvidencePolicy {
                consent: ConsentClass::ApplicationTelemetry,
                retention_class: "telemetry".into(),
            },
            spans,
        }
    }

    #[test]
    fn topology_compiles_into_incomplete_distributed_requirements() {
        let mut server = span("aaaaaaaaaaaaaaaa", None, OpenTelemetrySpanKind::Server);
        server
            .attributes
            .insert("http.request.method".into(), json!("POST"));
        server
            .attributes
            .insert("http.route".into(), json!("/orders"));
        server.attributes.insert(
            "reproit.failure.signature".into(),
            json!("panic:null-owner"),
        );
        let mut database = span(
            "bbbbbbbbbbbbbbbb",
            Some("aaaaaaaaaaaaaaaa"),
            OpenTelemetrySpanKind::Client,
        );
        database
            .attributes
            .insert("db.system.name".into(), json!("postgresql"));
        let batch = bridge_open_telemetry(input(vec![database, server])).unwrap();
        assert_eq!(
            batch.events[1].causal_parent_ids,
            vec!["otel_aaaaaaaaaaaaaaaa"]
        );
        assert!(!batch
            .events
            .iter()
            .any(|event| matches!(event.event, CaptureEventKind::Effect { .. })));

        let compiled = compile_capture_failure(
            &batch,
            "2026-08-03T12:01:00Z",
            CaptureAssessmentScope::Portable,
        )
        .unwrap()
        .unwrap();
        assert_eq!(compiled.assessment.status, AssessmentStatus::Incomplete);
        assert!(compiled
            .assessment
            .requirements
            .iter()
            .any(|requirement| matches!(
                requirement.requirement,
                RequirementKind::Dependency {
                    dependency: DependencyKind::DistributedSystem,
                    ..
                }
            )));
        assert!(compiled
            .assessment
            .unresolved
            .iter()
            .any(|item| item.detail.contains("replayable dependency result")));
    }

    #[test]
    fn cyclic_span_parents_are_rejected() {
        let mut left = span(
            "aaaaaaaaaaaaaaaa",
            Some("bbbbbbbbbbbbbbbb"),
            OpenTelemetrySpanKind::Internal,
        );
        left.start_time_unix_nano = 1;
        let right = span(
            "bbbbbbbbbbbbbbbb",
            Some("aaaaaaaaaaaaaaaa"),
            OpenTelemetrySpanKind::Internal,
        );
        assert_eq!(
            bridge_open_telemetry(input(vec![left, right]))
                .unwrap_err()
                .reason,
            ReasonCode::InvalidSequence
        );
    }

    #[test]
    fn invalid_external_span_identity_is_rejected_before_event_allocation() {
        let invalid = span("not-a-span-id", None, OpenTelemetrySpanKind::Internal);
        assert_eq!(
            bridge_open_telemetry(input(vec![invalid]))
                .unwrap_err()
                .reason,
            ReasonCode::InvalidEvent
        );
    }
}
