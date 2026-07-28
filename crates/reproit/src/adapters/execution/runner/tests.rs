use super::*;
use reproit_protocol::{
    ExecutionDestination, ObservationAuthority, ObservationKind, ObservationTarget, PlanBinding,
    ReproductionPlan, PLAN_VERSION,
};

fn provider() -> CommandProvider {
    CommandProvider {
        authority: MechanismAuthority::TrustedCheckout,
        phase: ExecutionPhase::Launch,
        argv: vec!["sh".into(), "-c".into(), "exit 17".into()],
        environment: BTreeMap::new(),
        working_directory: None,
        timeout_ms: 1_000,
        clean_exit_codes: vec![0],
        observation: Some(CommandObservation {
            identity: "service-start-failure".into(),
            matcher: ObservationMatcher::ExitCode { code: 17 },
        }),
        cleanup: None,
    }
}

#[test]
fn provider_digest_changes_with_executable_mechanism() {
    let first = provider();
    let mut second = first.clone();
    second.argv[2] = "exit 18".into();
    assert_ne!(
        provider_digest(&first).unwrap(),
        provider_digest(&second).unwrap()
    );
}

#[test]
fn imported_plan_cannot_substitute_a_provider_template() {
    let provider = provider();
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        providers: BTreeMap::from([("service-start".into(), provider)]),
    };
    let plan = ReproductionPlan {
        version: PLAN_VERSION,
        id: "plan_test".into(),
        occurrence_id: "occ_test".into(),
        target: "current-checkout".into(),
        destination: ExecutionDestination::LocalProcess,
        bindings: vec![PlanBinding {
            requirement_id: "req_process_launch".into(),
            provider_id: "service-start".into(),
            mechanism_authority: MechanismAuthority::TrustedCheckout,
            template_digest: format!("sha256:{}", "a".repeat(64)),
            evidence_artifact_ids: vec![],
        }],
        observation: ObservationTarget {
            observation: ObservationKind::Exit,
            identity: "service-start-failure".into(),
            authority: ObservationAuthority::RuntimeDiagnosis,
        },
    };
    let assessment = CapabilityAssessment {
        occurrence_id: "occ_test".into(),
        status: AssessmentStatus::Eligible,
        requirements: vec![ReproductionRequirement {
            id: "req_process_launch".into(),
            level: RequirementLevel::Required,
            requirement: RequirementKind::Process {
                role: "service".into(),
                operation: ProcessOperation::Launch,
            },
            evidence_artifact_ids: vec![],
        }],
        unresolved: vec![],
    };
    let error = resolve_providers(&plan, &assessment, &catalog).unwrap_err();
    assert!(error
        .to_string()
        .contains("changed since the plan was compiled"));
}

#[test]
fn project_config_owns_execution_providers() {
    let root = temporary_root("project-config");
    std::fs::write(
        root.join("reproit.yaml"),
        r#"
app:
  platform: web
execution:
  version: 1
  providers:
    service-start:
      authority: trusted-checkout
      phase: launch
      argv: [sh, -c, "exit 17"]
      timeoutMs: 1000
      cleanExitCodes: [0]
      observation:
        identity: service-start-failure
        kind: exit-code
        code: 17
"#,
    )
    .unwrap();
    let catalog = load_catalog(&root, None).unwrap();
    assert!(catalog.providers.contains_key("service-start"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn command_output_is_bounded_while_the_pipe_is_drained() {
    let root = temporary_root("output");
    let result = run_command(
        &root,
        &["sh".into(), "-c".into(), "yes x | head -c 1100000".into()],
        &BTreeMap::new(),
        None,
        5_000,
    )
    .await
    .unwrap();
    assert_eq!(result.stdout.len(), MAX_OUTPUT_BYTES);
    assert!(result.output_truncated);
    assert_eq!(result.exit_code, Some(0));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn imported_process_occurrence_compiles_and_reproduces_without_ui() {
    use reproit_protocol::{
        ConsentClass, EvidencePolicy, EvidenceSource, FailureObservation, OccurrenceEnvelope,
        SubjectIdentity, UnresolvedRequirement, UnresolvedRequirementReason, OCCURRENCE_VERSION,
        PACKAGE_VERSION,
    };

    let root = temporary_root("compile");
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        providers: BTreeMap::from([("service-start".into(), provider())]),
    };
    std::fs::write(
        root.join("reproit.execution.yaml"),
        serde_yaml::to_string(&catalog).unwrap(),
    )
    .unwrap();

    let occurrence = OccurrenceEnvelope {
        version: OCCURRENCE_VERSION,
        occurrence_id: "occ_process_compile_test".into(),
        source: EvidenceSource::SupportBundle,
        subject: SubjectIdentity {
            product: "suite".into(),
            component: "service".into(),
            platform: None,
        },
        observed_at: "2026-07-27T00:00:00Z".into(),
        received_at: "2026-07-27T00:00:01Z".into(),
        deployment: None,
        observations: vec![FailureObservation {
            kind: ObservationKind::Exit,
            authority: ObservationAuthority::SourceClaim,
            summary: "service startup failed".into(),
            signature: Some("service-start-failure".into()),
            observation_point: Some("service/start".into()),
            artifact_ids: vec![],
        }],
        artifacts: vec![],
        capture_defects: vec![],
        policy: EvidencePolicy {
            consent: ConsentClass::SupportExport,
            retention_class: "test".into(),
        },
    };
    let requirement = ReproductionRequirement {
        id: "req_process_launch".into(),
        level: RequirementLevel::Required,
        requirement: RequirementKind::Process {
            role: "service".into(),
            operation: ProcessOperation::Launch,
        },
        evidence_artifact_ids: vec![],
    };
    let assessment = CapabilityAssessment {
        occurrence_id: occurrence.occurrence_id.clone(),
        status: AssessmentStatus::Incomplete,
        requirements: vec![requirement.clone()],
        unresolved: vec![UnresolvedRequirement {
            requirement_id: requirement.id.clone(),
            reason: UnresolvedRequirementReason::MissingEvidence,
            detail: "bind a trusted process".into(),
        }],
    };
    let mut package = ReproductionPackage {
        version: PACKAGE_VERSION,
        id: String::new(),
        occurrence,
        assessment,
        plan: None,
        capsule: None,
        legacy: None,
    };
    package.finalize_id().unwrap();
    let compiled = compile_package(
        &root,
        &package,
        &BTreeMap::from([(requirement.id, "service-start".to_string())]),
        "service-start-failure",
    )
    .unwrap();
    assert_eq!(compiled.assessment.status, AssessmentStatus::Eligible);
    let run = execute(&root, &compiled).await.unwrap();
    assert_eq!(run.verdict, ExecutionVerdict::Reproduced);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_compilation_uses_only_an_unambiguous_trusted_provider() {
    let root = temporary_root("automatic-compile");
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        providers: BTreeMap::from([("service-start".into(), provider())]),
    };
    std::fs::write(
        root.join("reproit.execution.yaml"),
        serde_yaml::to_string(&catalog).unwrap(),
    )
    .unwrap();
    let package = incomplete_process_package();

    let AutomaticCompilation::Compiled(compiled) =
        compile_package_automatically(&root, &package).unwrap()
    else {
        panic!("one compatible provider should compile automatically");
    };
    let binding = &compiled.plan.as_ref().unwrap().bindings[0];
    assert_eq!(binding.requirement_id, "req_process_launch");
    assert_eq!(binding.provider_id, "service-start");
    assert_eq!(
        binding.mechanism_authority,
        MechanismAuthority::TrustedCheckout
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_compilation_refuses_ambiguous_providers() {
    let root = temporary_root("ambiguous-compile");
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        providers: BTreeMap::from([
            ("service-start".into(), provider()),
            ("service-start-copy".into(), provider()),
        ]),
    };
    std::fs::write(
        root.join("reproit.execution.yaml"),
        serde_yaml::to_string(&catalog).unwrap(),
    )
    .unwrap();

    let AutomaticCompilation::Blocked(blockers) =
        compile_package_automatically(&root, &incomplete_process_package()).unwrap()
    else {
        panic!("ambiguous providers must not compile automatically");
    };
    assert_eq!(blockers.len(), 1);
    assert!(blockers[0].contains("ambiguous"));
    assert!(blockers[0].contains("service-start"));
    assert!(blockers[0].contains("service-start-copy"));
    std::fs::remove_dir_all(root).unwrap();
}

fn incomplete_process_package() -> ReproductionPackage {
    use reproit_protocol::{
        ConsentClass, EvidencePolicy, EvidenceSource, FailureObservation, OccurrenceEnvelope,
        SubjectIdentity, UnresolvedRequirement, UnresolvedRequirementReason, OCCURRENCE_VERSION,
        PACKAGE_VERSION,
    };
    let occurrence = OccurrenceEnvelope {
        version: OCCURRENCE_VERSION,
        occurrence_id: "occ_process_automatic_test".into(),
        source: EvidenceSource::SupportBundle,
        subject: SubjectIdentity {
            product: "suite".into(),
            component: "service".into(),
            platform: None,
        },
        observed_at: "2026-07-27T00:00:00Z".into(),
        received_at: "2026-07-27T00:00:01Z".into(),
        deployment: None,
        observations: vec![FailureObservation {
            kind: ObservationKind::Exit,
            authority: ObservationAuthority::SourceClaim,
            summary: "service startup failed".into(),
            signature: Some("service-start-failure".into()),
            observation_point: Some("service/start".into()),
            artifact_ids: vec![],
        }],
        artifacts: vec![],
        capture_defects: vec![],
        policy: EvidencePolicy {
            consent: ConsentClass::SupportExport,
            retention_class: "test".into(),
        },
    };
    let requirement = ReproductionRequirement {
        id: "req_process_launch".into(),
        level: RequirementLevel::Required,
        requirement: RequirementKind::Process {
            role: "service".into(),
            operation: ProcessOperation::Launch,
        },
        evidence_artifact_ids: vec![],
    };
    let assessment = CapabilityAssessment {
        occurrence_id: occurrence.occurrence_id.clone(),
        status: AssessmentStatus::Incomplete,
        requirements: vec![requirement.clone()],
        unresolved: vec![UnresolvedRequirement {
            requirement_id: requirement.id,
            reason: UnresolvedRequirementReason::MissingEvidence,
            detail: "bind a trusted process".into(),
        }],
    };
    let mut package = ReproductionPackage {
        version: PACKAGE_VERSION,
        id: String::new(),
        occurrence,
        assessment,
        plan: None,
        capsule: None,
        legacy: None,
    };
    package.finalize_id().unwrap();
    package
}

fn temporary_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "reproit-execution-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}
