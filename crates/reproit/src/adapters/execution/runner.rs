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
pub(crate) mod model;
mod process;
pub(crate) use automatic::{
    compile_package_automatically, AutomaticCompilation, CompilationBlocker,
};
use catalog::*;
pub(crate) use catalog::{
    persist_plan_catalog, pinned_provider_digest, repin_guard_providers, repin_package_mechanism,
    source_digest,
};
pub(crate) use model::PlanRun;
use model::*;
use process::*;

const CATALOG_VERSION: u16 = 1;
const MAX_PROVIDERS: usize = 256;
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
    let providers = resolve_providers(plan, &package.assessment, &catalog)?;
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

    for phase in ExecutionPhase::ORDER
        .into_iter()
        .filter(|phase| !matches!(phase, ExecutionPhase::Validate | ExecutionPhase::Cleanup))
    {
        state.start(phase).unwrap();
        let phase_providers: Vec<_> = providers
            .iter()
            .filter(|(_, provider)| provider.phase == phase)
            .collect();
        if phase_providers.is_empty() {
            state
                .finish(phase, PhaseStatus::Skipped, "no provider")
                .unwrap();
            continue;
        }

        let mut phase_failed = false;
        for (provider_id, provider) in phase_providers {
            let execution =
                execute_provider(root, provider_id, provider, &plan.observation.identity).await;
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
    run_cleanup(root, &providers, &mut provider_runs).await;
    state
        .finish(
            ExecutionPhase::Cleanup,
            PhaseStatus::Passed,
            "owned cleanup attempted",
        )
        .unwrap();
    debug_assert_eq!(
        state.failed(),
        infrastructure_failure_seen || different_failure_seen
    );

    let verdict = fold_provider_verdicts(&seen);
    Ok(PlanRun {
        plan_id: plan.id.clone(),
        occurrence_id: plan.occurrence_id.clone(),
        verdict,
        phases: state.records().to_vec(),
        provider_runs,
    })
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
            template_digest: provider_digest(provider)?,
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
        destination: ExecutionDestination::LocalProcess,
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
            template_digest: provider_digest(provider)?,
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
        destination: ExecutionDestination::LocalProcess,
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

#[cfg(test)]
mod tests;
