//! Pure bounded flight recorder for the universal capture protocol.
//!
//! Platform adapters observe facts and pass them here. This module owns event
//! ordering, buffer bounds, overflow defects, and artifact identity checks. It
//! performs no filesystem, process, clock, or network operations.

use reproit_protocol::{
    CaptureBatch, CaptureDefectKind, CaptureEmitter, CaptureEvent, CaptureEventKind, CapturedValue,
    DependencyOperation, DeploymentIdentity, EvidenceArtifact, EvidencePolicy, FailureRecord,
    OperationOutcome, StateKind, StateOperation, TriggerKind, CAPTURE_BATCH_VERSION,
    MAX_CAPTURE_ARTIFACTS, MAX_CAPTURE_EVENTS,
};
use std::collections::{BTreeSet, VecDeque};

#[derive(Clone, Debug)]
pub struct RecorderConfig {
    pub batch_id: String,
    pub project_id: String,
    pub session_id: String,
    pub emitter: CaptureEmitter,
    pub deployment: Option<DeploymentIdentity>,
    pub observed_at: String,
    pub policy: EvidencePolicy,
    pub capabilities: Vec<reproit_protocol::CaptureCapability>,
    pub max_events: usize,
    pub max_artifacts: usize,
}

#[derive(Clone, Debug, Default)]
pub struct EventContext {
    pub monotonic_ns: u64,
    pub wall_time: Option<String>,
    pub process_id: Option<u64>,
    pub thread_id: Option<u64>,
    pub actor: Option<String>,
    pub causal_parent_ids: Vec<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

#[derive(Debug)]
pub enum RecorderError {
    InvalidBounds(String),
    InvalidArtifact(String),
    DuplicateArtifact(String),
    InvalidCapture(String),
}

impl std::fmt::Display for RecorderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBounds(detail)
            | Self::InvalidArtifact(detail)
            | Self::DuplicateArtifact(detail)
            | Self::InvalidCapture(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for RecorderError {}

pub struct Recorder {
    config: RecorderConfig,
    events: VecDeque<CaptureEvent>,
    artifacts: Vec<EvidenceArtifact>,
    artifact_ids: BTreeSet<String>,
    dropped_event_ids: BTreeSet<String>,
    dropped_events: u64,
    dropped_artifacts: u64,
    next_sequence: u64,
    last_monotonic_ns: u64,
}

impl Recorder {
    pub fn new(config: RecorderConfig) -> Result<Self, RecorderError> {
        if config.max_events < 2 || config.max_events > MAX_CAPTURE_EVENTS {
            return Err(RecorderError::InvalidBounds(format!(
                "recorder max events must be between 2 and {MAX_CAPTURE_EVENTS}, got {}",
                config.max_events
            )));
        }
        if config.max_artifacts == 0 || config.max_artifacts > MAX_CAPTURE_ARTIFACTS {
            return Err(RecorderError::InvalidBounds(format!(
                "recorder max artifacts must be between 1 and {MAX_CAPTURE_ARTIFACTS}, got {}",
                config.max_artifacts
            )));
        }
        Ok(Self {
            config,
            events: VecDeque::new(),
            artifacts: Vec::new(),
            artifact_ids: BTreeSet::new(),
            dropped_event_ids: BTreeSet::new(),
            dropped_events: 0,
            dropped_artifacts: 0,
            next_sequence: 1,
            last_monotonic_ns: 0,
        })
    }

    pub fn record(&mut self, context: EventContext, event: CaptureEventKind) -> String {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.last_monotonic_ns = self.last_monotonic_ns.max(context.monotonic_ns);
        let id = format!("evt_{}_{}", self.config.emitter.id, sequence);
        let capture_event = CaptureEvent {
            id: id.clone(),
            sequence,
            monotonic_ns: context.monotonic_ns,
            wall_time: context.wall_time,
            process_id: context.process_id,
            thread_id: context.thread_id,
            actor: context.actor,
            causal_parent_ids: context.causal_parent_ids,
            trace_id: context.trace_id,
            span_id: context.span_id,
            event,
        };
        if self.events.len() == self.config.max_events {
            self.drop_oldest_event();
        }
        self.events.push_back(capture_event);
        id
    }

    pub fn start_operation(&mut self, context: EventContext, name: impl Into<String>) -> String {
        self.record(
            context,
            CaptureEventKind::OperationStart { name: name.into() },
        )
    }

    pub fn end_operation(
        &mut self,
        context: EventContext,
        name: impl Into<String>,
        outcome: OperationOutcome,
    ) -> String {
        self.record(
            context,
            CaptureEventKind::OperationEnd {
                name: name.into(),
                outcome,
            },
        )
    }

    pub fn trigger(
        &mut self,
        context: EventContext,
        trigger: TriggerKind,
        subject: impl Into<String>,
        value: Option<CapturedValue>,
    ) -> String {
        self.record(
            context,
            CaptureEventKind::Trigger {
                trigger,
                subject: subject.into(),
                value,
            },
        )
    }

    pub fn input(
        &mut self,
        context: EventContext,
        name: impl Into<String>,
        value: CapturedValue,
    ) -> String {
        self.record(
            context,
            CaptureEventKind::Input {
                name: name.into(),
                value,
            },
        )
    }

    pub fn state(
        &mut self,
        context: EventContext,
        state: StateKind,
        operation: StateOperation,
        subject: impl Into<String>,
        value: Option<CapturedValue>,
    ) -> String {
        self.record(
            context,
            CaptureEventKind::StateAccess {
                state,
                operation,
                subject: subject.into(),
                value,
            },
        )
    }

    pub fn dependency(
        &mut self,
        context: EventContext,
        system: impl Into<String>,
        operation: DependencyOperation,
        subject: impl Into<String>,
        value: Option<CapturedValue>,
    ) -> String {
        self.record(
            context,
            CaptureEventKind::Dependency {
                system: system.into(),
                operation,
                subject: subject.into(),
                value,
            },
        )
    }

    pub fn effect(
        &mut self,
        context: EventContext,
        effect: impl Into<String>,
        subject: impl Into<String>,
        value: Option<CapturedValue>,
    ) -> String {
        self.record(
            context,
            CaptureEventKind::Effect {
                effect: effect.into(),
                subject: subject.into(),
                value,
            },
        )
    }

    pub fn checkpoint(
        &mut self,
        context: EventContext,
        name: impl Into<String>,
        attributes: serde_json::Value,
    ) -> String {
        self.record(
            context,
            CaptureEventKind::Checkpoint {
                name: name.into(),
                attributes,
            },
        )
    }

    pub fn failure(&mut self, context: EventContext, failure: FailureRecord) -> String {
        self.record(context, CaptureEventKind::Observation { failure })
    }

    pub fn add_artifact(&mut self, artifact: EvidenceArtifact) -> Result<bool, RecorderError> {
        artifact.validate().map_err(|error| {
            RecorderError::InvalidArtifact(format!("invalid capture artifact: {error}"))
        })?;
        if self.artifact_ids.contains(&artifact.id) {
            return Err(RecorderError::DuplicateArtifact(format!(
                "capture artifact {} was added more than once",
                artifact.id
            )));
        }
        if self.artifacts.len() == self.config.max_artifacts {
            self.dropped_artifacts = self.dropped_artifacts.saturating_add(1);
            return Ok(false);
        }
        self.artifact_ids.insert(artifact.id.clone());
        self.artifacts.push(artifact);
        Ok(true)
    }

    pub fn finish(mut self) -> Result<CaptureBatch, RecorderError> {
        self.remove_dropped_parents();
        if self.dropped_events > 0 || self.dropped_artifacts > 0 {
            if self.events.len() == self.config.max_events {
                self.drop_oldest_event();
                self.remove_dropped_parents();
            }
            let detail = format!(
                "{} event(s) and {} artifact(s) exceeded recorder bounds",
                self.dropped_events, self.dropped_artifacts
            );
            let sequence = self.next_sequence;
            self.events.push_back(CaptureEvent {
                id: format!("evt_{}_{}", self.config.emitter.id, sequence),
                sequence,
                monotonic_ns: self.last_monotonic_ns.saturating_add(1),
                wall_time: None,
                process_id: None,
                thread_id: None,
                actor: None,
                causal_parent_ids: vec![],
                trace_id: None,
                span_id: None,
                event: CaptureEventKind::Defect {
                    defect: CaptureDefectKind::Dropped,
                    detail,
                    artifact_id: None,
                },
            });
        }
        let batch = CaptureBatch {
            version: CAPTURE_BATCH_VERSION,
            batch_id: self.config.batch_id,
            project_id: self.config.project_id,
            session_id: self.config.session_id,
            emitter: self.config.emitter,
            deployment: self.config.deployment,
            observed_at: self.config.observed_at,
            policy: self.config.policy,
            capabilities: self.config.capabilities,
            events: self.events.into_iter().collect(),
            artifacts: self.artifacts,
        };
        batch.validate().map_err(|error| {
            RecorderError::InvalidCapture(format!("recorder produced an invalid capture: {error}"))
        })?;
        Ok(batch)
    }

    fn drop_oldest_event(&mut self) {
        if let Some(dropped) = self.events.pop_front() {
            self.dropped_event_ids.insert(dropped.id);
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
    }

    fn remove_dropped_parents(&mut self) {
        for event in &mut self.events {
            event
                .causal_parent_ids
                .retain(|parent| !self.dropped_event_ids.contains(parent));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reproit_protocol::{
        CaptureCapability, CaptureCapabilityKind, CaptureCompleteness, CaptureEmitterKind,
        ConsentClass, OperationOutcome,
    };

    fn config(max_events: usize) -> RecorderConfig {
        RecorderConfig {
            batch_id: "cb_test".into(),
            project_id: "project-test".into(),
            session_id: "session-test".into(),
            emitter: CaptureEmitter {
                id: "emitter-test".into(),
                kind: CaptureEmitterKind::RuntimeSdk,
                component: "worker".into(),
                runtime: Some("rust".into()),
                parent_id: None,
            },
            deployment: None,
            observed_at: "2026-07-27T12:00:00Z".into(),
            policy: EvidencePolicy {
                consent: ConsentClass::Preproduction,
                retention_class: "local".into(),
            },
            capabilities: vec![CaptureCapability {
                capability: CaptureCapabilityKind::Commands,
                completeness: CaptureCompleteness::Complete,
                detail: None,
            }],
            max_events,
            max_artifacts: 2,
        }
    }

    #[test]
    fn bounded_recorder_retains_recent_events_and_reports_overflow() {
        let mut recorder = Recorder::new(config(3)).unwrap();
        let first = recorder.record(
            EventContext::default(),
            CaptureEventKind::OperationStart {
                name: "first".into(),
            },
        );
        let second = recorder.record(
            EventContext {
                causal_parent_ids: vec![first.clone()],
                monotonic_ns: 2,
                ..EventContext::default()
            },
            CaptureEventKind::OperationStart {
                name: "second".into(),
            },
        );
        recorder.record(
            EventContext {
                causal_parent_ids: vec![second],
                monotonic_ns: 3,
                ..EventContext::default()
            },
            CaptureEventKind::OperationEnd {
                name: "second".into(),
                outcome: OperationOutcome::Succeeded,
            },
        );
        recorder.record(
            EventContext {
                monotonic_ns: 4,
                ..EventContext::default()
            },
            CaptureEventKind::Checkpoint {
                name: "done".into(),
                attributes: serde_json::Value::Null,
            },
        );

        let batch = recorder.finish().unwrap();
        assert_eq!(batch.events.len(), 3);
        assert!(batch.events.iter().any(|event| matches!(
            event.event,
            CaptureEventKind::Defect {
                defect: CaptureDefectKind::Dropped,
                ..
            }
        )));
        assert!(batch
            .events
            .iter()
            .all(|event| !event.causal_parent_ids.contains(&first)));
    }

    #[test]
    fn recorder_rejects_unbounded_configuration() {
        assert!(Recorder::new(config(1)).is_err());
        assert!(Recorder::new(config(MAX_CAPTURE_EVENTS + 1)).is_err());
    }
}
