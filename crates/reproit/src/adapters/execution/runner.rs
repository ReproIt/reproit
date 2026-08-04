//! Trusted checkout-owned provider runner for source-neutral reproduction plans.
//!
//! Imported evidence can name a provider and carry bounded evidence references.
//! It cannot supply an argv, working directory, environment, or cleanup action.

use crate::domain::execution::{ExecutionPhase, ExecutionState, ExecutionVerdict, PhaseStatus};
use crate::domain::repro;
use anyhow::{Context, Result};
use reproit_protocol::{
    AssessmentStatus, CapabilityAssessment, ExecutionDestination, MechanismAuthority,
    ObservationAuthority, ObservationTarget, PlanBinding, ProcessOperation, ReproductionPackage,
    ReproductionPlan, ReproductionRequirement, RequirementKind, RequirementLevel, PLAN_VERSION,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

mod automatic;
mod catalog;
mod cell;
mod debug_control;
mod host_debug;
pub(crate) mod model;
mod process;
pub(crate) use automatic::{
    assess_package_readiness, compile_package_automatically, AutomaticCompilation,
    CompilationBlocker,
};
use catalog::*;
pub(crate) use catalog::{
    inspect_project_catalog, persist_plan_catalog, pinned_provider_digest, repin_guard_providers,
    repin_package_mechanism, source_digest,
};
pub(crate) use cell::ExecutionMode;
pub(crate) use model::DebugLaunchOptions;
pub(crate) use model::PlanRun;
use model::*;
use process::*;

const CATALOG_VERSION: u16 = 1;
const MAX_PROVIDERS: usize = 256;
const MAX_CELLS: usize = 32;
const MAX_COMMAND_ARGS: usize = 128;
const MAX_ENVIRONMENT_ENTRIES: usize = 128;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_TIMEOUT_MS: u64 = 10 * 60 * 1000;

pub(crate) fn locate_package(
    config_path: Option<&Path>,
    reference: &str,
) -> Option<(PathBuf, repro::Meta, ReproductionPackage)> {
    candidate_roots(config_path).find_map(|root| {
        let meta = repro::resolve(&root, reference)?;
        let path = repro::repro_dir(&root, &meta.id).join("package.json");
        let raw = std::fs::read_to_string(path).ok()?;
        let package: ReproductionPackage = serde_json::from_str(&raw).ok()?;
        package.validate().ok()?;
        package.plan.as_ref()?;
        Some((root, meta, package))
    })
}

fn candidate_roots(config_path: Option<&Path>) -> impl Iterator<Item = PathBuf> {
    let explicit = config_path.and_then(|path| {
        let parent = path.canonicalize().ok()?.parent()?.to_path_buf();
        Some(
            if parent.file_name().is_some_and(|name| name == ".reproit") {
                parent.parent()?.to_path_buf()
            } else {
                parent
            },
        )
    });
    let mut roots = Vec::new();
    if let Some(root) = explicit {
        roots.push(root);
    }
    if let Ok(mut directory) = std::env::current_dir() {
        loop {
            if !roots.contains(&directory) {
                roots.push(directory.clone());
            }
            if !directory.pop() {
                break;
            }
        }
    }
    roots.into_iter()
}

pub(crate) async fn execute(root: &Path, package: &ReproductionPackage) -> Result<PlanRun> {
    execute_with_mode(root, package, ExecutionMode::Authoritative, None).await
}

pub(crate) async fn execute_diagnostic(
    root: &Path,
    package: &ReproductionPackage,
    options: DebugLaunchOptions,
) -> Result<PlanRun> {
    execute_with_mode(root, package, ExecutionMode::Diagnostic, Some(options)).await
}

async fn execute_with_mode(
    root: &Path,
    package: &ReproductionPackage,
    mode: ExecutionMode,
    debug_options: Option<DebugLaunchOptions>,
) -> Result<PlanRun> {
    package
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid reproduction package: {error}"))?;
    let plan = package
        .plan
        .as_ref()
        .context("eligible package has no reproduction plan")?;
    let mut state = ExecutionState::new();
    state.start(ExecutionPhase::Validate).unwrap();
    let catalog = load_catalog(
        root,
        Some(&package.occurrence.occurrence_id),
        Some(&plan.id),
    )?;
    let providers = resolve_providers(root, plan, &package.assessment, &catalog)?;
    let cell = cell::CellSession::prepare(root, plan, &providers, &catalog, mode).await?;
    let mut host_debug = if mode == ExecutionMode::Diagnostic && cell.is_none() {
        Some(host_debug::HostDebugSession::prepare(
            root, plan, &providers,
        )?)
    } else {
        None
    };
    state
        .finish(
            ExecutionPhase::Validate,
            PhaseStatus::Passed,
            format!("{} trusted provider(s)", providers.len()),
        )
        .unwrap();

    let mut provider_runs = Vec::new();
    let mut seen: Vec<ProviderVerdict> = Vec::new();
    let mut different_failure_seen = false;
    let mut infrastructure_failure_seen = false;
    let mut diagnostic_receipt = None;

    for phase in ExecutionPhase::ORDER
        .into_iter()
        .filter(|phase| !matches!(phase, ExecutionPhase::Validate | ExecutionPhase::Cleanup))
    {
        state.start(phase).unwrap();
        let cell_acted = match &cell {
            Some(cell) => match cell.before_phase(phase).await {
                Ok(acted) => acted,
                Err(error) => {
                    provider_runs.push(infrastructure_run(phase, "execution-cell", error));
                    seen.push(ProviderVerdict::InfrastructureFailed);
                    infrastructure_failure_seen = true;
                    state
                        .fail_and_advance_to_cleanup(phase, "execution cell phase failed")
                        .unwrap();
                    break;
                }
            },
            None => false,
        };
        if phase == ExecutionPhase::Debug && mode == ExecutionMode::Diagnostic {
            let receipt = match prepare_debugger(
                root,
                cell.as_ref(),
                host_debug.as_mut(),
                &plan.occurrence_id,
                debug_options
                    .as_ref()
                    .expect("diagnostic options are present"),
            )
            .await
            {
                Ok(receipt) => receipt,
                Err(error) => {
                    provider_runs.push(infrastructure_run(phase, "debugger-attach", error));
                    seen.push(ProviderVerdict::InfrastructureFailed);
                    infrastructure_failure_seen = true;
                    state
                        .fail_and_advance_to_cleanup(phase, "debugger attachment failed")
                        .unwrap();
                    break;
                }
            };
            diagnostic_receipt = Some(receipt);
        }
        let phase_providers: Vec<_> = providers
            .iter()
            .filter(|(_, provider)| provider.phase == phase)
            .collect();
        let diagnostic_pause = phase == ExecutionPhase::Debug && mode == ExecutionMode::Diagnostic;
        if phase_providers.is_empty() && !cell_acted && !diagnostic_pause {
            state
                .finish(phase, PhaseStatus::Skipped, "no provider")
                .unwrap();
            continue;
        }

        let mut phase_failed = false;
        for (provider_id, provider) in phase_providers {
            let execution = if host_debug
                .as_ref()
                .is_some_and(|session| session.owns(provider_id))
            {
                host_debug
                    .as_mut()
                    .expect("host debug ownership was checked")
                    .finish_trigger(&plan.observation.identity)
                    .await
            } else {
                execute_provider(root, provider_id, provider, &plan.observation.identity).await
            };
            let (run, verdict) = match execution {
                Ok(execution) => execution,
                Err(error) => {
                    provider_runs.push(ProviderRun {
                        provider_id: provider_id.to_string(),
                        phase: provider.phase,
                        exit_code: None,
                        signal: None,
                        timed_out: false,
                        output_truncated: false,
                        observation_matched: false,
                        expected_state_fingerprint: None,
                        actual_state_fingerprint: None,
                        state_verified: None,
                        error: Some(format!("{error:#}")),
                    });
                    seen.push(ProviderVerdict::InfrastructureFailed);
                    infrastructure_failure_seen = true;
                    phase_failed = true;
                    break;
                }
            };
            provider_runs.push(run);
            seen.push(verdict);
            match verdict {
                ProviderVerdict::SetupPassed
                | ProviderVerdict::Reproduced
                | ProviderVerdict::NotReproduced => {}
                ProviderVerdict::DifferentFailure => {
                    different_failure_seen = true;
                    phase_failed = true;
                }
                ProviderVerdict::InfrastructureFailed => {
                    infrastructure_failure_seen = true;
                    phase_failed = true;
                }
            }
        }
        if phase_failed {
            state
                .fail_and_advance_to_cleanup(
                    phase,
                    format!("{} provider run(s)", provider_runs.len()),
                )
                .unwrap();
            break;
        }
        state
            .finish(
                phase,
                PhaseStatus::Passed,
                format!("{} provider run(s)", provider_runs.len()),
            )
            .unwrap();
    }
    state
        .skip_until(ExecutionPhase::Cleanup, "no provider")
        .unwrap();
    state.start(ExecutionPhase::Cleanup).unwrap();
    let state_fingerprints = verified_state_fingerprints(&provider_runs);
    let host_cleanup = match &mut host_debug {
        Some(session) => Some(session.cleanup(state_fingerprints.clone()).await),
        None => None,
    };
    let cleanup_failures = run_cleanup(root, &providers, &mut provider_runs).await;
    let (cell_receipt, cell_cleanup_error) = match &cell {
        Some(cell) => {
            let (receipt, error) = cell.cleanup(state_fingerprints).await;
            (Some(receipt), error)
        }
        None => match host_cleanup {
            Some(Ok(receipt)) => (Some(receipt), None),
            Some(Err(error)) => (None, Some(error.context("cleaning up host debug executor"))),
            None => (None, None),
        },
    };
    let total_cleanup_failures = cleanup_failures + usize::from(cell_cleanup_error.is_some());
    if let Some(error) = cell_cleanup_error {
        infrastructure_failure_seen = true;
        seen.push(ProviderVerdict::InfrastructureFailed);
        provider_runs.push(infrastructure_run(
            ExecutionPhase::Cleanup,
            "execution-cell-cleanup",
            error,
        ));
    }
    if total_cleanup_failures == 0 {
        state
            .finish(
                ExecutionPhase::Cleanup,
                PhaseStatus::Passed,
                "owned cleanup commands exited cleanly",
            )
            .unwrap();
    } else {
        infrastructure_failure_seen = true;
        seen.push(ProviderVerdict::InfrastructureFailed);
        state
            .finish(
                ExecutionPhase::Cleanup,
                PhaseStatus::Failed,
                format!("{total_cleanup_failures} cleanup operation(s) failed"),
            )
            .unwrap();
    }
    debug_assert_eq!(
        state.failed(),
        infrastructure_failure_seen || different_failure_seen
    );

    let verdict = if mode == ExecutionMode::Diagnostic {
        ExecutionVerdict::Incomplete
    } else {
        fold_provider_verdicts(&seen)
    };
    if let Some(receipt) = &diagnostic_receipt {
        debug_control::finish(root, receipt, total_cleanup_failures == 0)?;
    }
    Ok(PlanRun {
        plan_id: plan.id.clone(),
        occurrence_id: plan.occurrence_id.clone(),
        verdict,
        phases: state.records().to_vec(),
        provider_runs,
        cell_receipt,
        diagnostic_receipt,
        authoritative: mode == ExecutionMode::Authoritative,
    })
}

async fn prepare_debugger(
    root: &Path,
    cell: Option<&cell::CellSession>,
    host: Option<&mut host_debug::HostDebugSession>,
    occurrence_id: &str,
    options: &DebugLaunchOptions,
) -> Result<reproit_protocol::DiagnosticReceipt> {
    let receipt = match (cell, host) {
        (Some(cell), None) => cell
            .debug_receipt(occurrence_id)
            .await?
            .context("diagnostic execution did not produce a debugger receipt")?,
        (None, Some(host)) => host.start(occurrence_id).await?,
        _ => anyhow::bail!("diagnostic execution resolved an ambiguous debug executor"),
    };
    let control = debug_control::DebugControl::start(root, &receipt).await?;
    debug_control::open_ide(root, &control, &options.ide, options.open).await?;
    match control.wait_for_trigger().await? {
        debug_control::DebugDecision::ReplayTrigger => {}
        debug_control::DebugDecision::Stop => {
            anyhow::bail!("diagnostic session stopped before the recorded trigger")
        }
    }
    Ok(receipt)
}

fn infrastructure_run(
    phase: ExecutionPhase,
    provider_id: &str,
    error: anyhow::Error,
) -> ProviderRun {
    ProviderRun {
        provider_id: provider_id.to_string(),
        phase,
        exit_code: None,
        signal: None,
        timed_out: false,
        output_truncated: false,
        observation_matched: false,
        expected_state_fingerprint: None,
        actual_state_fingerprint: None,
        state_verified: None,
        error: Some(format!("{error:#}")),
    }
}

fn verified_state_fingerprints(runs: &[ProviderRun]) -> BTreeMap<String, String> {
    runs.iter()
        .filter(|run| run.state_verified == Some(true))
        .filter_map(|run| {
            run.actual_state_fingerprint
                .as_deref()
                .and_then(|digest| digest.strip_prefix("sha256:"))
                .map(|digest| (run.provider_id.clone(), digest.to_string()))
        })
        .collect()
}

/// Fold the per-provider verdicts of one plan into the single `ExecutionVerdict`
/// the rest of the CLI reasons about.
///
/// The precedence is severity, not order: anything that means "this run is not
/// evidence about the bug" outranks anything that is. Infrastructure failure
/// first (we never even got to observe), then a DIFFERENT failure (we observed,
/// but not this bug), then a reproduction, then a clean run. A plan where no
/// provider observed anything at all stays `Incomplete`, which fails closed.
pub(crate) fn fold_provider_verdicts(seen: &[ProviderVerdict]) -> ExecutionVerdict {
    if seen.contains(&ProviderVerdict::InfrastructureFailed) {
        ExecutionVerdict::InfrastructureFailed
    } else if seen.contains(&ProviderVerdict::DifferentFailure) {
        ExecutionVerdict::DifferentFailure
    } else if seen.contains(&ProviderVerdict::Reproduced) {
        ExecutionVerdict::Reproduced
    } else if seen.contains(&ProviderVerdict::NotReproduced) {
        ExecutionVerdict::NotReproduced
    } else {
        ExecutionVerdict::Incomplete
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LocalCommandObservation {
    ExitCode(i32),
    #[cfg(unix)]
    Signal(i32),
    Timeout,
}

pub(crate) struct LocalCommandPlan<'a> {
    pub(crate) root: &'a Path,
    pub(crate) occurrence: reproit_protocol::OccurrenceEnvelope,
    pub(crate) assessment: CapabilityAssessment,
    pub(crate) argv: Vec<String>,
    pub(crate) working_directory: &'a Path,
    pub(crate) timeout_ms: u64,
    pub(crate) identity: &'a str,
    pub(crate) observation: LocalCommandObservation,
}

pub(crate) struct CompiledLocalCommandPackage {
    pub(crate) package: ReproductionPackage,
    catalog: ProviderCatalog,
}

impl CompiledLocalCommandPackage {
    pub(crate) fn install_provider(&self, root: &Path) -> Result<()> {
        persist_local_catalog(root, &self.package.occurrence.occurrence_id, &self.catalog)
    }
}

pub(crate) fn compile_local_command_package(
    local: LocalCommandPlan<'_>,
) -> Result<CompiledLocalCommandPackage> {
    let LocalCommandPlan {
        root,
        occurrence,
        assessment,
        argv,
        working_directory,
        timeout_ms,
        identity,
        observation,
    } = local;
    occurrence
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid local occurrence: {error}"))?;
    assessment
        .validate(&occurrence)
        .map_err(|error| anyhow::anyhow!("invalid local assessment: {error}"))?;
    if assessment.status != AssessmentStatus::Eligible {
        anyhow::bail!("local command capture is not eligible for reproduction");
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving checkout root {}", root.display()))?;
    let working_directory = working_directory.canonicalize().with_context(|| {
        format!(
            "resolving working directory {}",
            working_directory.display()
        )
    })?;
    let relative_working_directory = working_directory
        .strip_prefix(&root)
        .context("captured command working directory is outside the checkout")?;
    let provider_id = format!("local-{}", occurrence.occurrence_id);
    let matcher = match observation {
        LocalCommandObservation::ExitCode(code) => ObservationMatcher::ExitCode { code },
        #[cfg(unix)]
        LocalCommandObservation::Signal(number) => ObservationMatcher::Signal { number },
        LocalCommandObservation::Timeout => ObservationMatcher::Timeout,
    };
    let provider = CommandProvider {
        authority: MechanismAuthority::ExplicitLocalApproval,
        phase: ExecutionPhase::Trigger,
        capabilities: BTreeSet::new(),
        source: captured_provider_source(&root, &argv),
        cell: None,
        debug: None,
        argv,
        environment: BTreeMap::new(),
        working_directory: (!relative_working_directory.as_os_str().is_empty())
            .then(|| relative_working_directory.to_path_buf()),
        timeout_ms,
        clean_exit_codes: vec![0],
        observation: Some(CommandObservation {
            identity: identity.to_string(),
            matcher,
        }),
        state_fingerprint: None,
        cleanup: None,
    };
    validate_provider_id(&provider_id)?;
    validate_command(
        &root,
        &provider.argv,
        &provider.environment,
        provider.working_directory.as_deref(),
        provider.timeout_ms,
    )?;
    let mut bindings = Vec::new();
    for requirement in assessment
        .requirements
        .iter()
        .filter(|requirement| requirement.level == RequirementLevel::Required)
    {
        if requirement_phase(requirement) != ExecutionPhase::Trigger {
            anyhow::bail!(
                "local command provider cannot satisfy non-trigger requirement `{}`",
                requirement.id
            );
        }
        bindings.push(PlanBinding {
            requirement_id: requirement.id.clone(),
            provider_id: provider_id.clone(),
            mechanism_authority: provider.authority,
            template_digest: provider_digest(&provider)?,
            evidence_artifact_ids: requirement.evidence_artifact_ids.clone(),
        });
    }
    let observation_kind = occurrence
        .observations
        .first()
        .map(|observation| observation.kind)
        .context("local occurrence has no failure observation")?;
    let mut plan = ReproductionPlan {
        version: PLAN_VERSION,
        id: String::new(),
        occurrence_id: occurrence.occurrence_id.clone(),
        target: "current-checkout".into(),
        destination: ExecutionDestination::LocalProcess,
        bindings,
        observation: ObservationTarget {
            observation: observation_kind,
            identity: identity.to_string(),
            authority: ObservationAuthority::RuntimeDiagnosis,
        },
    };
    plan.finalize_id()
        .map_err(|error| anyhow::anyhow!("invalid local plan: {error}"))?;
    let mut package = ReproductionPackage {
        version: reproit_protocol::PACKAGE_VERSION,
        id: String::new(),
        occurrence,
        assessment,
        plan: Some(plan),
        capsule: None,
        legacy: None,
    };
    package
        .finalize_id()
        .map_err(|error| anyhow::anyhow!("invalid local package: {error}"))?;
    package
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid local package: {error}"))?;
    let catalog = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: BTreeMap::new(),
        providers: BTreeMap::from([(provider_id, provider)]),
    };
    validate_catalog(&root, &catalog)?;
    Ok(CompiledLocalCommandPackage { package, catalog })
}

pub(crate) fn compile_automatic_package(
    root: &Path,
    occurrence: reproit_protocol::OccurrenceEnvelope,
    assessment: CapabilityAssessment,
) -> Result<ReproductionPackage> {
    occurrence
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid Cloud occurrence: {error}"))?;
    assessment
        .validate(&occurrence)
        .map_err(|error| anyhow::anyhow!("invalid Cloud assessment: {error}"))?;
    if assessment.status != AssessmentStatus::Eligible {
        let missing = assessment
            .unresolved
            .iter()
            .map(|item| format!("{}: {}", item.requirement_id, item.detail))
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!(
            "occurrence is {:?}, not locally eligible: {missing}",
            assessment.status
        );
    }
    let identities: BTreeSet<_> = occurrence
        .observations
        .iter()
        .filter_map(|observation| observation.signature.as_deref())
        .collect();
    if identities.len() != 1 {
        anyhow::bail!("Cloud occurrence must have exactly one exact failure signature");
    }
    let identity = identities.into_iter().next().unwrap();
    let observation_kind = occurrence
        .observations
        .first()
        .map(|observation| observation.kind)
        .context("Cloud occurrence has no failure observation")?;
    let catalog = load_catalog(root, None, None)?;
    let mut bindings = Vec::new();
    for requirement in assessment
        .requirements
        .iter()
        .filter(|requirement| requirement.level == RequirementLevel::Required)
    {
        let phase = requirement_phase(requirement);
        let candidates = catalog
            .providers
            .iter()
            .filter(|(_, provider)| provider.phase == phase)
            .filter(|(_, provider)| {
                provider
                    .observation
                    .as_ref()
                    .is_none_or(|observation| observation.identity == identity)
            })
            .collect::<Vec<_>>();
        let [(provider_id, provider)] = candidates.as_slice() else {
            let names = candidates
                .iter()
                .map(|(provider_id, _)| provider_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "requirement `{}` needs one trusted {:?} provider; candidates: [{}]",
                requirement.id,
                phase,
                names
            );
        };
        bindings.push(PlanBinding {
            requirement_id: requirement.id.clone(),
            provider_id: (*provider_id).clone(),
            mechanism_authority: provider.authority,
            template_digest: provider_binding_digest(root, &catalog, provider)?,
            evidence_artifact_ids: requirement.evidence_artifact_ids.clone(),
        });
    }
    if !bindings.iter().any(|binding| {
        catalog
            .providers
            .get(&binding.provider_id)
            .and_then(|provider| provider.observation.as_ref())
            .is_some_and(|observation| observation.identity == identity)
    }) {
        anyhow::bail!("no automatically selected provider observes exact identity `{identity}`");
    }
    let mut plan = ReproductionPlan {
        version: PLAN_VERSION,
        id: String::new(),
        occurrence_id: occurrence.occurrence_id.clone(),
        target: "current-checkout".into(),
        destination: destination_for_bindings(&catalog, &bindings)?,
        bindings,
        observation: ObservationTarget {
            observation: observation_kind,
            identity: identity.to_string(),
            authority: ObservationAuthority::RuntimeDiagnosis,
        },
    };
    plan.finalize_id()
        .map_err(|error| anyhow::anyhow!("invalid automatic plan: {error}"))?;
    let mut package = ReproductionPackage {
        version: reproit_protocol::PACKAGE_VERSION,
        id: String::new(),
        occurrence,
        assessment,
        plan: Some(plan),
        capsule: None,
        legacy: None,
    };
    package
        .finalize_id()
        .map_err(|error| anyhow::anyhow!("invalid automatic package: {error}"))?;
    package
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid automatic package: {error}"))?;
    Ok(package)
}

pub(crate) fn compile_package(
    root: &Path,
    package: &ReproductionPackage,
    requested_bindings: &BTreeMap<String, String>,
    identity: &str,
) -> Result<ReproductionPackage> {
    package
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid imported package: {error}"))?;
    validate_text(identity, "observation identity")?;
    if let Some(source_identity) = package
        .occurrence
        .observations
        .iter()
        .find_map(|observation| observation.signature.as_deref())
    {
        if source_identity != identity {
            anyhow::bail!(
                "the requested identity `{identity}` does not match source identity \
                 `{source_identity}`"
            );
        }
    }
    let catalog = load_catalog(root, None, None)?;
    let mut bindings = Vec::new();
    for requirement in package
        .assessment
        .requirements
        .iter()
        .filter(|requirement| requirement.level == RequirementLevel::Required)
    {
        let provider_id = requested_bindings
            .get(&requirement.id)
            .with_context(|| format!("missing --bind {}=PROVIDER", requirement.id))?;
        let provider = catalog.providers.get(provider_id).with_context(|| {
            format!(
                "requirement `{}` names unknown provider `{provider_id}`",
                requirement.id
            )
        })?;
        if provider.phase != requirement_phase(requirement) {
            anyhow::bail!(
                "provider `{provider_id}` phase {:?} cannot satisfy requirement `{}` phase {:?}",
                provider.phase,
                requirement.id,
                requirement_phase(requirement)
            );
        }
        if let Some(capability) = required_trusted_capability(requirement) {
            if !provider.capabilities.contains(&capability) {
                anyhow::bail!(
                    "provider `{provider_id}` does not declare trusted capability \
                     `{:?}` for requirement `{}`",
                    capability,
                    requirement.id
                );
            }
        }
        if let Some(observation) = &provider.observation {
            if observation.identity != identity {
                anyhow::bail!(
                    "provider `{provider_id}` observes `{}`, not `{identity}`",
                    observation.identity
                );
            }
        }
        bindings.push(PlanBinding {
            requirement_id: requirement.id.clone(),
            provider_id: provider_id.clone(),
            mechanism_authority: provider.authority,
            template_digest: provider_binding_digest(root, &catalog, provider)?,
            evidence_artifact_ids: requirement.evidence_artifact_ids.clone(),
        });
    }
    if requested_bindings.len() != bindings.len() {
        anyhow::bail!("every --bind must name one required assessed requirement");
    }
    if !bindings.iter().any(|binding| {
        catalog
            .providers
            .get(&binding.provider_id)
            .and_then(|provider| provider.observation.as_ref())
            .is_some()
    }) {
        anyhow::bail!("at least one bound provider must define the exact observation");
    }

    let mut assessment = package.assessment.clone();
    assessment.status = AssessmentStatus::Eligible;
    assessment.unresolved.clear();
    let observation_kind = package
        .occurrence
        .observations
        .first()
        .map(|observation| observation.kind)
        .context("occurrence has no observation")?;
    let mut plan = ReproductionPlan {
        version: PLAN_VERSION,
        id: String::new(),
        occurrence_id: package.occurrence.occurrence_id.clone(),
        target: "current-checkout".into(),
        destination: destination_for_bindings(&catalog, &bindings)?,
        bindings,
        observation: ObservationTarget {
            observation: observation_kind,
            identity: identity.to_string(),
            authority: ObservationAuthority::AuthoredContract,
        },
    };
    plan.finalize_id()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut compiled = package.clone();
    compiled.assessment = assessment;
    compiled.plan = Some(plan);
    compiled
        .finalize_id()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    compiled
        .validate()
        .map_err(|error| anyhow::anyhow!("compiled package is invalid: {error}"))?;
    Ok(compiled)
}

fn destination_for_bindings(
    catalog: &ProviderCatalog,
    bindings: &[PlanBinding],
) -> Result<ExecutionDestination> {
    let mut cells = BTreeSet::new();
    let mut host_bound = false;
    for binding in bindings {
        let provider = catalog
            .providers
            .get(&binding.provider_id)
            .context("compiled binding lost its trusted provider")?;
        match &provider.cell {
            Some(cell) => {
                cells.insert(cell.as_str());
            }
            None => host_bound = true,
        }
    }
    if cells.len() > 1 || (!cells.is_empty() && host_bound) {
        anyhow::bail!("all required providers must use the same execution cell");
    }
    Ok(if cells.is_empty() {
        ExecutionDestination::LocalProcess
    } else {
        ExecutionDestination::LocalCompose
    })
}

#[cfg(test)]
mod tests;
