//! Universal, source-neutral causal capture stream.
//!
//! Capture batches contain observed facts. They never contain executable
//! commands or provider bindings. A later compiler turns these facts into an
//! occurrence and a proposed reproduction plan.

use crate::{
    validate_optional_text, validate_optional_token, validate_text, validate_token, validate_value,
    ArtifactPolicy, CaptureDefectKind, DeploymentIdentity, EnvironmentKind, Event, EventBatch,
    EvidenceArtifact, EvidencePolicy, ObservationAuthority, ObservationKind, ProtocolError,
    ReasonCode, RedactionState, StateKind, TriggerKind, MAX_CONTEXT_BYTES, MAX_TEXT_BYTES,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const CAPTURE_BATCH_VERSION: u16 = 1;
pub const MAX_CAPTURE_EVENTS: usize = 5_000;
pub const MAX_CAPTURE_ARTIFACTS: usize = 256;
pub const MAX_CAPTURE_CAPABILITIES: usize = 128;
pub const MAX_CAUSAL_PARENTS: usize = 32;
pub const MAX_CAPTURE_EVENT_BYTES: usize = 128 * 1024;

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureEmitterKind {
    HostCollector,
    RuntimeSdk,
    BrowserSdk,
    DeviceSdk,
    PlatformAdapter,
    #[serde(alias = "telemetry-adapter")]
    ImportedEvidence,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureEmitter {
    pub id: String,
    pub kind: CaptureEmitterKind,
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

impl CaptureEmitter {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_token(&self.id)?;
        validate_token(&self.component)?;
        validate_optional_token(&self.runtime)?;
        validate_optional_token(&self.parent_id)?;
        if self.parent_id.as_deref() == Some(self.id.as_str()) {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        Ok(())
    }
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureCapabilityKind {
    ProcessTree,
    Commands,
    StandardStreams,
    Filesystem,
    Environment,
    Network,
    Http,
    Rpc,
    Database,
    Cache,
    Queue,
    ObjectStore,
    Jobs,
    Timers,
    UserInterface,
    Device,
    CrashDiagnostics,
    ResourcePressure,
    Clock,
    Randomness,
    Concurrency,
    #[serde(alias = "open-telemetry")]
    ImportedDiagnostics,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureCompleteness {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureCapability {
    pub capability: CaptureCapabilityKind,
    pub completeness: CaptureCompleteness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CaptureCapability {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_optional_text(&self.detail, MAX_TEXT_BYTES)?;
        if self.completeness != CaptureCompleteness::Complete
            && self.detail.as_deref().is_none_or(str::is_empty)
        {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "representation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CapturedValue {
    Structural {
        shape: Value,
    },
    Replayable {
        value: Value,
        redaction: RedactionState,
    },
    Artifact {
        artifact_id: String,
        policy: ArtifactPolicy,
    },
    EnvironmentBound {
        reference: String,
    },
}

impl CapturedValue {
    fn validate(&self, artifact_ids: &BTreeSet<&str>) -> Result<(), ProtocolError> {
        match self {
            Self::Structural { shape } => validate_value(shape, MAX_CONTEXT_BYTES),
            Self::Replayable { value, redaction } => {
                if *redaction == RedactionState::UnredactedRestricted {
                    return Err(ProtocolError::new(ReasonCode::InvalidEvent));
                }
                validate_value(value, MAX_CONTEXT_BYTES)
            }
            Self::Artifact {
                artifact_id,
                policy,
            } => {
                if !artifact_ids.contains(artifact_id.as_str())
                    || *policy == ArtifactPolicy::EnvironmentBound
                {
                    return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
                }
                Ok(())
            }
            Self::EnvironmentBound { reference } => validate_text(reference, MAX_TEXT_BYTES),
        }
    }
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum OperationOutcome {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Unknown,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum StateOperation {
    Read,
    Write,
    Create,
    Delete,
    Rename,
    List,
    Lock,
    Unlock,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyOperation {
    Call,
    Return,
    Publish,
    Consume,
    Connect,
    Disconnect,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessIdentity {
    pub process_id: u64,
    pub executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_process_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_hash: Option<String>,
}

impl ProcessIdentity {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.process_id == 0 || self.parent_process_id == Some(self.process_id) {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        validate_text(&self.executable, MAX_TEXT_BYTES)?;
        if self.executable.is_empty() {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        if let Some(hash) = &self.executable_hash {
            validate_artifact_id(hash)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureRecord {
    pub observation: ObservationKind,
    pub authority: ObservationAuthority,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_point: Option<String>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
}

impl FailureRecord {
    fn validate(&self, artifact_ids: &BTreeSet<&str>) -> Result<(), ProtocolError> {
        validate_text(&self.summary, MAX_TEXT_BYTES)?;
        validate_optional_text(&self.signature, MAX_TEXT_BYTES)?;
        validate_optional_text(&self.observation_point, MAX_TEXT_BYTES)?;
        if self.summary.is_empty() || self.artifact_ids.len() > MAX_CAPTURE_ARTIFACTS {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        let mut seen = BTreeSet::new();
        for artifact_id in &self.artifact_ids {
            if !artifact_ids.contains(artifact_id.as_str()) || !seen.insert(artifact_id) {
                return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CaptureEventKind {
    ProcessStart {
        process: ProcessIdentity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<CapturedValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_directory: Option<CapturedValue>,
    },
    ProcessExit {
        process_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
    },
    OperationStart {
        name: String,
    },
    OperationEnd {
        name: String,
        outcome: OperationOutcome,
    },
    Trigger {
        trigger: TriggerKind,
        subject: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<CapturedValue>,
    },
    Input {
        name: String,
        value: CapturedValue,
    },
    StateAccess {
        state: StateKind,
        operation: StateOperation,
        subject: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<CapturedValue>,
    },
    EnvironmentRead {
        environment: EnvironmentKind,
        subject: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<CapturedValue>,
    },
    Dependency {
        system: String,
        operation: DependencyOperation,
        subject: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<CapturedValue>,
    },
    Effect {
        effect: String,
        subject: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<CapturedValue>,
    },
    Checkpoint {
        name: String,
        #[serde(default)]
        attributes: Value,
    },
    Observation {
        failure: FailureRecord,
    },
    Defect {
        defect: CaptureDefectKind,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_id: Option<String>,
    },
}

impl CaptureEventKind {
    fn validate(&self, artifact_ids: &BTreeSet<&str>) -> Result<(), ProtocolError> {
        match self {
            Self::ProcessStart {
                process,
                arguments,
                working_directory,
            } => {
                process.validate()?;
                validate_optional_value(arguments, artifact_ids)?;
                validate_optional_value(working_directory, artifact_ids)
            }
            Self::ProcessExit {
                process_id,
                exit_code,
                signal,
            } => {
                if *process_id == 0 || (exit_code.is_none() && signal.is_none()) {
                    return Err(ProtocolError::new(ReasonCode::InvalidEvent));
                }
                validate_optional_token(signal)
            }
            Self::OperationStart { name } | Self::OperationEnd { name, .. } => {
                validate_text(name, MAX_TEXT_BYTES)?;
                if name.is_empty() {
                    return Err(ProtocolError::new(ReasonCode::InvalidEvent));
                }
                Ok(())
            }
            Self::Trigger { subject, value, .. }
            | Self::StateAccess { subject, value, .. }
            | Self::EnvironmentRead { subject, value, .. } => {
                validate_text(subject, MAX_TEXT_BYTES)?;
                if subject.is_empty() {
                    return Err(ProtocolError::new(ReasonCode::InvalidEvent));
                }
                validate_optional_value(value, artifact_ids)
            }
            Self::Input { name, value } => {
                validate_token(name)?;
                value.validate(artifact_ids)
            }
            Self::Dependency {
                system,
                subject,
                value,
                ..
            } => {
                validate_token(system)?;
                validate_subject_value(subject, value, artifact_ids)
            }
            Self::Effect {
                effect,
                subject,
                value,
            } => {
                validate_token(effect)?;
                validate_subject_value(subject, value, artifact_ids)
            }
            Self::Checkpoint { name, attributes } => {
                validate_token(name)?;
                validate_value(attributes, MAX_CONTEXT_BYTES)
            }
            Self::Observation { failure } => failure.validate(artifact_ids),
            Self::Defect {
                detail,
                artifact_id,
                ..
            } => {
                validate_text(detail, MAX_TEXT_BYTES)?;
                if detail.is_empty()
                    || artifact_id
                        .as_deref()
                        .is_some_and(|id| !artifact_ids.contains(id))
                {
                    return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureEvent {
    pub id: String,
    pub sequence: u64,
    pub monotonic_ns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default)]
    pub causal_parent_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    pub event: CaptureEventKind,
}

impl CaptureEvent {
    fn validate(&self, artifact_ids: &BTreeSet<&str>) -> Result<(), ProtocolError> {
        validate_token(&self.id)?;
        validate_optional_text(&self.wall_time, crate::MAX_TOKEN_BYTES)?;
        validate_optional_token(&self.actor)?;
        validate_optional_token(&self.trace_id)?;
        validate_optional_token(&self.span_id)?;
        if self.sequence == 0
            || self.process_id == Some(0)
            || self.thread_id == Some(0)
            || self.causal_parent_ids.len() > MAX_CAUSAL_PARENTS
        {
            return Err(ProtocolError::new(ReasonCode::InvalidSequence));
        }
        let mut parents = BTreeSet::new();
        for parent in &self.causal_parent_ids {
            validate_token(parent)?;
            if parent == &self.id || !parents.insert(parent) {
                return Err(ProtocolError::new(ReasonCode::InvalidEvent));
            }
        }
        self.event.validate(artifact_ids)?;
        let encoded =
            serde_json::to_vec(self).map_err(|_| ProtocolError::new(ReasonCode::InvalidEvent))?;
        if encoded.len() > MAX_CAPTURE_EVENT_BYTES {
            return Err(ProtocolError::new(ReasonCode::FrameTooLarge));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureBatch {
    pub version: u16,
    pub batch_id: String,
    pub project_id: String,
    pub session_id: String,
    pub emitter: CaptureEmitter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentIdentity>,
    pub observed_at: String,
    pub policy: EvidencePolicy,
    #[serde(default)]
    pub capabilities: Vec<CaptureCapability>,
    pub events: Vec<CaptureEvent>,
    #[serde(default)]
    pub artifacts: Vec<EvidenceArtifact>,
}

impl CaptureBatch {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != CAPTURE_BATCH_VERSION {
            return Err(ProtocolError::new(ReasonCode::UnsupportedVersion));
        }
        validate_token(&self.batch_id)?;
        validate_token(&self.project_id)?;
        validate_token(&self.session_id)?;
        crate::validate_timestamp(&self.observed_at)?;
        if self.events.is_empty()
            || self.events.len() > MAX_CAPTURE_EVENTS
            || self.artifacts.len() > MAX_CAPTURE_ARTIFACTS
            || self.capabilities.len() > MAX_CAPTURE_CAPABILITIES
        {
            return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
        }
        self.emitter.validate()?;
        if let Some(deployment) = &self.deployment {
            deployment.validate()?;
        }
        validate_token(&self.policy.retention_class)?;
        let policy_value = serde_json::to_value(&self.policy)
            .map_err(|_| ProtocolError::new(ReasonCode::InvalidEvent))?;
        validate_value(&policy_value, MAX_CONTEXT_BYTES)?;

        let mut capability_kinds = BTreeSet::new();
        for capability in &self.capabilities {
            capability.validate()?;
            if !capability_kinds.insert(capability.capability) {
                return Err(ProtocolError::new(ReasonCode::InvalidEvent));
            }
        }

        let mut artifact_ids = BTreeSet::new();
        let mut artifact_bytes = 0u64;
        for artifact in &self.artifacts {
            artifact.validate()?;
            if !artifact_ids.insert(artifact.id.as_str()) {
                return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
            }
            artifact_bytes = artifact_bytes
                .checked_add(artifact.bytes)
                .ok_or_else(|| ProtocolError::new(ReasonCode::BatchTooLarge))?;
        }
        if artifact_bytes > crate::occurrence::MAX_OCCURRENCE_ARTIFACT_BYTES {
            return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
        }

        let batch_event_ids = self
            .events
            .iter()
            .map(|event| event.id.as_str())
            .collect::<BTreeSet<_>>();
        if batch_event_ids.len() != self.events.len() {
            return Err(ProtocolError::new(ReasonCode::InvalidSequence));
        }
        let mut event_ids = BTreeSet::new();
        let mut last_sequence = None;
        for event in &self.events {
            event.validate(&artifact_ids)?;
            if event.causal_parent_ids.iter().any(|parent| {
                batch_event_ids.contains(parent.as_str()) && !event_ids.contains(parent.as_str())
            }) || !event_ids.insert(event.id.as_str())
                || last_sequence.is_some_and(|last| event.sequence <= last)
            {
                return Err(ProtocolError::new(ReasonCode::InvalidSequence));
            }
            last_sequence = Some(event.sequence);
        }
        Ok(())
    }
}

pub fn translate_event_batches(
    legacy: &EventBatch,
    observed_at: String,
    policy: EvidencePolicy,
) -> Result<Vec<CaptureBatch>, ProtocolError> {
    legacy.validate()?;
    let mut run_order = Vec::new();
    let mut frames_by_run = std::collections::BTreeMap::<String, Vec<crate::EventFrame>>::new();
    for frame in &legacy.frames {
        if !frames_by_run.contains_key(&frame.run_id) {
            run_order.push(frame.run_id.clone());
        }
        frames_by_run
            .entry(frame.run_id.clone())
            .or_default()
            .push(frame.clone());
    }
    let mut evidence_by_run =
        std::collections::BTreeMap::<String, Vec<crate::EvidenceGraph>>::new();
    for graph in &legacy.evidence {
        if !frames_by_run.contains_key(&graph.run_id)
            && !evidence_by_run.contains_key(&graph.run_id)
        {
            run_order.push(graph.run_id.clone());
        }
        evidence_by_run
            .entry(graph.run_id.clone())
            .or_default()
            .push(graph.clone());
    }
    let mut captures = Vec::with_capacity(run_order.len());
    for run_id in run_order {
        let run_hash = short_digest(format!("{}:{run_id}", legacy.batch_id).as_bytes());
        let run_batch = EventBatch {
            version: legacy.version,
            batch_id: format!("legacy-{run_hash}"),
            app_id: legacy.app_id.clone(),
            deployment: legacy.deployment.clone(),
            frames: frames_by_run.remove(&run_id).unwrap_or_default(),
            evidence: evidence_by_run.remove(&run_id).unwrap_or_default(),
        };
        captures.push(translate_single_event_batch(
            &run_batch,
            observed_at.clone(),
            policy.clone(),
        )?);
    }
    Ok(captures)
}

pub fn translate_event_batch(
    legacy: &EventBatch,
    observed_at: String,
    policy: EvidencePolicy,
) -> Result<CaptureBatch, ProtocolError> {
    let mut captures = translate_event_batches(legacy, observed_at, policy)?;
    if captures.len() != 1 {
        return Err(ProtocolError::new(ReasonCode::InvalidEvent));
    }
    Ok(captures.remove(0))
}

fn translate_single_event_batch(
    legacy: &EventBatch,
    observed_at: String,
    policy: EvidencePolicy,
) -> Result<CaptureBatch, ProtocolError> {
    legacy.validate()?;
    validate_text(&observed_at, crate::MAX_TOKEN_BYTES)?;
    if observed_at.is_empty() {
        return Err(ProtocolError::new(ReasonCode::InvalidEvent));
    }
    let emitter_suffix = short_digest(legacy.batch_id.as_bytes());
    let mut events: Vec<CaptureEvent> =
        Vec::with_capacity(legacy.frames.len().min(MAX_CAPTURE_EVENTS));
    for frame in &legacy.frames {
        if events.len() == MAX_CAPTURE_EVENTS {
            break;
        }
        let sequence = events.len() as u64 + 1;
        let event = translate_legacy_event(&frame.event);
        events.push(CaptureEvent {
            id: format!("evt_legacy_{emitter_suffix}_{sequence}"),
            sequence,
            monotonic_ns: sequence,
            wall_time: None,
            process_id: None,
            thread_id: None,
            actor: legacy_actor(&frame.event),
            causal_parent_ids: events
                .last()
                .map(|parent| vec![parent.id.clone()])
                .unwrap_or_default(),
            trace_id: None,
            span_id: None,
            event,
        });
    }
    if legacy.frames.len() > events.len() {
        if events.len() == MAX_CAPTURE_EVENTS {
            events.pop();
        }
        let sequence = events.len() as u64 + 1;
        events.push(CaptureEvent {
            id: format!("evt_legacy_{emitter_suffix}_{sequence}"),
            sequence,
            monotonic_ns: sequence,
            wall_time: None,
            process_id: None,
            thread_id: None,
            actor: None,
            causal_parent_ids: vec![],
            trace_id: None,
            span_id: None,
            event: CaptureEventKind::Defect {
                defect: CaptureDefectKind::Truncated,
                detail: format!(
                    "{} legacy frames exceeded the universal capture event bound",
                    legacy.frames.len() - events.len()
                ),
                artifact_id: None,
            },
        });
    }
    if events.is_empty() {
        events.push(CaptureEvent {
            id: format!("evt_legacy_{emitter_suffix}_1"),
            sequence: 1,
            monotonic_ns: 1,
            wall_time: None,
            process_id: None,
            thread_id: None,
            actor: None,
            causal_parent_ids: vec![],
            trace_id: None,
            span_id: None,
            event: CaptureEventKind::Checkpoint {
                name: "legacy-evidence".into(),
                attributes: serde_json::json!({
                    "graphs": legacy.evidence.len(),
                    "sourceBatchRetained": true,
                }),
            },
        });
    }
    let mut capabilities = Vec::new();
    if legacy
        .frames
        .iter()
        .any(|frame| matches!(frame.event, Event::Action { .. } | Event::GraphEdge { .. }))
    {
        capabilities.push(CaptureCapability {
            capability: CaptureCapabilityKind::UserInterface,
            completeness: CaptureCompleteness::Complete,
            detail: None,
        });
    }
    if legacy
        .frames
        .iter()
        .any(|frame| matches!(frame.event, Event::Backend { .. }))
    {
        capabilities.push(CaptureCapability {
            capability: CaptureCapabilityKind::Http,
            completeness: CaptureCompleteness::Partial,
            detail: Some("translated from the legacy backend evidence contract".into()),
        });
    }
    let batch = CaptureBatch {
        version: CAPTURE_BATCH_VERSION,
        batch_id: format!("cb_legacy_{emitter_suffix}"),
        project_id: legacy.app_id.clone(),
        session_id: legacy
            .frames
            .first()
            .map(|frame| frame.run_id.clone())
            .unwrap_or_else(|| format!("session_{emitter_suffix}")),
        emitter: CaptureEmitter {
            id: format!("legacy-{emitter_suffix}"),
            kind: CaptureEmitterKind::ImportedEvidence,
            component: "legacy-sdk".into(),
            runtime: None,
            parent_id: None,
        },
        deployment: legacy.deployment.clone(),
        observed_at,
        policy,
        capabilities,
        events,
        artifacts: vec![],
    };
    batch.validate()?;
    Ok(batch)
}

fn translate_legacy_event(event: &Event) -> CaptureEventKind {
    match event {
        Event::Action { action, .. } => CaptureEventKind::Trigger {
            trigger: TriggerKind::UiAction,
            subject: action.clone(),
            value: None,
        },
        Event::Observation {
            state,
            route,
            visible_text,
            counts,
            oracle_signals,
            network_statuses,
            response_shapes,
            ..
        } => CaptureEventKind::Checkpoint {
            name: "legacy-observation".into(),
            attributes: bounded_structural(serde_json::json!({
                "state": state,
                "route": route,
                "visibleText": visible_text,
                "counts": counts,
                "oracleSignals": oracle_signals,
                "networkStatuses": network_statuses,
                "responseShapes": response_shapes,
            })),
        },
        Event::Backend { evidence } => CaptureEventKind::Effect {
            effect: "backend-event".into(),
            subject: "legacy-backend".into(),
            value: Some(CapturedValue::Structural {
                shape: bounded_structural(evidence.clone()),
            }),
        },
        Event::GraphEdge { from, action, to } => CaptureEventKind::Trigger {
            trigger: TriggerKind::UiAction,
            subject: action.clone(),
            value: Some(CapturedValue::Structural {
                shape: serde_json::json!({"from": from, "to": to}),
            }),
        },
        Event::Finding {
            signature,
            message,
            identity,
            ..
        } => CaptureEventKind::Observation {
            failure: FailureRecord {
                observation: legacy_observation_kind(&identity.kind),
                authority: ObservationAuthority::RuntimeDiagnosis,
                summary: if message.is_empty() {
                    "legacy SDK finding".into()
                } else {
                    message.clone()
                },
                signature: Some(signature.clone()),
                observation_point: (!identity.frame.is_empty()).then(|| identity.frame.clone()),
                artifact_ids: vec![],
            },
        },
        Event::StreamDefect { reason } => CaptureEventKind::Defect {
            defect: legacy_defect(*reason),
            detail: format!("legacy SDK reported {}", reason.as_str()),
            artifact_id: None,
        },
    }
}

fn legacy_actor(event: &Event) -> Option<String> {
    match event {
        Event::Action { actor, .. } | Event::Observation { actor, .. } => actor.clone(),
        Event::Backend { .. }
        | Event::GraphEdge { .. }
        | Event::Finding { .. }
        | Event::StreamDefect { .. } => None,
    }
}

fn legacy_observation_kind(kind: &str) -> ObservationKind {
    match kind {
        "crash" => ObservationKind::Crash,
        "exit" => ObservationKind::Exit,
        "hang" => ObservationKind::Hang,
        "data-corruption" => ObservationKind::DataCorruption,
        "performance" => ObservationKind::Performance,
        "contract" | "contract-violation" => ObservationKind::ContractViolation,
        _ => ObservationKind::Exception,
    }
}

fn legacy_defect(reason: ReasonCode) -> CaptureDefectKind {
    match reason {
        ReasonCode::FrameTooLarge | ReasonCode::BatchTooLarge => CaptureDefectKind::Truncated,
        ReasonCode::UnsupportedVersion => CaptureDefectKind::Unsupported,
        ReasonCode::AuthorityUnavailable => CaptureDefectKind::Unavailable,
        ReasonCode::IncompleteStream | ReasonCode::InvalidSequence => {
            CaptureDefectKind::SequenceGap
        }
        ReasonCode::MalformedFrame
        | ReasonCode::InvalidScope
        | ReasonCode::InvalidEvent
        | ReasonCode::NoObservations
        | ReasonCode::InvalidArtifact => CaptureDefectKind::Rejected,
    }
}

fn bounded_structural(value: Value) -> Value {
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return serde_json::json!({"omitted": true, "reason": "serialization-failed"});
    };
    if bytes.len() <= MAX_CONTEXT_BYTES {
        return value;
    }
    serde_json::json!({
        "omitted": true,
        "bytes": bytes.len(),
        "sha256": format!("sha256:{}", hex_digest(Sha256::digest(bytes))),
    })
}

fn short_digest(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))[..16].to_string()
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}

fn validate_optional_value(
    value: &Option<CapturedValue>,
    artifact_ids: &BTreeSet<&str>,
) -> Result<(), ProtocolError> {
    value
        .as_ref()
        .map(|value| value.validate(artifact_ids))
        .transpose()
        .map(drop)
}

fn validate_subject_value(
    subject: &str,
    value: &Option<CapturedValue>,
    artifact_ids: &BTreeSet<&str>,
) -> Result<(), ProtocolError> {
    validate_text(subject, MAX_TEXT_BYTES)?;
    if subject.is_empty() {
        return Err(ProtocolError::new(ReasonCode::InvalidEvent));
    }
    validate_optional_value(value, artifact_ids)
}

fn validate_artifact_id(value: &str) -> Result<(), ProtocolError> {
    if !value.starts_with("sha256:") || !crate::valid_hash(&value[7..], 64) {
        return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
    }
    Ok(())
}
