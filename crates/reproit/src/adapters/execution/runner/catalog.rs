//! Trusted provider catalog: load, merge, persist, and validate the committed
//! and machine-local provider records a reproduction plan may execute.
//!
//! Split out of `runner.rs` so both files stay inside the workspace
//! reviewability bound. Items are `pub(super)` unless the wider crate already
//! depended on them.

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderCatalogInspection {
    pub(crate) provider_count: usize,
    pub(crate) cell_count: usize,
    pub(crate) debug_executor_count: usize,
    pub(crate) phases: Vec<ExecutionPhase>,
    pub(crate) observation_count: usize,
    pub(crate) state_fingerprint_count: usize,
    pub(crate) source_pinned_count: usize,
    pub(crate) cleanup_count: usize,
}

/// Validate and summarize only the checkout-owned provider catalog.
///
/// Machine-local and kept-guard providers are occurrence-specific, so doctor
/// must not count them as general project readiness.
pub(crate) fn inspect_project_catalog(root: &Path) -> Result<Option<ProviderCatalogInspection>> {
    let Some(catalog) = read_project_catalog(&root.join("reproit.yaml"))? else {
        return Ok(None);
    };
    validate_catalog(root, &catalog)?;
    let phases = catalog
        .providers
        .values()
        .map(|provider| provider.phase)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(Some(ProviderCatalogInspection {
        provider_count: catalog.providers.len(),
        cell_count: catalog.cells.len(),
        debug_executor_count: catalog
            .cells
            .values()
            .filter(|cell| match cell {
                ReproductionCell::DockerCompose(cell) => cell.debug.is_some(),
            })
            .count()
            + catalog
                .providers
                .values()
                .filter(|provider| provider.debug.is_some())
                .count(),
        phases,
        observation_count: catalog
            .providers
            .values()
            .filter(|provider| provider.observation.is_some())
            .count(),
        state_fingerprint_count: catalog
            .providers
            .values()
            .filter(|provider| provider.state_fingerprint.is_some())
            .count(),
        source_pinned_count: catalog
            .providers
            .values()
            .filter(|provider| provider.source.is_some())
            .count(),
        cleanup_count: catalog
            .providers
            .values()
            .filter(|provider| provider.cleanup.is_some())
            .count(),
    }))
}

pub(super) fn load_catalog(
    root: &Path,
    occurrence_id: Option<&str>,
    plan_id: Option<&str>,
) -> Result<ProviderCatalog> {
    let project_path = root.join("reproit.yaml");
    let mut catalog = if let Some(catalog) = read_project_catalog(&project_path)? {
        catalog
    } else {
        ProviderCatalog {
            version: CATALOG_VERSION,
            cells: BTreeMap::new(),
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
    // A committed guard catalog is looked up by the failure it preserves, not
    // by the plan: the plan id moves whenever the mechanism is re-pinned.
    if let (Some(occurrence_id), Some(_)) = (occurrence_id, plan_id) {
        let committed_path = committed_catalog_path(root, occurrence_id);
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
    for (cell_id, cell) in source.cells {
        if let Some(existing) = destination.cells.get(&cell_id) {
            if serde_json::to_vec(existing)? == serde_json::to_vec(&cell)? {
                continue;
            }
            anyhow::bail!("{source_label} `{cell_id}` conflicts with another execution cell");
        }
        destination.cells.insert(cell_id, cell);
    }
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

pub(super) fn committed_catalog_path(root: &Path, occurrence_id: &str) -> PathBuf {
    repro::repro_dir(root, &repro::guard_repro_id(occurrence_id)).join("providers.yaml")
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
    if catalog.cells.len() > MAX_CELLS {
        anyhow::bail!("execution catalog exceeds {MAX_CELLS} cells");
    }
    for (cell_id, cell) in &catalog.cells {
        validate_provider_id(cell_id)?;
        validate_cell(root, cell_id, cell)?;
    }
    for (provider_id, provider) in &catalog.providers {
        validate_provider_id(provider_id)?;
        if let Some(cell_id) = &provider.cell {
            validate_provider_id(cell_id)?;
            if !catalog.cells.contains_key(cell_id) {
                anyhow::bail!("provider `{provider_id}` names unknown cell `{cell_id}`");
            }
        }
        if let Some(source) = &provider.source {
            validate_provider_source(root, source)?;
        }
        if let Some(debug) = &provider.debug {
            if provider.cell.is_some() {
                anyhow::bail!(
                    "provider `{provider_id}` cannot declare debug alongside an execution cell"
                );
            }
            if provider.phase != ExecutionPhase::Trigger {
                anyhow::bail!("provider `{provider_id}` debug requires phase trigger");
            }
            validate_debug_profile(root, &format!("provider `{provider_id}`"), debug)?;
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
        validate_state_fingerprint(root, provider_id, provider)?;
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

fn validate_cell(root: &Path, cell_id: &str, cell: &ReproductionCell) -> Result<()> {
    match cell {
        ReproductionCell::DockerCompose(cell) => validate_compose_cell(root, cell_id, cell),
    }
}

fn validate_compose_cell(root: &Path, cell_id: &str, cell: &DockerComposeCell) -> Result<()> {
    resolve_checkout_file(root, &cell.compose_file, "compose file")?;
    validate_provider_id(&cell.application_service)?;
    if cell.dependency_services.len() > 63 {
        anyhow::bail!("cell `{cell_id}` exceeds 63 dependency services");
    }
    let mut services = BTreeSet::new();
    services.insert(cell.application_service.as_str());
    for service in &cell.dependency_services {
        validate_provider_id(service)?;
        if !services.insert(service) {
            anyhow::bail!("cell `{cell_id}` repeats service `{service}`");
        }
    }
    if cell.timeout_ms == 0 || cell.timeout_ms > MAX_TIMEOUT_MS {
        anyhow::bail!("cell `{cell_id}` timeoutMs must be within 1..={MAX_TIMEOUT_MS}");
    }
    if let Some(platform) = &cell.platform {
        validate_text(platform, "cell platform")?;
    }
    if let Some(debug) = &cell.debug {
        validate_debug_profile(root, &format!("cell `{cell_id}`"), debug)?;
    }
    Ok(())
}

fn validate_debug_profile(root: &Path, owner: &str, debug: &DebugProfile) -> Result<()> {
    if debug.argv.is_empty() || debug.argv.len() > MAX_COMMAND_ARGS || debug.port == 0 {
        anyhow::bail!("{owner} has an invalid debug profile");
    }
    for argument in &debug.argv {
        validate_text(argument, "debug command argument")?;
    }
    resolve_checkout_directory(root, &debug.local_source_root, "local source root")?;
    if !debug.target_source_root.is_absolute() {
        anyhow::bail!("{owner} targetSourceRoot must be absolute");
    }
    Ok(())
}

fn resolve_checkout_file(root: &Path, configured: &Path, label: &str) -> Result<PathBuf> {
    if configured.is_absolute() {
        anyhow::bail!("{label} must be checkout-relative");
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving checkout root {}", root.display()))?;
    let joined = root.join(configured);
    let path = joined
        .canonicalize()
        .with_context(|| format!("resolving {label} {}", joined.display()))?;
    if !path.starts_with(&root) || !path.is_file() {
        anyhow::bail!("{label} must be a regular file inside the checkout");
    }
    Ok(path)
}

fn resolve_checkout_directory(root: &Path, configured: &Path, label: &str) -> Result<PathBuf> {
    let directory = resolve_working_directory(root, Some(configured))?;
    if !directory.is_dir() {
        anyhow::bail!("{label} must be a directory inside the checkout");
    }
    Ok(directory)
}

fn validate_state_fingerprint(
    root: &Path,
    provider_id: &str,
    provider: &CommandProvider,
) -> Result<()> {
    let changes_state = matches!(provider.phase, ExecutionPhase::Reset | ExecutionPhase::Seed);
    if changes_state && provider.observation.is_some() {
        anyhow::bail!(
            "provider `{provider_id}` phase {:?} must verify state, not match a failure observation",
            provider.phase
        );
    }
    match (&provider.state_fingerprint, changes_state) {
        (None, true) => anyhow::bail!(
            "provider `{provider_id}` phase {:?} requires stateFingerprint verification",
            provider.phase
        ),
        (Some(_), false) => {
            anyhow::bail!("provider `{provider_id}` may use stateFingerprint only in reset or seed")
        }
        (None, false) => return Ok(()),
        (Some(fingerprint), true) => {
            validate_sha256(&fingerprint.expected_sha256, "state fingerprint")?;
            validate_command(
                root,
                &fingerprint.command.argv,
                &fingerprint.command.environment,
                fingerprint.command.working_directory.as_deref(),
                fingerprint.command.timeout_ms,
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
    validate_sha256(&source.sha256, "provider source")?;
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

fn validate_sha256(value: &str, field: &str) -> Result<()> {
    if !value.starts_with("sha256:")
        || value.len() != "sha256:".len() + 64
        || !value["sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        anyhow::bail!("{field} sha256 is invalid");
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

pub(super) fn provider_binding_digest(
    root: &Path,
    catalog: &ProviderCatalog,
    provider: &CommandProvider,
) -> Result<String> {
    let Some(cell_id) = &provider.cell else {
        return provider_digest(provider);
    };
    let cell = catalog
        .cells
        .get(cell_id)
        .with_context(|| format!("provider names unknown cell `{cell_id}`"))?;
    let compose_digest = match cell {
        ReproductionCell::DockerCompose(cell) => {
            let path = resolve_checkout_file(root, &cell.compose_file, "compose file")?;
            sha256_path(&path)?
        }
    };
    let bytes = serde_json::to_vec(&(provider, cell, compose_digest))
        .context("serializing trusted provider and execution cell")?;
    Ok(sha256_bytes(&bytes))
}

/// The digest a provider pins its source file by. Exposed so a refresh can say
/// which pinned sources moved, which is the reason it is being run.
pub(crate) fn source_digest(path: &Path) -> Result<String> {
    sha256_path(path)
}

pub(super) fn sha256_path(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(sha256_bytes(&bytes))
}

pub(super) fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    format!("sha256:{encoded}")
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

/// Re-pin a guard's committed providers onto the current checkout.
///
/// Reads the guard's own `providers.yaml` RAW: `load_catalog` validates the
/// pinned source digest and would reject exactly the state a re-pin exists to
/// resolve. Each provider's `source.sha256` is advanced to what its file
/// hashes to now, the catalog is rewritten, and the resulting provider digest
/// is returned so the plan's bindings can be moved to match. A machine-local
/// copy of the same provider is advanced alongside it, or the two would
/// disagree and every later run would report a provider conflict.
pub(crate) fn repin_guard_providers(
    root: &Path,
    directory: &Path,
    occurrence_id: &str,
) -> Result<String> {
    let path = directory.join("providers.yaml");
    let mut catalog = read_catalog(&path)?;
    for provider in catalog.providers.values_mut() {
        let Some(source) = provider.source.as_mut() else {
            continue;
        };
        let file = root.join(&source.path);
        if !file.is_file() {
            anyhow::bail!(
                "provider source {} is missing from this checkout; there is no mechanism to \
                 re-pin onto",
                source.path.display()
            );
        }
        source.sha256 = sha256_path(&file)?;
    }
    let provider = catalog
        .providers
        .values()
        .next()
        .context("the guard catalog defines no provider")?;
    let digest = provider_binding_digest(root, &catalog, provider)?;

    let encoded = serde_yaml::to_string(&catalog).context("serializing guard providers")?;
    let temporary = directory.join(format!(".providers.{}.tmp", std::process::id()));
    std::fs::write(&temporary, &encoded)?;
    std::fs::rename(&temporary, &path).with_context(|| format!("installing {}", path.display()))?;

    let local = local_catalog_path(root, occurrence_id);
    if local.exists() {
        let temporary = local.with_extension("yaml.tmp");
        std::fs::write(&temporary, &encoded)?;
        std::fs::rename(&temporary, &local)
            .with_context(|| format!("installing {}", local.display()))?;
    }
    Ok(digest)
}

/// The mechanism digest the guard's plan currently pins.
pub(crate) fn pinned_provider_digest(package: &ReproductionPackage) -> Option<String> {
    package
        .plan
        .as_ref()?
        .bindings
        .first()
        .map(|binding| binding.template_digest.clone())
}

/// Accept a reviewed mechanism for this package.
///
/// Every binding's `templateDigest` moves, and then each content-addressed
/// container re-derives its own id with its own code rather than being patched,
/// so nothing ends up with an id that no longer describes it. The occurrence
/// and its recorded artifacts are deliberately untouched.
pub(crate) fn repin_package_mechanism(
    package: &mut ReproductionPackage,
    digest: &str,
) -> Result<()> {
    if let Some(plan) = package.plan.as_mut() {
        for binding in &mut plan.bindings {
            binding.template_digest = digest.to_string();
        }
        plan.finalize_id()
            .map_err(|error| anyhow::anyhow!("re-deriving the reproduction plan id: {error}"))?;
    }
    if let Some(value) = package.capsule.clone() {
        let mut capsule: crate::domain::capsule::Capsule =
            serde_json::from_value(value).context("parsing the guard capsule for a re-pin")?;
        if let Some(plan) = capsule.reproduction_plan.as_mut() {
            for binding in &mut plan.bindings {
                binding.template_digest = digest.to_string();
            }
            plan.finalize_id()
                .map_err(|error| anyhow::anyhow!("re-deriving the capsule's plan id: {error}"))?;
        }
        capsule.finalize_id()?;
        package.capsule = Some(serde_json::to_value(&capsule)?);
    }
    package
        .finalize_id()
        .map_err(|error| anyhow::anyhow!("re-deriving the package id: {error}"))?;
    package
        .validate()
        .map_err(|error| anyhow::anyhow!("the re-pinned package is invalid: {error}"))?;
    Ok(())
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
    let providers: BTreeMap<String, CommandProvider> =
        resolve_providers(root, plan, &package.assessment, &catalog)?
            .into_iter()
            .map(|(provider_id, provider)| (provider_id, provider.clone()))
            .collect();
    let selected_cells = providers
        .values()
        .filter_map(|provider| provider.cell.as_deref())
        .filter_map(|cell_id| {
            catalog
                .cells
                .get(cell_id)
                .cloned()
                .map(|cell| (cell_id.to_string(), cell))
        })
        .collect();
    let committed = ProviderCatalog {
        version: CATALOG_VERSION,
        cells: selected_cells,
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
    root: &Path,
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
        let digest = provider_binding_digest(root, catalog, provider)?;
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
