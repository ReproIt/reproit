use super::*;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LineCase {
    name: String,
    line: String,
    outcome: String,
}

fn action_frame() -> EventFrame {
    EventFrame {
        run_id: "run-1".into(),
        sequence: 1,
        scope: EvidenceScope::Shared,
        event: Event::Action {
            actor: Some("alice".into()),
            action: "tap:key:send".into(),
        },
    }
}

fn occurrence() -> OccurrenceEnvelope {
    OccurrenceEnvelope {
        version: OCCURRENCE_VERSION,
        occurrence_id: "occ_0123456789abcdef".into(),
        source: EvidenceSource::SupportBundle,
        subject: SubjectIdentity {
            product: "desktop-suite".into(),
            component: "index-service".into(),
            platform: Some("windows".into()),
        },
        observed_at: "2026-07-27T12:00:00Z".into(),
        received_at: "2026-07-27T12:01:00Z".into(),
        deployment: Some(DeploymentIdentity {
            version: Some("1.2.3".into()),
            commit: None,
            platforms: Vec::new(),
            platform_gaps: Vec::new(),
        }),
        observations: vec![FailureObservation {
            kind: ObservationKind::Exception,
            authority: ObservationAuthority::RuntimeDiagnosis,
            summary: "index service failed during startup".into(),
            signature: Some("System.InvalidOperationException:index-start".into()),
            observation_point: Some("index-service/startup".into()),
            artifact_ids: vec![],
        }],
        artifacts: vec![],
        capture_defects: vec![],
        policy: EvidencePolicy {
            consent: ConsentClass::SupportExport,
            retention_class: "support-30d".into(),
        },
    }
}

fn eligible_assessment(occurrence: &OccurrenceEnvelope) -> CapabilityAssessment {
    CapabilityAssessment {
        occurrence_id: occurrence.occurrence_id.clone(),
        status: AssessmentStatus::Eligible,
        requirements: vec![ReproductionRequirement {
            id: "req_process_launch".into(),
            level: RequirementLevel::Required,
            requirement: RequirementKind::Process {
                role: "index-service".into(),
                operation: ProcessOperation::Launch,
            },
            evidence_artifact_ids: vec![],
        }],
        unresolved: vec![],
    }
}

fn process_plan(
    occurrence: &OccurrenceEnvelope,
    assessment: &CapabilityAssessment,
) -> ReproductionPlan {
    let mut plan = ReproductionPlan {
        version: PLAN_VERSION,
        id: String::new(),
        occurrence_id: occurrence.occurrence_id.clone(),
        target: "current-checkout".into(),
        destination: ExecutionDestination::LocalVm {
            platform: "windows-x86_64".into(),
        },
        bindings: vec![PlanBinding {
            requirement_id: assessment.requirements[0].id.clone(),
            provider_id: "trusted-dotnet-service".into(),
            mechanism_authority: MechanismAuthority::TrustedCheckout,
            template_digest: format!("sha256:{}", "a".repeat(64)),
            evidence_artifact_ids: vec![],
        }],
        observation: ObservationTarget {
            observation: ObservationKind::Exception,
            identity: "System.InvalidOperationException:index-start".into(),
            authority: ObservationAuthority::RuntimeDiagnosis,
        },
    };
    plan.finalize_id().unwrap();
    plan
}

fn capture_batch() -> CaptureBatch {
    CaptureBatch {
        version: CAPTURE_BATCH_VERSION,
        batch_id: "cb_0123456789abcdef".into(),
        project_id: "project-demo".into(),
        session_id: "session-demo".into(),
        emitter: CaptureEmitter {
            id: "emitter-cli".into(),
            kind: CaptureEmitterKind::HostCollector,
            component: "invoice-importer".into(),
            runtime: Some("native".into()),
            parent_id: None,
        },
        deployment: Some(DeploymentIdentity {
            version: Some("1.2.3".into()),
            commit: Some("abc123".into()),
            platforms: Vec::new(),
            platform_gaps: Vec::new(),
        }),
        observed_at: "2026-07-27T12:00:00Z".into(),
        policy: EvidencePolicy {
            consent: ConsentClass::ApplicationTelemetry,
            retention_class: "production-30d".into(),
        },
        capabilities: vec![
            CaptureCapability {
                capability: CaptureCapabilityKind::ProcessTree,
                completeness: CaptureCompleteness::Complete,
                detail: None,
            },
            CaptureCapability {
                capability: CaptureCapabilityKind::Filesystem,
                completeness: CaptureCompleteness::Partial,
                detail: Some("metadata only".into()),
            },
        ],
        events: vec![
            CaptureEvent {
                id: "evt_start".into(),
                sequence: 1,
                monotonic_ns: 1,
                wall_time: Some("2026-07-27T12:00:00Z".into()),
                process_id: Some(42),
                thread_id: Some(1),
                actor: None,
                causal_parent_ids: vec![],
                trace_id: None,
                span_id: None,
                event: CaptureEventKind::ProcessStart {
                    process: ProcessIdentity {
                        process_id: 42,
                        executable: "invoice-importer".into(),
                        parent_process_id: None,
                        executable_hash: None,
                    },
                    arguments: Some(CapturedValue::Structural {
                        shape: serde_json::json!({
                            "count": 2,
                            "flags": ["--source"]
                        }),
                    }),
                    working_directory: None,
                },
            },
            CaptureEvent {
                id: "evt_failure".into(),
                sequence: 2,
                monotonic_ns: 2,
                wall_time: Some("2026-07-27T12:00:01Z".into()),
                process_id: Some(42),
                thread_id: Some(1),
                actor: None,
                causal_parent_ids: vec!["evt_start".into()],
                trace_id: None,
                span_id: None,
                event: CaptureEventKind::Observation {
                    failure: FailureRecord {
                        observation: ObservationKind::Exit,
                        authority: ObservationAuthority::RuntimeDiagnosis,
                        summary: "invoice importer exited with status 17".into(),
                        signature: Some("process-exit:17".into()),
                        observation_point: Some("invoice-importer/exit".into()),
                        artifact_ids: vec![],
                    },
                },
            },
        ],
        artifacts: vec![],
    }
}

#[test]
fn universal_capture_batch_accepts_a_non_ui_command_failure() {
    capture_batch().validate().unwrap();
}

#[test]
fn capture_batch_rejects_forward_causal_references() {
    let mut batch = capture_batch();
    batch.events[0].causal_parent_ids = vec!["evt_failure".into()];
    assert_eq!(
        batch.validate().unwrap_err().reason,
        ReasonCode::InvalidSequence
    );
}

#[test]
fn capture_batch_rejects_partial_capability_without_reason() {
    let mut batch = capture_batch();
    batch.capabilities[1].detail = None;
    assert_eq!(
        batch.validate().unwrap_err().reason,
        ReasonCode::InvalidEvent
    );
}

#[test]
fn capture_batch_rejects_unredacted_replayable_values() {
    let mut batch = capture_batch();
    let CaptureEventKind::ProcessStart { arguments, .. } = &mut batch.events[0].event else {
        panic!("fixture starts with a process");
    };
    *arguments = Some(CapturedValue::Replayable {
        value: serde_json::json!({"token": "secret"}),
        redaction: RedactionState::UnredactedRestricted,
    });
    assert_eq!(
        batch.validate().unwrap_err().reason,
        ReasonCode::InvalidEvent
    );
}

#[test]
fn legacy_ui_and_finding_frames_translate_into_the_universal_capture_model() {
    let finding = FindingIdentity {
        oracle: "crash".into(),
        invariant: "no-exception".into(),
        kind: "crash".into(),
        message: "boom".into(),
        frame: "checkout/submit".into(),
        trigger: "tap:submit".into(),
        boundary: None,
    };
    let legacy = EventBatch {
        version: VERSION,
        batch_id: "legacy-batch".into(),
        app_id: "shop".into(),
        deployment: None,
        frames: vec![
            EventFrame {
                run_id: "run-legacy".into(),
                sequence: 1,
                scope: EvidenceScope::Shared,
                event: Event::Action {
                    actor: Some("user".into()),
                    action: "tap:submit".into(),
                },
            },
            EventFrame {
                run_id: "run-legacy".into(),
                sequence: 2,
                scope: EvidenceScope::Shared,
                event: Event::Finding {
                    signature: "crash:checkout".into(),
                    message: "boom".into(),
                    identity: finding,
                    path: vec![],
                    context: BTreeMap::new(),
                },
            },
        ],
        evidence: vec![],
    };
    let capture = translate_event_batch(
        &legacy,
        "2026-07-27T12:00:00Z".into(),
        EvidencePolicy {
            consent: ConsentClass::ApplicationTelemetry,
            retention_class: "production".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        capture.events[0].event,
        CaptureEventKind::Trigger {
            trigger: TriggerKind::UiAction,
            ..
        }
    ));
    assert!(matches!(
        capture.events[1].event,
        CaptureEventKind::Observation { .. }
    ));
    assert_eq!(
        capture.capabilities[0].capability,
        CaptureCapabilityKind::UserInterface
    );
}

#[test]
fn legacy_batches_split_into_one_universal_capture_per_run() {
    let mut second = action_frame();
    second.run_id = "run-2".into();
    second.sequence = 2;
    let legacy = EventBatch {
        version: VERSION,
        batch_id: "legacy-multi".into(),
        app_id: "shop".into(),
        deployment: None,
        frames: vec![action_frame(), second],
        evidence: vec![],
    };
    let policy = EvidencePolicy {
        consent: ConsentClass::ApplicationTelemetry,
        retention_class: "production".into(),
    };
    let captures =
        translate_event_batches(&legacy, "2026-07-27T12:00:00Z".into(), policy.clone()).unwrap();
    assert_eq!(captures.len(), 2);
    assert_eq!(captures[0].session_id, "run-1");
    assert_eq!(captures[1].session_id, "run-2");
    assert!(translate_event_batch(&legacy, "2026-07-27T12:00:00Z".into(), policy).is_err());
}

#[test]
fn support_bundle_manifest_binds_payload_and_excludes_only_signature_bytes() {
    let payload = "b".repeat(64);
    let mut manifest = SupportBundleManifest {
        version: SUPPORT_BUNDLE_VERSION,
        bundle_id: format!("rpb_{payload}"),
        occurrence: occurrence(),
        encryption: BundleEncryption {
            algorithm: BundleEncryptionAlgorithm::Xchacha20Poly1305,
            recipient_key_id: "key_support".into(),
            nonce: "c".repeat(48),
        },
        payload_sha256: format!("sha256:{payload}"),
        signature: BundleSignature {
            algorithm: BundleSignatureAlgorithm::Ed25519,
            key_id: "sig_support".into(),
            public_key: "d".repeat(64),
            signature: "e".repeat(128),
        },
    };
    manifest.validate().unwrap();
    let signing_bytes = manifest.signing_bytes().unwrap();
    manifest.signature.signature = "f".repeat(128);
    assert_eq!(manifest.signing_bytes().unwrap(), signing_bytes);
    manifest.payload_sha256 = format!("sha256:{}", "a".repeat(64));
    assert!(manifest.validate().is_err());
}

#[test]
fn frame_round_trip_is_exact() {
    let frame = action_frame();
    let line = frame.encode_line().unwrap();
    assert_eq!(decode_frame_line(&line).unwrap(), frame);
}

#[test]
fn canonical_line_corpus_has_exact_outcomes() {
    let cases: Vec<LineCase> =
        serde_json::from_str(include_str!("../fixtures/event-lines-v1.json")).unwrap();
    for case in cases {
        let actual = match decode_frame_line(&case.line) {
            Ok(_) => "accepted",
            Err(defect) => defect.reason.as_str(),
        };
        assert_eq!(actual, case.outcome, "case: {}", case.name);
    }
}

#[test]
fn canonical_line_corpus_is_the_frozen_v1_contract() {
    let actual = Sha256::digest(include_bytes!("../fixtures/event-lines-v1.json"));
    assert_eq!(
        hex::encode(actual),
        "4fc2e740e7e2a4f10d04ec58cfee272e50216fb13f59e4855e31091992376c36"
    );
}

#[test]
fn oversized_scoped_frame_retains_only_bounded_attribution() {
    let line = format!(
        "REPROIT/1 contract 0123456789abcdef 7 run-1 {}",
        "x".repeat(MAX_FRAME_BYTES)
    );
    let defect = decode_frame_line(&line).unwrap_err();
    assert_eq!(defect.reason, ReasonCode::FrameTooLarge);
    assert!(defect.scope.affects_contract("0123456789abcdef"));
    assert!(!defect.scope.affects_contract("fedcba9876543210"));
}

#[test]
fn evidence_graph_rejects_forward_parent_references() {
    let parent = ArtifactNode::new(ArtifactKind::RawCapture, vec![], Value::Null).unwrap();
    let child = ArtifactNode::new(
        ArtifactKind::NormalizedTrace,
        vec![parent.id.clone()],
        Value::Null,
    )
    .unwrap();
    let graph = EvidenceGraph {
        run_id: "run-1".into(),
        root: child.id.clone(),
        nodes: vec![child, parent],
    };
    assert!(graph.validate().is_err());
}

#[test]
fn proof_ledger_promotes_only_complete_exact_proof() {
    let ledger = ProofLedger::from_stages(
        vec!["fnd_0123456789ab".into()],
        vec![AuthoritySource::AuthoredContract],
        EvaluationStatus::Violation,
        vec![],
        ConfirmationStatus::Reproduced,
        true,
        MinimizationStatus::Preserved,
    )
    .unwrap();
    assert_eq!(ledger.promotion, PromotionStatus::Confirmed);
    assert!(ledger.blockers.is_empty());

    let node = ArtifactNode::new(
        ArtifactKind::ProofLedger,
        vec![],
        serde_json::to_value(&ledger).unwrap(),
    )
    .unwrap();
    let graph = EvidenceGraph {
        run_id: "run-1".into(),
        root: node.id.clone(),
        nodes: vec![node],
    };
    assert_eq!(graph.proof_ledger().unwrap(), Some(ledger));
}

#[test]
fn proof_ledger_canonicalizes_set_like_fields() {
    let ledger = ProofLedger::from_stages(
        vec!["second".into(), "first".into(), "first".into()],
        vec![
            AuthoritySource::RuntimeDiagnosis,
            AuthoritySource::AuthoredContract,
            AuthoritySource::RuntimeDiagnosis,
        ],
        EvaluationStatus::Abstain,
        vec![ReasonCode::NoObservations, ReasonCode::NoObservations],
        ConfirmationStatus::NotAttempted,
        false,
        MinimizationStatus::NotAttempted,
    )
    .unwrap();
    assert_eq!(ledger.finding_identities, vec!["first", "second"]);
    assert_eq!(
        ledger.authority,
        vec![
            AuthoritySource::AuthoredContract,
            AuthoritySource::RuntimeDiagnosis,
        ]
    );
    assert_eq!(ledger.evaluation_reasons, vec![ReasonCode::NoObservations]);
}

#[test]
fn proof_ledger_rejects_forged_confirmation() {
    let mut ledger = ProofLedger::from_stages(
        vec!["fnd_0123456789ab".into()],
        vec![],
        EvaluationStatus::Violation,
        vec![],
        ConfirmationStatus::Reproduced,
        true,
        MinimizationStatus::Preserved,
    )
    .unwrap();
    assert_eq!(ledger.promotion, PromotionStatus::Candidate);
    assert_eq!(ledger.blockers, vec![PromotionBlocker::MissingAuthority]);

    ledger.promotion = PromotionStatus::Confirmed;
    ledger.blockers.clear();
    let node = ArtifactNode::new(
        ArtifactKind::ProofLedger,
        vec![],
        serde_json::to_value(ledger).unwrap(),
    )
    .unwrap();
    let graph = EvidenceGraph {
        run_id: "run-1".into(),
        root: node.id.clone(),
        nodes: vec![node],
    };
    assert_eq!(
        graph.validate().unwrap_err().reason,
        ReasonCode::InvalidArtifact
    );
}

#[test]
fn source_neutral_occurrence_is_valid_without_ui_actions() {
    let occurrence = occurrence();
    assert!(occurrence.validate().is_ok());
    let encoded = serde_json::to_value(&occurrence).unwrap();
    assert!(encoded.get("actions").is_none());
    assert!(encoded.get("command").is_none());
}

#[test]
fn unredacted_artifact_cannot_be_exportable() {
    let mut occurrence = occurrence();
    occurrence.artifacts.push(EvidenceArtifact {
        id: format!("sha256:{}", "b".repeat(64)),
        kind: EvidenceArtifactKind::CrashDump,
        media_type: "application/octet-stream".into(),
        bytes: 1024,
        policy: ArtifactPolicy::Exportable,
        redaction: RedactionState::UnredactedRestricted,
        collection: CollectionMethod::CrashCollector,
        encryption_key_id: Some("customer-key".into()),
        name: Some("service.dmp".into()),
    });
    assert_eq!(
        occurrence.validate().unwrap_err().reason,
        ReasonCode::InvalidArtifact
    );
}

#[test]
fn assessment_cannot_call_required_missing_evidence_eligible() {
    let occurrence = occurrence();
    let mut assessment = eligible_assessment(&occurrence);
    assessment.unresolved.push(UnresolvedRequirement {
        requirement_id: assessment.requirements[0].id.clone(),
        reason: UnresolvedRequirementReason::MissingEvidence,
        detail: "startup configuration inventory was not collected".into(),
    });
    assert_eq!(
        assessment.validate(&occurrence).unwrap_err().reason,
        ReasonCode::InvalidEvent
    );
    assessment.status = AssessmentStatus::Incomplete;
    assert!(assessment.validate(&occurrence).is_ok());
}

#[test]
fn non_ui_package_is_executable_through_a_trusted_plan() {
    let occurrence = occurrence();
    let assessment = eligible_assessment(&occurrence);
    let plan = process_plan(&occurrence, &assessment);
    let mut package = ReproductionPackage {
        version: PACKAGE_VERSION,
        id: String::new(),
        occurrence,
        assessment,
        plan: Some(plan),
        capsule: None,
        legacy: None,
    };
    package.finalize_id().unwrap();
    assert!(package.validate().is_ok());
}

#[test]
fn evidence_cannot_deserialize_as_mechanism_authority() {
    let binding = serde_json::json!({
        "requirementId": "req_process_launch",
        "providerId": "from-bundle",
        "mechanismAuthority": "evidence",
        "templateDigest": format!("sha256:{}", "c".repeat(64)),
        "evidenceArtifactIds": []
    });
    assert!(serde_json::from_value::<PlanBinding>(binding).is_err());
}

#[test]
fn universal_capture_v1_conformance_fixture_is_valid() {
    let raw = include_str!("../fixtures/capture-batch-v1.json");
    let batch: CaptureBatch = serde_json::from_str(raw).unwrap();
    batch.validate().unwrap();
    let compilation = compile_capture_failure(
        &batch,
        "2026-07-27T12:01:00Z",
        CaptureAssessmentScope::Portable,
    )
    .unwrap()
    .unwrap();
    assert_eq!(compilation.assessment.status, AssessmentStatus::Eligible);
    assert_eq!(
        compilation.occurrence.observations[0].signature.as_deref(),
        Some("orders:create:unique-violation")
    );
}
