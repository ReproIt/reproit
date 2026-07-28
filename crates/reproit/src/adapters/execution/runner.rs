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

mod model;
mod process;
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
    let catalog = load_catalog(root, Some(&package.occurrence.occurrence_id))?;
    let providers = resolve_providers(plan, &package.assessment, &catalog)?;
    state
        .finish(
            ExecutionPhase::Validate,
            PhaseStatus::Passed,
            format!("{} trusted provider(s)", providers.len()),
        )
        .unwrap();

    let mut provider_runs = Vec::new();
    let mut exact_observation_seen = false;
    let mut clean_observation_seen = false;
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
                    infrastructure_failure_seen = true;
                    phase_failed = true;
                    break;
                }
            };
            provider_runs.push(run);
            match verdict {
                ProviderVerdict::SetupPassed => {}
                ProviderVerdict::Reproduced => exact_observation_seen = true,
                ProviderVerdict::NotReproduced => clean_observation_seen = true,
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

    let verdict = if infrastructure_failure_seen {
        ExecutionVerdict::InfrastructureFailed
    } else if different_failure_seen {
        ExecutionVerdict::DifferentFailure
    } else if exact_observation_seen {
        ExecutionVerdict::Reproduced
    } else if clean_observation_seen {
        ExecutionVerdict::NotReproduced
    } else {
        ExecutionVerdict::Incomplete
    };
    Ok(PlanRun {
        plan_id: plan.id.clone(),
        occurrence_id: plan.occurrence_id.clone(),
        verdict,
        phases: state.records().to_vec(),
        provider_runs,
    })
}

fn load_catalog(root: &Path, occurrence_id: Option<&str>) -> Result<ProviderCatalog> {
    let compatibility_path = root.join("reproit.execution.yaml");
    let project_path = root.join("reproit.yaml");
    let project_catalog = read_project_catalog(&project_path)?;
    if project_catalog.is_some() && compatibility_path.exists() {
        anyhow::bail!(
            "execution providers are defined in both reproit.yaml and \
             reproit.execution.yaml; keep only reproit.yaml:execution"
        );
    }
    let mut catalog = if let Some(catalog) = project_catalog {
        catalog
    } else if compatibility_path.exists() {
        read_catalog(&compatibility_path)?
    } else {
        ProviderCatalog {
            version: CATALOG_VERSION,
            providers: BTreeMap::new(),
        }
    };
    if let Some(occurrence_id) = occurrence_id {
        validate_occurrence_id(occurrence_id)?;
        let local_path = local_catalog_path(root, occurrence_id);
        if local_path.exists() {
            let local = read_catalog(&local_path)?;
            for (provider_id, provider) in local.providers {
                if catalog
                    .providers
                    .insert(provider_id.clone(), provider)
                    .is_some()
                {
                    anyhow::bail!(
                        "local execution provider `{provider_id}` conflicts with \
                         checkout execution configuration"
                    );
                }
            }
        }
    }
    if catalog.providers.is_empty() {
        anyhow::bail!(
            "no trusted execution providers found; add execution.providers to reproit.yaml"
        );
    }
    validate_catalog(root, &catalog)?;
    Ok(catalog)
}

fn read_project_catalog(path: &Path) -> Result<Option<ProviderCatalog>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if raw.len() > 1024 * 1024 {
        anyhow::bail!("{} exceeds the 1 MiB config limit", path.display());
    }
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    crate::adapters::config::interpolate_value(&mut value)?;
    let Some(execution) = value
        .as_mapping()
        .and_then(|mapping| mapping.get(serde_yaml::Value::String("execution".into())))
    else {
        return Ok(None);
    };
    serde_yaml::from_value(execution.clone())
        .with_context(|| format!("parsing {}:execution", path.display()))
        .map(Some)
}

fn read_catalog(path: &Path) -> Result<ProviderCatalog> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if raw.len() > 1024 * 1024 {
        anyhow::bail!(
            "{} exceeds the 1 MiB provider-catalog limit",
            path.display()
        );
    }
    serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

fn local_catalog_path(root: &Path, occurrence_id: &str) -> PathBuf {
    root.join(".reproit")
        .join("private-providers")
        .join(format!("{occurrence_id}.yaml"))
}

fn validate_occurrence_id(occurrence_id: &str) -> Result<()> {
    if !occurrence_id.starts_with("occ_") {
        anyhow::bail!("invalid occurrence id `{occurrence_id}`");
    }
    validate_provider_id(occurrence_id)
}

fn persist_local_catalog(
    root: &Path,
    occurrence_id: &str,
    catalog: &ProviderCatalog,
) -> Result<()> {
    validate_catalog(root, catalog)?;
    let path = local_catalog_path(root, occurrence_id);
    let parent = path
        .parent()
        .context("local provider path has no parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let encoded = serde_yaml::to_string(catalog).context("serializing local execution provider")?;
    if path.exists() {
        let existing = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if existing == encoded {
            return Ok(());
        }
        anyhow::bail!(
            "local provider receipt {} already exists with different contents",
            path.display()
        );
    }
    let temporary = parent.join(format!(".{occurrence_id}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    file.write_all(encoded.as_bytes())
        .with_context(|| format!("writing {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("installing {}", path.display()))?;
    Ok(())
}

fn validate_catalog(root: &Path, catalog: &ProviderCatalog) -> Result<()> {
    if catalog.version != CATALOG_VERSION {
        anyhow::bail!("unsupported execution provider catalog version");
    }
    if catalog.providers.is_empty() || catalog.providers.len() > MAX_PROVIDERS {
        anyhow::bail!("execution provider catalog must contain 1..={MAX_PROVIDERS} providers");
    }
    for (provider_id, provider) in &catalog.providers {
        validate_provider_id(provider_id)?;
        validate_command(
            root,
            &provider.argv,
            &provider.environment,
            provider.working_directory.as_deref(),
            provider.timeout_ms,
        )?;
        if provider.clean_exit_codes.is_empty() || provider.clean_exit_codes.len() > 16 {
            anyhow::bail!("provider `{provider_id}` has invalid cleanExitCodes");
        }
        if let Some(observation) = &provider.observation {
            validate_text(&observation.identity, "observation identity")?;
            match &observation.matcher {
                ObservationMatcher::StdoutContains { value }
                | ObservationMatcher::StderrContains { value } => {
                    validate_text(value, "observation marker")?
                }
                ObservationMatcher::ExitCode { .. } | ObservationMatcher::Timeout => {}
                ObservationMatcher::Signal { number } if *number > 0 => {}
                ObservationMatcher::Signal { .. } => {
                    anyhow::bail!("provider signal number must be positive")
                }
            }
        }
        if let Some(cleanup) = &provider.cleanup {
            validate_command(
                root,
                &cleanup.argv,
                &cleanup.environment,
                cleanup.working_directory.as_deref(),
                cleanup.timeout_ms,
            )?;
        }
    }
    Ok(())
}

fn validate_provider_id(provider_id: &str) -> Result<()> {
    if provider_id.is_empty()
        || provider_id.len() > 128
        || !provider_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        anyhow::bail!("invalid execution provider id `{provider_id}`");
    }
    Ok(())
}

fn validate_command(
    root: &Path,
    argv: &[String],
    environment: &BTreeMap<String, String>,
    working_directory: Option<&Path>,
    timeout_ms: u64,
) -> Result<()> {
    if argv.is_empty() || argv.len() > MAX_COMMAND_ARGS {
        anyhow::bail!("provider argv must contain 1..={MAX_COMMAND_ARGS} entries");
    }
    for argument in argv {
        validate_text(argument, "command argument")?;
    }
    if environment.len() > MAX_ENVIRONMENT_ENTRIES {
        anyhow::bail!("provider environment exceeds {MAX_ENVIRONMENT_ENTRIES} entries");
    }
    for (name, value) in environment {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            anyhow::bail!("invalid provider environment name");
        }
        validate_text(value, "provider environment value")?;
    }
    if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
        anyhow::bail!("provider timeoutMs must be within 1..={MAX_TIMEOUT_MS}");
    }
    resolve_working_directory(root, working_directory)?;
    Ok(())
}

fn validate_text(value: &str, field: &str) -> Result<()> {
    if value.is_empty() || value.len() > 16 * 1024 || value.contains('\0') {
        anyhow::bail!("invalid {field}");
    }
    Ok(())
}

fn resolve_working_directory(root: &Path, configured: Option<&Path>) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving checkout root {}", root.display()))?;
    let candidate = configured.map_or_else(|| root.clone(), |path| root.join(path));
    let candidate = candidate
        .canonicalize()
        .with_context(|| format!("resolving provider directory {}", candidate.display()))?;
    if !candidate.starts_with(&root) {
        anyhow::bail!("provider working directory escapes the checkout");
    }
    Ok(candidate)
}

fn provider_digest(provider: &CommandProvider) -> Result<String> {
    let bytes = serde_json::to_vec(provider).context("serializing trusted provider")?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    Ok(format!("sha256:{encoded}"))
}

fn resolve_providers<'a>(
    plan: &reproit_protocol::ReproductionPlan,
    assessment: &CapabilityAssessment,
    catalog: &'a ProviderCatalog,
) -> Result<Vec<(String, &'a CommandProvider)>> {
    let mut seen = BTreeSet::new();
    let mut providers = Vec::new();
    let mut observation_provider = false;
    for binding in &plan.bindings {
        let provider = catalog
            .providers
            .get(&binding.provider_id)
            .with_context(|| {
                format!(
                    "plan binding `{}` names unknown trusted provider `{}`",
                    binding.requirement_id, binding.provider_id
                )
            })?;
        if provider.authority != binding.mechanism_authority {
            anyhow::bail!(
                "provider `{}` authority does not match the plan binding",
                binding.provider_id
            );
        }
        let requirement = assessment
            .requirements
            .iter()
            .find(|requirement| requirement.id == binding.requirement_id)
            .context("plan binding has no assessed requirement")?;
        if provider.phase != requirement_phase(requirement) {
            anyhow::bail!(
                "provider `{}` runs in {:?}, but requirement `{}` needs {:?}",
                binding.provider_id,
                provider.phase,
                requirement.id,
                requirement_phase(requirement)
            );
        }
        let digest = provider_digest(provider)?;
        if digest != binding.template_digest {
            anyhow::bail!(
                "provider `{}` changed since the plan was compiled: expected {}, got {}",
                binding.provider_id,
                binding.template_digest,
                digest
            );
        }
        if let Some(observation) = &provider.observation {
            if observation.identity != plan.observation.identity {
                anyhow::bail!(
                    "provider `{}` observes `{}`, not the plan identity `{}`",
                    binding.provider_id,
                    observation.identity,
                    plan.observation.identity
                );
            }
            observation_provider = true;
        }
        if seen.insert(binding.provider_id.as_str()) {
            providers.push((binding.provider_id.clone(), provider));
        }
    }
    if !observation_provider {
        anyhow::bail!(
            "no trusted provider observes the exact identity `{}`",
            plan.observation.identity
        );
    }
    providers.sort_by_key(|(_, provider)| provider.phase);
    Ok(providers)
}

fn requirement_phase(requirement: &ReproductionRequirement) -> ExecutionPhase {
    match &requirement.requirement {
        RequirementKind::Process { operation, .. } => match operation {
            ProcessOperation::Build => ExecutionPhase::Build,
            ProcessOperation::Launch => ExecutionPhase::Launch,
            ProcessOperation::Attach => ExecutionPhase::Debug,
            ProcessOperation::Stop => ExecutionPhase::Cleanup,
        },
        RequirementKind::Trigger { .. } => ExecutionPhase::Trigger,
        RequirementKind::State { .. } => ExecutionPhase::Seed,
        RequirementKind::Dependency { .. } => ExecutionPhase::Launch,
        RequirementKind::Environment { .. } => ExecutionPhase::Reset,
        RequirementKind::Observation { .. } => ExecutionPhase::Observe,
        RequirementKind::Debugger { .. } => ExecutionPhase::Debug,
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
    let identity = occurrence
        .observations
        .iter()
        .find_map(|observation| observation.signature.as_deref())
        .context("Cloud occurrence has no exact failure signature")?;
    let observation_kind = occurrence
        .observations
        .first()
        .map(|observation| observation.kind)
        .context("Cloud occurrence has no failure observation")?;
    let catalog = load_catalog(root, None)?;
    let mut bindings = Vec::new();
    for requirement in assessment
        .requirements
        .iter()
        .filter(|requirement| requirement.level == RequirementLevel::Required)
    {
        let phase = requirement_phase(requirement);
        let mut candidates = catalog
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
        if candidates.len() > 1 {
            let hint = requirement_hint(requirement);
            let matching = candidates
                .iter()
                .copied()
                .filter(|(provider_id, _)| provider_id.contains(&hint))
                .collect::<Vec<_>>();
            if matching.len() == 1 {
                candidates = matching;
            }
        }
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

fn requirement_hint(requirement: &ReproductionRequirement) -> String {
    let raw = match &requirement.requirement {
        RequirementKind::Process { role, .. } => role,
        RequirementKind::Trigger { subject, .. }
        | RequirementKind::State { subject, .. }
        | RequirementKind::Dependency { subject, .. }
        | RequirementKind::Observation { subject, .. } => subject,
        RequirementKind::Environment { .. } | RequirementKind::Debugger { .. } => {
            return String::new();
        }
    };
    raw.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
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
    let catalog = load_catalog(root, None)?;
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
