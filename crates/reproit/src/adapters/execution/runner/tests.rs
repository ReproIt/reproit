use super::*;
use reproit_protocol::{
    DependencyKind, EnvironmentKind, ExecutionDestination, ObservationAuthority, ObservationKind,
    ObservationTarget, PlanBinding, ReproductionPlan, TriggerKind, PLAN_VERSION,
};

fn provider() -> CommandProvider {
    CommandProvider {
        authority: MechanismAuthority::TrustedCheckout,
        phase: ExecutionPhase::Launch,
        capabilities: BTreeSet::new(),
        source: None,
        cell: None,
        debug: None,
        argv: vec!["sh".into(), "-c".into(), "exit 17".into()],
        environment: BTreeMap::new(),
        working_directory: None,
        timeout_ms: 1_000,
        clean_exit_codes: vec![0],
        observation: Some(CommandObservation {
            identity: "service-start-failure".into(),
            matcher: ObservationMatcher::ExitCode { code: 17 },
        }),
        state_fingerprint: None,
        cleanup: None,
    }
}

fn write_project_catalog(root: &Path, catalog: &ProviderCatalog) {
    let serialized = serde_yaml::to_string(catalog).unwrap();
    let indented = serialized
        .lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        root.join("reproit.yaml"),
        format!("execution:\n{indented}\n"),
    )
    .unwrap();
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
fn cell_binding_digest_includes_the_compose_file() {
    let root = temporary_root("cell-binding-digest");
    std::fs::write(root.join("compose.yaml"), "services: {}\n").unwrap();
    let mut provider = provider();
    provider.cell = Some("backend".into());
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: BTreeMap::from([(
            "backend".into(),
            ReproductionCell::DockerCompose(DockerComposeCell {
                compose_file: "compose.yaml".into(),
                application_service: "app".into(),
                dependency_services: Vec::new(),
                allow_local_build: false,
                platform: None,
                timeout_ms: 1_000,
                debug: None,
            }),
        )]),
        providers: BTreeMap::from([("service-start".into(), provider.clone())]),
    };
    let first = provider_binding_digest(&root, &catalog, &provider).unwrap();
    std::fs::write(root.join("compose.yaml"), "services: {app: {}}\n").unwrap();
    let second = provider_binding_digest(&root, &catalog, &provider).unwrap();
    assert_ne!(first, second);
}

#[test]
fn interpreted_provider_source_is_bound_by_checkout_relative_digest() {
    let root = temporary_root("provider-source");
    let directory = root.join("validation");
    std::fs::create_dir_all(&directory).unwrap();
    let script = directory.join("oracle.mjs");
    std::fs::write(&script, "process.exit(17);\n").unwrap();
    let source = captured_provider_source(
        &root,
        &[
            "node".into(),
            "validation/oracle.mjs".into(),
            "--check".into(),
        ],
    )
    .unwrap();
    assert_eq!(source.path, PathBuf::from("validation/oracle.mjs"));
    validate_provider_source(&root, &source).unwrap();
    std::fs::write(&script, "process.exit(0);\n").unwrap();
    assert!(validate_provider_source(&root, &source)
        .unwrap_err()
        .to_string()
        .contains("changed"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn imported_plan_cannot_substitute_a_provider_template() {
    let root = temporary_root("imported-plan");
    let provider = provider();
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: BTreeMap::new(),
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
    let error = resolve_providers(&root, &plan, &assessment, &catalog).unwrap_err();
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
    let catalog = load_catalog(&root, None, None).unwrap();
    assert!(catalog.providers.contains_key("service-start"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_catalog_inspection_reports_only_declared_readiness() {
    let root = temporary_root("catalog-inspection");
    let mut launch = provider();
    launch.cleanup = Some(CommandTemplate {
        argv: vec!["sh".into(), "-c".into(), "exit 0".into()],
        environment: BTreeMap::new(),
        working_directory: None,
        timeout_ms: 1_000,
    });
    write_project_catalog(
        &root,
        &ProviderCatalog {
            version: CATALOG_VERSION,
            cells: BTreeMap::new(),
            providers: BTreeMap::from([("service-start".into(), launch)]),
        },
    );
    let inspected = inspect_project_catalog(&root).unwrap().unwrap();
    assert_eq!(inspected.provider_count, 1);
    assert_eq!(inspected.cell_count, 0);
    assert_eq!(inspected.debug_executor_count, 0);
    assert_eq!(inspected.phases, vec![ExecutionPhase::Launch]);
    assert_eq!(inspected.observation_count, 1);
    assert_eq!(inspected.state_fingerprint_count, 0);
    assert_eq!(inspected.source_pinned_count, 0);
    assert_eq!(inspected.cleanup_count, 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn provider_debug_capability_is_source_neutral_and_trigger_bound() {
    let root = temporary_root("provider-debug-capability");
    let mut trigger = provider();
    trigger.phase = ExecutionPhase::Trigger;
    trigger.debug = Some(DebugProfile {
        debugger: reproit_protocol::DebuggerKind::NodeInspector,
        argv: vec!["node".into(), "--inspect-brk=127.0.0.1:9229".into()],
        port: 9_229,
        local_source_root: ".".into(),
        target_source_root: "/workspace".into(),
    });
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: BTreeMap::new(),
        providers: BTreeMap::from([("trigger".into(), trigger.clone())]),
    };
    write_project_catalog(&root, &catalog);
    let inspected = inspect_project_catalog(&root).unwrap().unwrap();
    assert_eq!(inspected.debug_executor_count, 1);

    trigger.phase = ExecutionPhase::Launch;
    let invalid = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: BTreeMap::new(),
        providers: BTreeMap::from([("launch".into(), trigger)]),
    };
    assert!(validate_catalog(&root, &invalid)
        .unwrap_err()
        .to_string()
        .contains("requires phase trigger"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_process_plan_becomes_debug_ready_from_provider_capability() {
    let root = temporary_root("provider-debug-readiness");
    let mut trigger = provider();
    trigger.phase = ExecutionPhase::Trigger;
    trigger.debug = Some(DebugProfile {
        debugger: reproit_protocol::DebuggerKind::NodeInspector,
        argv: vec!["node".into(), "--inspect-brk=127.0.0.1:9229".into()],
        port: 9_229,
        local_source_root: ".".into(),
        target_source_root: "/workspace".into(),
    });
    write_project_catalog(
        &root,
        &ProviderCatalog {
            version: CATALOG_VERSION,
            cells: BTreeMap::new(),
            providers: BTreeMap::from([("trigger".into(), trigger)]),
        },
    );
    let mut package = incomplete_process_package();
    package.assessment.requirements[0].id = "req_command".into();
    package.assessment.requirements[0].requirement = RequirementKind::Trigger {
        trigger: TriggerKind::Command,
        subject: "captured command".into(),
    };
    package.assessment.unresolved[0].requirement_id = "req_command".into();
    package.id.clear();
    package.finalize_id().unwrap();
    let AutomaticCompilation::Compiled(compiled) =
        compile_package_automatically(&root, &package).unwrap()
    else {
        panic!("the trusted provider should compile");
    };
    let local_destinations = [
        ExecutionDestination::LocalProcess,
        ExecutionDestination::Simulator {
            platform: "android".into(),
        },
        ExecutionDestination::PhysicalDevice {
            platform: "ios".into(),
        },
        ExecutionDestination::LocalVm {
            platform: "windows".into(),
        },
    ];
    for destination in local_destinations {
        let mut candidate = (*compiled).clone();
        candidate.plan.as_mut().unwrap().destination = destination;
        candidate.plan.as_mut().unwrap().id.clear();
        candidate.plan.as_mut().unwrap().finalize_id().unwrap();
        candidate.id.clear();
        candidate.finalize_id().unwrap();
        let readiness = assess_package_readiness(&root, &candidate).unwrap();
        assert_eq!(
            readiness
                .dimension(reproit_protocol::ReadinessDimension::Debug)
                .status,
            reproit_protocol::ReadinessStatus::Ready
        );
    }

    let mut remote = (*compiled).clone();
    remote.plan.as_mut().unwrap().destination = ExecutionDestination::HostedWorker {
        worker_class: "linux".into(),
    };
    remote.plan.as_mut().unwrap().id.clear();
    remote.plan.as_mut().unwrap().finalize_id().unwrap();
    remote.id.clear();
    remote.finalize_id().unwrap();
    let readiness = assess_package_readiness(&root, &remote).unwrap();
    assert_eq!(
        readiness
            .dimension(reproit_protocol::ReadinessDimension::Debug)
            .status,
        reproit_protocol::ReadinessStatus::Blocked
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn reset_provider(expected: &str) -> CommandProvider {
    let mut reset = provider();
    reset.phase = ExecutionPhase::Reset;
    reset.argv = vec!["sh".into(), "-c".into(), "exit 0".into()];
    reset.observation = None;
    reset.state_fingerprint = Some(StateFingerprint {
        command: CommandTemplate {
            argv: vec!["sh".into(), "-c".into(), "printf clean-state".into()],
            environment: BTreeMap::new(),
            working_directory: None,
            timeout_ms: 1_000,
        },
        expected_sha256: expected.into(),
    });
    reset
}

#[test]
fn state_changing_providers_require_a_bounded_fingerprint_probe() {
    let root = temporary_root("state-fingerprint-required");
    let mut reset = provider();
    reset.phase = ExecutionPhase::Reset;
    reset.observation = None;
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: BTreeMap::new(),
        providers: BTreeMap::from([("reset-db".into(), reset)]),
    };
    assert!(validate_catalog(&root, &catalog)
        .unwrap_err()
        .to_string()
        .contains("requires stateFingerprint verification"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn state_changing_providers_cannot_substitute_a_failure_observation() {
    let root = temporary_root("state-fingerprint-observation");
    let expected = sha256_bytes(b"clean-state");
    let mut reset = reset_provider(&expected);
    reset.observation = provider().observation;
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: BTreeMap::new(),
        providers: BTreeMap::from([("reset-db".into(), reset)]),
    };
    assert!(validate_catalog(&root, &catalog)
        .unwrap_err()
        .to_string()
        .contains("must verify state"));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reset_provider_records_matching_state_fingerprint() {
    let root = temporary_root("state-fingerprint-match");
    let expected = sha256_bytes(b"clean-state");
    let reset = reset_provider(&expected);
    let (run, verdict) = execute_provider(&root, "reset-db", &reset, "unused")
        .await
        .unwrap();
    assert_eq!(verdict, ProviderVerdict::SetupPassed);
    assert_eq!(
        run.expected_state_fingerprint.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        run.actual_state_fingerprint.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(run.state_verified, Some(true));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reset_provider_fails_closed_on_state_fingerprint_mismatch() {
    let root = temporary_root("state-fingerprint-mismatch");
    let reset = reset_provider(&sha256_bytes(b"different-state"));
    let (run, verdict) = execute_provider(&root, "reset-db", &reset, "unused")
        .await
        .unwrap();
    let actual = sha256_bytes(b"clean-state");
    assert_eq!(verdict, ProviderVerdict::InfrastructureFailed);
    assert_eq!(
        run.actual_state_fingerprint.as_deref(),
        Some(actual.as_str())
    );
    assert_eq!(run.state_verified, Some(false));
    assert!(run.error.unwrap().contains("state fingerprint mismatch"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_catalog_inspection_distinguishes_missing_and_invalid_catalogs() {
    let root = temporary_root("catalog-diagnostics");
    std::fs::write(root.join("reproit.yaml"), "app:\n  platform: web\n").unwrap();
    assert_eq!(inspect_project_catalog(&root).unwrap(), None);

    std::fs::write(
        root.join("reproit.yaml"),
        "execution:\n  version: 99\n  providers: {}\n",
    )
    .unwrap();
    assert!(inspect_project_catalog(&root)
        .unwrap_err()
        .to_string()
        .contains("unsupported execution provider catalog version"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn removed_standalone_execution_catalog_is_not_loaded() {
    let root = temporary_root("standalone-catalog");
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: BTreeMap::new(),
        providers: BTreeMap::from([("service-start".into(), provider())]),
    };
    std::fs::write(
        root.join("reproit.execution.yaml"),
        serde_yaml::to_string(&catalog).unwrap(),
    )
    .unwrap();
    let error = load_catalog(&root, None, None).unwrap_err();
    assert!(error
        .to_string()
        .contains("add execution.providers to reproit.yaml"));
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
async fn cleanup_failures_are_recorded_and_fail_closed() {
    let root = temporary_root("cleanup-failure");
    let mut launch = provider();
    launch.cleanup = Some(CommandTemplate {
        argv: vec!["sh".into(), "-c".into(), "exit 23".into()],
        environment: BTreeMap::new(),
        working_directory: None,
        timeout_ms: 1_000,
    });
    let providers = vec![("service-start".into(), &launch)];
    let mut runs = Vec::new();
    assert_eq!(run_cleanup(&root, &providers, &mut runs).await, 1);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].exit_code, Some(23));
    assert!(runs[0]
        .error
        .as_deref()
        .is_some_and(|error| error.contains("cleanup command exited")));
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
        cells: BTreeMap::new(),
        providers: BTreeMap::from([("service-start".into(), provider())]),
    };
    write_project_catalog(&root, &catalog);

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
        cells: BTreeMap::new(),
        providers: BTreeMap::from([("service-start".into(), provider())]),
    };
    write_project_catalog(&root, &catalog);
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
        cells: BTreeMap::new(),
        providers: BTreeMap::from([
            ("service-start".into(), provider()),
            ("service-start-copy".into(), provider()),
        ]),
    };
    write_project_catalog(&root, &catalog);

    let AutomaticCompilation::Blocked(blockers) =
        compile_package_automatically(&root, &incomplete_process_package()).unwrap()
    else {
        panic!("ambiguous providers must not compile automatically");
    };
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0].code, "ambiguous-trusted-provider");
    assert_eq!(
        blockers[0].reason,
        reproit_protocol::UnresolvedRequirementReason::AmbiguousMapping
    );
    assert!(blockers[0].detail.contains("service-start"));
    assert!(blockers[0].detail.contains("service-start-copy"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn advanced_requirements_map_to_explicit_trusted_capabilities() {
    let requirement = |requirement| ReproductionRequirement {
        id: "req_capability".into(),
        level: RequirementLevel::Required,
        requirement,
        evidence_artifact_ids: vec![],
    };
    let cases = [
        (
            RequirementKind::Trigger {
                trigger: TriggerKind::ConcurrencySchedule,
                subject: "worker interleaving".into(),
            },
            TrustedCapability::Concurrency,
        ),
        (
            RequirementKind::Dependency {
                dependency: DependencyKind::DistributedSystem,
                subject: "replicated service".into(),
            },
            TrustedCapability::DistributedSystems,
        ),
        (
            RequirementKind::Observation {
                observation: ObservationKind::Performance,
                subject: "latency regression".into(),
            },
            TrustedCapability::Performance,
        ),
        (
            RequirementKind::Environment {
                environment: EnvironmentKind::Hardware,
                required_value: Some("gpu".into()),
            },
            TrustedCapability::Hardware,
        ),
        (
            RequirementKind::Environment {
                environment: EnvironmentKind::Kernel,
                required_value: Some("linux".into()),
            },
            TrustedCapability::Kernel,
        ),
        (
            RequirementKind::Environment {
                environment: EnvironmentKind::OperatingSystem,
                required_value: Some("windows".into()),
            },
            TrustedCapability::Environment,
        ),
    ];
    for (kind, expected) in cases {
        assert_eq!(
            required_trusted_capability(&requirement(kind)),
            Some(expected)
        );
    }
}

#[test]
fn automatic_compilation_returns_a_typed_blocker_for_an_untrusted_capability() {
    let root = temporary_root("unsupported-capability");
    let mut trigger_provider = provider();
    trigger_provider.phase = ExecutionPhase::Trigger;
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: BTreeMap::new(),
        providers: BTreeMap::from([("concurrency-trigger".into(), trigger_provider)]),
    };
    write_project_catalog(&root, &catalog);
    let mut package = incomplete_process_package();
    package.assessment.requirements[0].id = "req_concurrency".into();
    package.assessment.requirements[0].requirement = RequirementKind::Trigger {
        trigger: TriggerKind::ConcurrencySchedule,
        subject: "worker interleaving".into(),
    };
    package.assessment.unresolved[0].requirement_id = "req_concurrency".into();
    package.id.clear();
    package.finalize_id().unwrap();

    let AutomaticCompilation::Blocked(blockers) =
        compile_package_automatically(&root, &package).unwrap()
    else {
        panic!("a provider without the concurrency capability must not execute");
    };
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0].code, "unsupported-trusted-capability");
    assert_eq!(
        blockers[0].requirement_id.as_deref(),
        Some("req_concurrency")
    );
    assert_eq!(
        blockers[0].reason,
        reproit_protocol::UnresolvedRequirementReason::UnsupportedCapability
    );
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
