use super::*;
use crate::{
    CaptureCapability, CaptureEmitter, CaptureEmitterKind, CaptureEvent, EvidencePolicy,
    FailureRecord, ObservationAuthority, OperationOutcome, ProcessIdentity, CAPTURE_BATCH_VERSION,
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
            CaptureEvent {
                id: "evt_5".into(),
                sequence: 5,
                monotonic_ns: 5,
                wall_time: None,
                process_id: Some(7),
                thread_id: None,
                actor: None,
                causal_parent_ids: vec!["evt_4".into()],
                trace_id: None,
                span_id: None,
                event: CaptureEventKind::Checkpoint {
                    name: "determinism-envelope".into(),
                    attributes: serde_json::json!({
                        "observedAtMs": 1_753_632_000_000_i64,
                        "runtime": "native",
                        "replaySeed": 7
                    }),
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
    assert!(compiled.assessment.requirements.iter().any(|requirement| {
        matches!(
            requirement.requirement,
            RequirementKind::Trigger {
                trigger: TriggerKind::Command,
                ..
            }
        )
    }));
    assert!(!compiled
        .assessment
        .requirements
        .iter()
        .any(|requirement| matches!(requirement.requirement, RequirementKind::Observation { .. })));
    assert!(!compiled.assessment.requirements.iter().any(|requirement| {
        matches!(
            requirement.requirement,
            RequirementKind::Environment {
                environment: EnvironmentKind::Runtime,
                ..
            }
        )
    }));
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
fn observation_without_a_trigger_is_incomplete() {
    let mut batch = command_failure();
    batch.events.retain(|event| {
        !matches!(
            event.event,
            CaptureEventKind::Trigger { .. } | CaptureEventKind::ProcessStart { .. }
        )
    });
    let compiled = compile_capture_failure(
        &batch,
        "2026-07-27T12:01:00Z",
        CaptureAssessmentScope::Portable,
    )
    .unwrap()
    .unwrap();
    assert_eq!(compiled.assessment.status, AssessmentStatus::Incomplete);
    assert!(compiled
        .assessment
        .unresolved
        .iter()
        .any(|item| item.detail.contains("action that starts")));
}

#[test]
fn capture_without_a_determinism_envelope_is_incomplete() {
    let mut batch = command_failure();
    batch.events.retain(|event| {
        !matches!(
            &event.event,
            CaptureEventKind::Checkpoint { name, .. } if name == "determinism-envelope"
        )
    });
    let compiled = compile_capture_failure(
        &batch,
        "2026-07-27T12:01:00Z",
        CaptureAssessmentScope::Portable,
    )
    .unwrap()
    .unwrap();
    assert_eq!(compiled.assessment.status, AssessmentStatus::Incomplete);
    assert!(compiled
        .assessment
        .unresolved
        .iter()
        .any(|item| item.detail.contains("determinism envelope")));
}

#[test]
fn arbitrary_checkpoint_is_not_a_determinism_envelope() {
    let mut batch = command_failure();
    let checkpoint = batch
        .events
        .iter_mut()
        .find_map(|event| match &mut event.event {
            CaptureEventKind::Checkpoint { attributes, .. } => Some(attributes),
            _ => None,
        })
        .expect("fixture has a determinism envelope");
    *checkpoint = serde_json::json!({"anything": "present"});
    let compiled = compile_capture_failure(
        &batch,
        "2026-07-27T12:01:00Z",
        CaptureAssessmentScope::Portable,
    )
    .unwrap()
    .unwrap();
    assert_eq!(compiled.assessment.status, AssessmentStatus::Incomplete);
    assert!(compiled
        .assessment
        .unresolved
        .iter()
        .any(|item| item.detail.contains("valid determinism envelope")));
}

#[test]
fn environment_bound_input_blocks_portable_compilation() {
    let mut batch = command_failure();
    push_event(
        &mut batch,
        CaptureEventKind::Input {
            name: "request".into(),
            value: CapturedValue::EnvironmentBound {
                reference: "local-request-cache".into(),
            },
        },
    );
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
fn producer_reported_defect_blocks_eligibility() {
    let mut batch = command_failure();
    push_event(
        &mut batch,
        CaptureEventKind::Defect {
            defect: CaptureDefectKind::Dropped,
            detail: "required events were dropped".into(),
            artifact_id: None,
        },
    );
    let compiled = compile_capture_failure(
        &batch,
        "2026-07-27T12:01:00Z",
        CaptureAssessmentScope::Portable,
    )
    .unwrap()
    .unwrap();
    assert_eq!(compiled.assessment.status, AssessmentStatus::Incomplete);
    assert!(compiled
        .assessment
        .unresolved
        .iter()
        .any(|item| item.detail.contains("required events were dropped")));
}

#[test]
fn local_only_oracle_artifact_blocks_portable_compilation() {
    let mut batch = command_failure();
    let artifact_id = format!("sha256:{}", "b".repeat(64));
    batch.artifacts.push(crate::EvidenceArtifact {
        id: artifact_id.clone(),
        kind: crate::EvidenceArtifactKind::CrashDump,
        media_type: "application/octet-stream".into(),
        bytes: 1,
        policy: crate::ArtifactPolicy::LocalAnalysisOnly,
        redaction: crate::RedactionState::NotRequired,
        collection: crate::CollectionMethod::FlightRecorder,
        encryption_key_id: None,
        name: Some("oracle.bin".into()),
    });
    let observation = batch
        .events
        .iter_mut()
        .find_map(|event| match &mut event.event {
            CaptureEventKind::Observation { failure } => Some(failure),
            _ => None,
        })
        .expect("fixture has an observation");
    observation.artifact_ids.push(artifact_id);
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
fn ambiguous_database_effect_blocks_eligibility() {
    let mut batch = command_failure();
    batch.capabilities.push(CaptureCapability {
        capability: CaptureCapabilityKind::Database,
        completeness: CaptureCompleteness::Partial,
        detail: Some("query result was not captured".into()),
    });
    push_event(
        &mut batch,
        CaptureEventKind::Effect {
            effect: "read".into(),
            subject: "orders".into(),
            value: Some(CapturedValue::Replayable {
                value: serde_json::json!({"id": 1}),
                redaction: crate::RedactionState::NotRequired,
            }),
        },
    );
    let compiled = compile_capture_failure(
        &batch,
        "2026-07-27T12:01:00Z",
        CaptureAssessmentScope::Portable,
    )
    .unwrap()
    .unwrap();
    assert_eq!(compiled.assessment.status, AssessmentStatus::Incomplete);
    assert!(compiled
        .assessment
        .unresolved
        .iter()
        .any(|item| item.detail.contains("untyped effect event")));
}

#[test]
fn source_claim_is_not_an_executable_oracle() {
    let mut batch = command_failure();
    let observation = batch
        .events
        .iter_mut()
        .find_map(|event| match &mut event.event {
            CaptureEventKind::Observation { failure } => Some(failure),
            _ => None,
        })
        .expect("fixture has an observation");
    observation.authority = ObservationAuthority::SourceClaim;
    let compiled = compile_capture_failure(
        &batch,
        "2026-07-27T12:01:00Z",
        CaptureAssessmentScope::Portable,
    )
    .unwrap()
    .unwrap();
    assert_eq!(compiled.assessment.status, AssessmentStatus::Incomplete);
    assert!(compiled
        .assessment
        .unresolved
        .iter()
        .any(|item| item.detail.contains("runtime oracle")));
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
            environment: EnvironmentKind::RandomSeed,
            subject: "seed".into(),
            value: Some(CapturedValue::Replayable {
                value: serde_json::json!(42),
                redaction: crate::RedactionState::NotRequired,
            }),
        },
    });
    batch.events.push(CaptureEvent {
        id: "evt_7".into(),
        sequence: 7,
        monotonic_ns: 7,
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

fn push_event(batch: &mut CaptureBatch, event: CaptureEventKind) {
    let sequence = batch.events.len() as u64 + 1;
    batch.events.push(CaptureEvent {
        id: format!("evt_{sequence}"),
        sequence,
        monotonic_ns: sequence,
        wall_time: None,
        process_id: Some(7),
        thread_id: None,
        actor: None,
        causal_parent_ids: vec![],
        trace_id: None,
        span_id: None,
        event,
    });
}
