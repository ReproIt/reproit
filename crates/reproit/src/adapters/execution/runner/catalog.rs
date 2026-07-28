//! Trusted provider catalog: load, merge, persist, and validate the committed
//! and machine-local provider records a reproduction plan may execute.
//!
//! Split out of `runner.rs` so both files stay inside the workspace
//! reviewability bound. Items are `pub(super)` unless the wider crate already
//! depended on them.

use super::*;

pub(super) fn load_catalog(
    root: &Path,
    occurrence_id: Option<&str>,
    plan_id: Option<&str>,
) -> Result<ProviderCatalog> {
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
            merge_catalog(&mut catalog, local, "local execution provider")?;
        }
    }
    if let Some(plan_id) = plan_id {
        let committed_path = committed_catalog_path(root, plan_id);
        if committed_path.exists() {
            let committed = read_catalog(&committed_path)?;
            merge_catalog(&mut catalog, committed, "committed guard provider")?;
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

pub(super) fn merge_catalog(
    destination: &mut ProviderCatalog,
    source: ProviderCatalog,
    source_label: &str,
) -> Result<()> {
    for (provider_id, provider) in source.providers {
        if let Some(existing) = destination.providers.get(&provider_id) {
            if provider_digest(existing)? == provider_digest(&provider)? {
                continue;
            }
            anyhow::bail!(
                "{source_label} `{provider_id}` conflicts with another execution provider"
            );
        }
        destination.providers.insert(provider_id, provider);
    }
    Ok(())
}

pub(super) fn read_project_catalog(path: &Path) -> Result<Option<ProviderCatalog>> {
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

pub(super) fn read_catalog(path: &Path) -> Result<ProviderCatalog> {
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

pub(super) fn local_catalog_path(root: &Path, occurrence_id: &str) -> PathBuf {
    root.join(".reproit")
        .join("private-providers")
        .join(format!("{occurrence_id}.yaml"))
}

pub(super) fn committed_catalog_path(root: &Path, plan_id: &str) -> PathBuf {
    let repro_id = repro::repro_id(0, &[format!("plan:{plan_id}")]);
    repro::repro_dir(root, &repro_id).join("providers.yaml")
}

pub(super) fn validate_occurrence_id(occurrence_id: &str) -> Result<()> {
    if !occurrence_id.starts_with("occ_") {
        anyhow::bail!("invalid occurrence id `{occurrence_id}`");
    }
    validate_provider_id(occurrence_id)
}

pub(super) fn persist_local_catalog(
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

pub(super) fn validate_catalog(root: &Path, catalog: &ProviderCatalog) -> Result<()> {
    if catalog.version != CATALOG_VERSION {
        anyhow::bail!("unsupported execution provider catalog version");
    }
    if catalog.providers.is_empty() || catalog.providers.len() > MAX_PROVIDERS {
        anyhow::bail!("execution provider catalog must contain 1..={MAX_PROVIDERS} providers");
    }
    for (provider_id, provider) in &catalog.providers {
        validate_provider_id(provider_id)?;
        if let Some(source) = &provider.source {
            validate_provider_source(root, source)?;
        }
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

pub(super) fn validate_provider_source(root: &Path, source: &ProviderSource) -> Result<()> {
    if source.path.is_absolute()
        || source
            .path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        anyhow::bail!("provider source path must be a normalized checkout-relative path");
    }
    if !source.sha256.starts_with("sha256:")
        || source.sha256.len() != "sha256:".len() + 64
        || !source.sha256["sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        anyhow::bail!("provider source sha256 is invalid");
    }
    let declared_path = root.join(&source.path);
    let path = declared_path
        .canonicalize()
        .with_context(|| format!("resolving provider source {}", declared_path.display()))?;
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("resolving checkout root {}", root.display()))?;
    if !path.starts_with(&canonical_root) {
        anyhow::bail!("provider source path escapes the checkout");
    }
    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("reading provider source {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("provider source is not a regular file: {}", path.display());
    }
    let actual = sha256_path(&path)?;
    if actual != source.sha256 {
        anyhow::bail!(
            "provider source {} changed: expected {}, got {}",
            source.path.display(),
            source.sha256,
            actual
        );
    }
    Ok(())
}

pub(super) fn validate_provider_id(provider_id: &str) -> Result<()> {
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

pub(super) fn validate_command(
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

pub(super) fn validate_text(value: &str, field: &str) -> Result<()> {
    if value.is_empty() || value.len() > 16 * 1024 || value.contains('\0') {
        anyhow::bail!("invalid {field}");
    }
    Ok(())
}

pub(super) fn resolve_working_directory(root: &Path, configured: Option<&Path>) -> Result<PathBuf> {
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

pub(super) fn provider_digest(provider: &CommandProvider) -> Result<String> {
    let bytes = serde_json::to_vec(provider).context("serializing trusted provider")?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    Ok(format!("sha256:{encoded}"))
}

pub(super) fn sha256_path(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    Ok(format!("sha256:{encoded}"))
}

pub(super) fn captured_provider_source(root: &Path, argv: &[String]) -> Option<ProviderSource> {
    let canonical_root = root.canonicalize().ok()?;
    let executable = Path::new(argv.first()?);
    let executable_name = executable.file_name()?.to_str()?;
    let interpreted =
        ["node", "nodejs", "python", "python3", "bash", "sh"].contains(&executable_name);
    let declared = if interpreted {
        Path::new(argv.get(1)?)
    } else {
        executable
    };
    if declared.as_os_str().is_empty() || declared.starts_with("-") {
        return None;
    }
    let candidate = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        canonical_root.join(declared)
    };
    let canonical = candidate.canonicalize().ok()?;
    let relative = canonical.strip_prefix(canonical_root).ok()?.to_path_buf();
    Some(ProviderSource {
        path: relative,
        sha256: sha256_path(&canonical).ok()?,
    })
}

pub(crate) fn persist_plan_catalog(
    root: &Path,
    package: &ReproductionPackage,
    repro_directory: &Path,
) -> Result<()> {
    let plan = package
        .plan
        .as_ref()
        .context("cannot persist providers for a package without a plan")?;
    let catalog = load_catalog(root, Some(&package.occurrence.occurrence_id), None)?;
    let providers = resolve_providers(plan, &package.assessment, &catalog)?
        .into_iter()
        .map(|(provider_id, provider)| (provider_id, provider.clone()))
        .collect();
    let committed = ProviderCatalog {
        version: CATALOG_VERSION,
        providers,
    };
    validate_catalog(root, &committed)?;
    let encoded =
        serde_yaml::to_string(&committed).context("serializing committed execution providers")?;
    let path = repro_directory.join("providers.yaml");
    if path.exists() {
        let existing = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if existing == encoded {
            return Ok(());
        }
        anyhow::bail!("committed execution providers changed for {}", plan.id);
    }
    let temporary = repro_directory.join(format!(".providers.{}.tmp", std::process::id()));
    std::fs::write(&temporary, encoded)
        .with_context(|| format!("writing {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("installing {}", path.display()))
}

pub(super) fn resolve_providers<'a>(
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

pub(super) fn requirement_phase(requirement: &ReproductionRequirement) -> ExecutionPhase {
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
