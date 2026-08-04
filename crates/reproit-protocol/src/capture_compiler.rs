//! Deterministic capture-to-occurrence compilation.
//!
//! This stage derives evidence requirements only. It never chooses executable
//! commands or grants a provider authority.

use crate::{
    AssessmentStatus, CapabilityAssessment, CaptureBatch, CaptureCapabilityKind,
    CaptureCompleteness, CaptureDefect, CaptureDefectKind, CaptureEventKind, CapturedValue,
    CollectionMethod, DependencyKind, EnvironmentKind, EvidenceSource, FailureObservation,
    OccurrenceEnvelope, ProtocolError, ReasonCode, ReproductionRequirement, RequirementKind,
    RequirementLevel, StateKind, SubjectIdentity, TriggerKind, UnresolvedRequirement,
    UnresolvedRequirementReason, OCCURRENCE_VERSION,
};
use std::collections::{BTreeMap, BTreeSet};

pub struct CaptureCompilation {
    pub occurrence: OccurrenceEnvelope,
    pub assessment: CapabilityAssessment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureAssessmentScope {
    SourceEnvironment,
    Portable,
}

pub fn compile_capture_failure(
    batch: &CaptureBatch,
    received_at: &str,
    scope: CaptureAssessmentScope,
) -> Result<Option<CaptureCompilation>, ProtocolError> {
    batch.validate()?;
    let observations = failure_observations(batch);
    if observations.is_empty() {
        return Ok(None);
    }
    let occurrence_id = occurrence_id(batch)?;
    let occurrence = OccurrenceEnvelope {
        version: OCCURRENCE_VERSION,
        occurrence_id: occurrence_id.clone(),
        source: EvidenceSource::ReproitCapture,
        subject: SubjectIdentity {
            product: batch.project_id.clone(),
            component: batch.emitter.component.clone(),
            platform: batch.emitter.runtime.clone(),
        },
        observed_at: batch.observed_at.clone(),
        received_at: received_at.to_string(),
        deployment: batch.deployment.clone(),
        observations,
        artifacts: batch
            .artifacts
            .iter()
            .cloned()
            .map(|mut artifact| {
                artifact.collection = CollectionMethod::FlightRecorder;
                artifact
            })
            .collect(),
        capture_defects: capture_defects(batch),
        policy: batch.policy.clone(),
    };
    occurrence.validate()?;
    let assessment = assess(batch, &occurrence_id, scope);
    assessment.validate(&occurrence)?;
    Ok(Some(CaptureCompilation {
        occurrence,
        assessment,
    }))
}

fn occurrence_id(batch: &CaptureBatch) -> Result<String, ProtocolError> {
    let bytes =
        serde_json::to_vec(batch).map_err(|_| ProtocolError::new(ReasonCode::InvalidEvent))?;
    Ok(crate::derive_occurrence_id("capture-batch-v1", &bytes))
}

fn failure_observations(batch: &CaptureBatch) -> Vec<FailureObservation> {
    batch
        .events
        .iter()
        .filter_map(|event| {
            let CaptureEventKind::Observation { failure } = &event.event else {
                return None;
            };
            Some(FailureObservation {
                kind: failure.observation,
                authority: failure.authority,
                summary: failure.summary.clone(),
                signature: failure.signature.clone(),
                observation_point: failure.observation_point.clone(),
                artifact_ids: failure.artifact_ids.clone(),
            })
        })
        .collect()
}

fn capture_defects(batch: &CaptureBatch) -> Vec<CaptureDefect> {
    let mut defects = Vec::new();
    for event in &batch.events {
        if let CaptureEventKind::Defect {
            defect,
            detail,
            artifact_id,
        } = &event.event
        {
            defects.push(CaptureDefect {
                kind: *defect,
                detail: detail.clone(),
                artifact_id: artifact_id.clone(),
            });
        }
    }
    for capability in &batch.capabilities {
        let kind = match capability.completeness {
            CaptureCompleteness::Complete => continue,
            CaptureCompleteness::Partial => CaptureDefectKind::Truncated,
            CaptureCompleteness::Unavailable => CaptureDefectKind::Unavailable,
        };
        defects.push(CaptureDefect {
            kind,
            detail: format!(
                "{:?}: {}",
                capability.capability,
                capability.detail.as_deref().unwrap_or("unavailable")
            ),
            artifact_id: None,
        });
    }
    defects
}

fn assess(
    batch: &CaptureBatch,
    occurrence_id: &str,
    scope: CaptureAssessmentScope,
) -> CapabilityAssessment {
    let capabilities = batch
        .capabilities
        .iter()
        .map(|capability| (capability.capability, capability.completeness))
        .collect::<BTreeMap<_, _>>();
    let input_artifacts = batch
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CaptureEventKind::Input { value, .. } => artifact_id(value),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut requirements = Vec::new();
    let mut unresolved = Vec::new();
    let mut seen = BTreeSet::new();
    let mut requirement_number = 1usize;

    for event in &batch.events {
        match &event.event {
            CaptureEventKind::Trigger {
                trigger,
                subject,
                value,
            } => {
                let key = format!("trigger:{trigger:?}:{subject}");
                if !seen.insert(key) {
                    continue;
                }
                let mut artifacts = input_artifacts.iter().cloned().collect::<Vec<_>>();
                if let Some(id) = value.as_ref().and_then(artifact_id) {
                    artifacts.push(id);
                }
                artifacts.sort();
                artifacts.dedup();
                let id = requirement_id(requirement_number, "trigger");
                requirement_number += 1;
                requirements.push(ReproductionRequirement {
                    id: id.clone(),
                    level: RequirementLevel::Required,
                    requirement: RequirementKind::Trigger {
                        trigger: *trigger,
                        subject: subject.clone(),
                    },
                    evidence_artifact_ids: artifacts,
                });
                if !capability_complete(&capabilities, trigger_capability(*trigger)) {
                    unresolved.push(missing_capability(id, trigger_capability(*trigger)));
                } else if value.as_ref().is_some_and(structural_only) {
                    unresolved.push(missing_evidence(
                        id,
                        "replayable trigger value or construction recipe",
                    ));
                } else if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    unresolved.push(environment_bound_requirement(id));
                }
            }
            CaptureEventKind::StateAccess {
                state,
                subject,
                value,
                ..
            } => {
                let key = format!("state:{state:?}:{subject}");
                if !seen.insert(key) {
                    continue;
                }
                let id = requirement_id(requirement_number, "state");
                requirement_number += 1;
                requirements.push(ReproductionRequirement {
                    id: id.clone(),
                    level: RequirementLevel::Required,
                    requirement: RequirementKind::State {
                        state: *state,
                        subject: subject.clone(),
                    },
                    evidence_artifact_ids: value
                        .as_ref()
                        .and_then(artifact_id)
                        .into_iter()
                        .collect(),
                });
                if value.is_none() {
                    unresolved.push(missing_evidence(id, "state value or construction recipe"));
                } else if value.as_ref().is_some_and(structural_only) {
                    unresolved.push(missing_evidence(
                        id,
                        "replayable state value or construction recipe",
                    ));
                } else if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    unresolved.push(environment_bound_requirement(id));
                } else if !capability_complete(&capabilities, state_capability(*state)) {
                    unresolved.push(missing_capability(id, state_capability(*state)));
                }
            }
            CaptureEventKind::EnvironmentRead {
                environment,
                subject,
                value,
            } => {
                let key = format!("environment:{environment:?}:{subject}");
                if !seen.insert(key) {
                    continue;
                }
                let id = requirement_id(requirement_number, "environment");
                requirement_number += 1;
                let artifact = value.as_ref().and_then(artifact_id);
                requirements.push(ReproductionRequirement {
                    id: id.clone(),
                    level: RequirementLevel::Required,
                    requirement: RequirementKind::Environment {
                        environment: *environment,
                        required_value: deterministic_required_value(subject, value.as_ref()),
                    },
                    evidence_artifact_ids: artifact.clone().into_iter().collect(),
                });
                if value.is_none() {
                    unresolved.push(missing_evidence(id, "deterministic environment value"));
                } else if value.as_ref().is_some_and(structural_only) {
                    unresolved.push(missing_evidence(id, "replayable deterministic value"));
                } else if requires_stream_artifact(*environment) && artifact.is_none() {
                    unresolved.push(missing_evidence(id, "artifact-backed deterministic stream"));
                } else if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    unresolved.push(environment_bound_requirement(id));
                } else if !capability_complete(&capabilities, environment_capability(*environment))
                {
                    unresolved.push(missing_capability(id, environment_capability(*environment)));
                }
            }
            CaptureEventKind::Dependency {
                system,
                subject,
                value,
                ..
            } => {
                let key = format!("dependency:{system}:{subject}");
                if !seen.insert(key) {
                    continue;
                }
                let id = requirement_id(requirement_number, "dependency");
                requirement_number += 1;
                let dependency = if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    DependencyKind::EnvironmentBound
                } else {
                    DependencyKind::CapturedReplay
                };
                requirements.push(ReproductionRequirement {
                    id: id.clone(),
                    level: RequirementLevel::Required,
                    requirement: RequirementKind::Dependency {
                        dependency,
                        subject: format!("{system}:{subject}"),
                    },
                    evidence_artifact_ids: value
                        .as_ref()
                        .and_then(artifact_id)
                        .into_iter()
                        .collect(),
                });
                if value.is_none() {
                    unresolved.push(missing_evidence(id, "dependency result"));
                } else if value.as_ref().is_some_and(structural_only) {
                    unresolved.push(missing_evidence(id, "replayable dependency result"));
                } else if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    unresolved.push(environment_bound_requirement(id));
                }
            }
            _ => {}
        }
    }

    if requirements.is_empty() {
        if let Some(process) = batch.events.iter().find_map(|event| match &event.event {
            CaptureEventKind::ProcessStart { process, .. } => Some(process),
            _ => None,
        }) {
            let id = requirement_id(requirement_number, "startup");
            requirements.push(ReproductionRequirement {
                id: id.clone(),
                level: RequirementLevel::Required,
                requirement: RequirementKind::Trigger {
                    trigger: TriggerKind::ProcessStartup,
                    subject: process.executable.clone(),
                },
                evidence_artifact_ids: vec![],
            });
            if !capability_complete(&capabilities, CaptureCapabilityKind::ProcessTree) {
                unresolved.push(missing_capability(id, CaptureCapabilityKind::ProcessTree));
            }
        }
    }

    let status = if unresolved.is_empty() {
        AssessmentStatus::Eligible
    } else if unresolved
        .iter()
        .any(|item| item.reason == UnresolvedRequirementReason::UnauthorizedDestination)
    {
        AssessmentStatus::EnvironmentBound
    } else {
        AssessmentStatus::Incomplete
    };
    CapabilityAssessment {
        occurrence_id: occurrence_id.to_string(),
        status,
        requirements,
        unresolved,
    }
}

fn requirement_id(number: usize, suffix: &str) -> String {
    format!("req_{number:03}_{suffix}")
}

fn artifact_id(value: &CapturedValue) -> Option<String> {
    match value {
        CapturedValue::Artifact { artifact_id, .. } => Some(artifact_id.clone()),
        CapturedValue::Structural { .. }
        | CapturedValue::Replayable { .. }
        | CapturedValue::EnvironmentBound { .. } => None,
    }
}

fn environment_bound(value: &CapturedValue, scope: CaptureAssessmentScope) -> bool {
    match value {
        CapturedValue::EnvironmentBound { .. } => true,
        CapturedValue::Artifact { policy, .. } => {
            scope == CaptureAssessmentScope::Portable
                && *policy != crate::ArtifactPolicy::Exportable
        }
        CapturedValue::Structural { .. } | CapturedValue::Replayable { .. } => false,
    }
}

fn structural_only(value: &CapturedValue) -> bool {
    matches!(value, CapturedValue::Structural { .. })
}

fn deterministic_required_value(subject: &str, value: Option<&CapturedValue>) -> Option<String> {
    match value {
        Some(CapturedValue::Artifact { artifact_id, .. }) => {
            Some(format!("artifact:{artifact_id}"))
        }
        Some(CapturedValue::Replayable { value, .. })
            if value.is_string() || value.is_number() || value.is_boolean() =>
        {
            let encoded = value.to_string();
            (encoded.len() <= crate::MAX_TEXT_BYTES).then(|| format!("{subject}:{encoded}"))
        }
        _ => Some(subject.to_string()),
    }
}

fn requires_stream_artifact(environment: EnvironmentKind) -> bool {
    matches!(
        environment,
        EnvironmentKind::Clock
            | EnvironmentKind::WallClock
            | EnvironmentKind::MonotonicClock
            | EnvironmentKind::Randomness
            | EnvironmentKind::RandomBytes
    )
}

fn capability_complete(
    capabilities: &BTreeMap<CaptureCapabilityKind, CaptureCompleteness>,
    required: CaptureCapabilityKind,
) -> bool {
    capabilities.get(&required) == Some(&CaptureCompleteness::Complete)
}

fn trigger_capability(trigger: TriggerKind) -> CaptureCapabilityKind {
    match trigger {
        TriggerKind::UiAction => CaptureCapabilityKind::UserInterface,
        TriggerKind::HttpRequest => CaptureCapabilityKind::Http,
        TriggerKind::RpcRequest => CaptureCapabilityKind::Rpc,
        TriggerKind::Command
        | TriggerKind::Installer
        | TriggerKind::Upgrade
        | TriggerKind::Migration => CaptureCapabilityKind::Commands,
        TriggerKind::Message => CaptureCapabilityKind::Queue,
        TriggerKind::Timer => CaptureCapabilityKind::Timers,
        TriggerKind::ProcessStartup | TriggerKind::Signal => CaptureCapabilityKind::ProcessTree,
        TriggerKind::FilesystemEvent => CaptureCapabilityKind::Filesystem,
        TriggerKind::ResourcePressure => CaptureCapabilityKind::ResourcePressure,
        TriggerKind::ConcurrencySchedule => CaptureCapabilityKind::Concurrency,
        TriggerKind::DeviceInteraction => CaptureCapabilityKind::Device,
    }
}

fn state_capability(state: StateKind) -> CaptureCapabilityKind {
    match state {
        StateKind::Filesystem => CaptureCapabilityKind::Filesystem,
        StateKind::Registry => CaptureCapabilityKind::Environment,
        StateKind::Database => CaptureCapabilityKind::Database,
        StateKind::Cache => CaptureCapabilityKind::Cache,
        StateKind::Queue => CaptureCapabilityKind::Queue,
        StateKind::ObjectStore => CaptureCapabilityKind::ObjectStore,
        StateKind::ApplicationStorage => CaptureCapabilityKind::Filesystem,
        StateKind::Device => CaptureCapabilityKind::Device,
    }
}

fn environment_capability(environment: EnvironmentKind) -> CaptureCapabilityKind {
    match environment {
        EnvironmentKind::Clock | EnvironmentKind::WallClock | EnvironmentKind::MonotonicClock => {
            CaptureCapabilityKind::Clock
        }
        EnvironmentKind::Randomness
        | EnvironmentKind::RandomSeed
        | EnvironmentKind::RandomBytes => CaptureCapabilityKind::Randomness,
        _ => CaptureCapabilityKind::Environment,
    }
}

fn missing_capability(
    requirement_id: String,
    capability: CaptureCapabilityKind,
) -> UnresolvedRequirement {
    UnresolvedRequirement {
        requirement_id,
        reason: UnresolvedRequirementReason::MissingEvidence,
        detail: format!("capture capability {capability:?} was not complete"),
    }
}

fn missing_evidence(requirement_id: String, evidence: &str) -> UnresolvedRequirement {
    UnresolvedRequirement {
        requirement_id,
        reason: UnresolvedRequirementReason::MissingEvidence,
        detail: format!("capture did not retain required {evidence}"),
    }
}

fn environment_bound_requirement(requirement_id: String) -> UnresolvedRequirement {
    UnresolvedRequirement {
        requirement_id,
        reason: UnresolvedRequirementReason::UnauthorizedDestination,
        detail: "required evidence is restricted to its source environment".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CaptureCapability, CaptureEmitter, CaptureEmitterKind, CaptureEvent, EvidencePolicy,
        FailureRecord, ObservationAuthority, OperationOutcome, ProcessIdentity,
        CAPTURE_BATCH_VERSION,
    };

    fn command_failure() -> CaptureBatch {
        CaptureBatch {
            version: CAPTURE_BATCH_VERSION,
            batch_id: "cb_compile".into(),
            project_id: "tools".into(),
            session_id: "session-1".into(),
            emitter: CaptureEmitter {
                id: "collector".into(),
                kind: CaptureEmitterKind::HostCollector,
                component: "migrator".into(),
                runtime: Some("native".into()),
                parent_id: None,
            },
            deployment: None,
            observed_at: "2026-07-27T12:00:00Z".into(),
            policy: EvidencePolicy {
                consent: crate::ConsentClass::LocalAnalysis,
                retention_class: "local".into(),
            },
            capabilities: vec![
                CaptureCapability {
                    capability: CaptureCapabilityKind::Commands,
                    completeness: CaptureCompleteness::Complete,
                    detail: None,
                },
                CaptureCapability {
                    capability: CaptureCapabilityKind::ProcessTree,
                    completeness: CaptureCompleteness::Partial,
                    detail: Some("root process only".into()),
                },
            ],
            events: vec![
                CaptureEvent {
                    id: "evt_1".into(),
                    sequence: 1,
                    monotonic_ns: 1,
                    wall_time: None,
                    process_id: Some(7),
                    thread_id: None,
                    actor: None,
                    causal_parent_ids: vec![],
                    trace_id: None,
                    span_id: None,
                    event: CaptureEventKind::ProcessStart {
                        process: ProcessIdentity {
                            process_id: 7,
                            executable: "migrate".into(),
                            parent_process_id: None,
                            executable_hash: None,
                        },
                        arguments: None,
                        working_directory: None,
                    },
                },
                CaptureEvent {
                    id: "evt_2".into(),
                    sequence: 2,
                    monotonic_ns: 2,
                    wall_time: None,
                    process_id: Some(7),
                    thread_id: None,
                    actor: None,
                    causal_parent_ids: vec!["evt_1".into()],
                    trace_id: None,
                    span_id: None,
                    event: CaptureEventKind::Trigger {
                        trigger: TriggerKind::Command,
                        subject: "migrate".into(),
                        value: None,
                    },
                },
                CaptureEvent {
                    id: "evt_3".into(),
                    sequence: 3,
                    monotonic_ns: 3,
                    wall_time: None,
                    process_id: Some(7),
                    thread_id: None,
                    actor: None,
                    causal_parent_ids: vec!["evt_2".into()],
                    trace_id: None,
                    span_id: None,
                    event: CaptureEventKind::Observation {
                        failure: FailureRecord {
                            observation: crate::ObservationKind::Exit,
                            authority: ObservationAuthority::RuntimeDiagnosis,
                            summary: "migrator exited 17".into(),
                            signature: Some("process-exit:17".into()),
                            observation_point: Some("migrator/exit".into()),
                            artifact_ids: vec![],
                        },
                    },
                },
                CaptureEvent {
                    id: "evt_4".into(),
                    sequence: 4,
                    monotonic_ns: 4,
                    wall_time: None,
                    process_id: Some(7),
                    thread_id: None,
                    actor: None,
                    causal_parent_ids: vec!["evt_3".into()],
                    trace_id: None,
                    span_id: None,
                    event: CaptureEventKind::OperationEnd {
                        name: "migrate".into(),
                        outcome: OperationOutcome::Failed,
                    },
                },
            ],
            artifacts: vec![],
        }
    }

    #[test]
    fn command_failure_is_eligible_even_if_unneeded_process_tree_detail_is_partial() {
        let compiled = compile_capture_failure(
            &command_failure(),
            "2026-07-27T12:01:00Z",
            CaptureAssessmentScope::SourceEnvironment,
        )
        .unwrap()
        .unwrap();
        assert_eq!(compiled.assessment.status, AssessmentStatus::Eligible);
        assert_eq!(compiled.assessment.requirements.len(), 1);
        assert!(matches!(
            compiled.assessment.requirements[0].requirement,
            RequirementKind::Trigger {
                trigger: TriggerKind::Command,
                ..
            }
        ));
        assert!(compiled
            .occurrence
            .capture_defects
            .iter()
            .any(|defect| defect.kind == CaptureDefectKind::Truncated));
    }

    #[test]
    fn successful_capture_does_not_create_a_failure_occurrence() {
        let mut batch = command_failure();
        batch
            .events
            .retain(|event| !matches!(event.event, CaptureEventKind::Observation { .. }));
        for (index, event) in batch.events.iter_mut().enumerate() {
            event.sequence = (index + 1) as u64;
            event.causal_parent_ids.clear();
        }
        assert!(compile_capture_failure(
            &batch,
            "2026-07-27T12:01:00Z",
            CaptureAssessmentScope::SourceEnvironment,
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn local_only_command_input_is_environment_bound_for_portable_compilation() {
        let mut batch = command_failure();
        let artifact_id = format!("sha256:{}", "a".repeat(64));
        batch.artifacts.push(crate::EvidenceArtifact {
            id: artifact_id.clone(),
            kind: crate::EvidenceArtifactKind::InteractionTrace,
            media_type: "application/json".into(),
            bytes: 2,
            policy: crate::ArtifactPolicy::LocalAnalysisOnly,
            redaction: crate::RedactionState::UnredactedRestricted,
            collection: crate::CollectionMethod::FlightRecorder,
            encryption_key_id: None,
            name: Some("argv.json".into()),
        });
        let CaptureEventKind::Trigger { value, .. } = &mut batch.events[1].event else {
            panic!("fixture has command trigger");
        };
        *value = Some(CapturedValue::Artifact {
            artifact_id,
            policy: crate::ArtifactPolicy::LocalAnalysisOnly,
        });
        let compiled = compile_capture_failure(
            &batch,
            "2026-07-27T12:01:00Z",
            CaptureAssessmentScope::Portable,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            compiled.assessment.status,
            AssessmentStatus::EnvironmentBound
        );
    }

    #[test]
    fn deterministic_inputs_compile_with_typed_clock_and_randomness_requirements() {
        let mut batch = command_failure();
        batch.capabilities.push(CaptureCapability {
            capability: CaptureCapabilityKind::Clock,
            completeness: CaptureCompleteness::Complete,
            detail: None,
        });
        batch.capabilities.push(CaptureCapability {
            capability: CaptureCapabilityKind::Randomness,
            completeness: CaptureCompleteness::Complete,
            detail: None,
        });
        batch.events.push(CaptureEvent {
            id: "evt_5".into(),
            sequence: 5,
            monotonic_ns: 5,
            wall_time: None,
            process_id: Some(7),
            thread_id: None,
            actor: None,
            causal_parent_ids: vec![],
            trace_id: None,
            span_id: None,
            event: CaptureEventKind::EnvironmentRead {
                environment: EnvironmentKind::RandomSeed,
                subject: "seed".into(),
                value: Some(CapturedValue::Replayable {
                    value: serde_json::json!(42),
                    redaction: crate::RedactionState::NotRequired,
                }),
            },
        });
        batch.events.push(CaptureEvent {
            id: "evt_6".into(),
            sequence: 6,
            monotonic_ns: 6,
            wall_time: None,
            process_id: Some(7),
            thread_id: None,
            actor: None,
            causal_parent_ids: vec![],
            trace_id: None,
            span_id: None,
            event: CaptureEventKind::EnvironmentRead {
                environment: EnvironmentKind::WallClock,
                subject: "wall-clock-read-stream".into(),
                value: Some(CapturedValue::Structural {
                    shape: serde_json::json!({"reads": 3}),
                }),
            },
        });

        let compiled = compile_capture_failure(
            &batch,
            "2026-07-27T12:01:00Z",
            CaptureAssessmentScope::Portable,
        )
        .unwrap()
        .unwrap();
        assert!(compiled
            .assessment
            .requirements
            .iter()
            .any(|requirement| matches!(
                &requirement.requirement,
                RequirementKind::Environment {
                    environment: EnvironmentKind::RandomSeed,
                    required_value: Some(value),
                } if value == "seed:42"
            )));
        assert!(compiled
            .assessment
            .requirements
            .iter()
            .any(|requirement| matches!(
                requirement.requirement,
                RequirementKind::Environment {
                    environment: EnvironmentKind::WallClock,
                    ..
                }
            )));
        assert!(compiled
            .assessment
            .unresolved
            .iter()
            .any(|item| item.detail.contains("replayable deterministic value")));
    }
}
