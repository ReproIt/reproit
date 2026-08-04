use super::*;
use reproit_protocol::UnresolvedRequirementReason;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompilationBlocker {
    pub(crate) code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) requirement_id: Option<String>,
    pub(crate) reason: UnresolvedRequirementReason,
    pub(crate) detail: String,
}

impl std::fmt::Display for CompilationBlocker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

pub(crate) enum AutomaticCompilation {
    Compiled(Box<ReproductionPackage>),
    Blocked(Vec<CompilationBlocker>),
}

pub(crate) fn compile_package_automatically(
    root: &Path,
    package: &ReproductionPackage,
) -> Result<AutomaticCompilation> {
    package
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid imported package: {error}"))?;
    let identities: BTreeSet<_> = package
        .occurrence
        .observations
        .iter()
        .filter_map(|observation| observation.signature.as_deref())
        .collect();
    if identities.len() != 1 {
        return Ok(AutomaticCompilation::Blocked(vec![CompilationBlocker {
            code: "ambiguous-observation-identity",
            requirement_id: None,
            reason: UnresolvedRequirementReason::AmbiguousMapping,
            detail: "automatic planning requires exactly one source observation identity".into(),
        }]));
    }
    let identity = identities.into_iter().next().unwrap();
    if read_project_catalog(&root.join("reproit.yaml"))?.is_none() {
        return Ok(AutomaticCompilation::Blocked(vec![CompilationBlocker {
            code: "missing-provider-catalog",
            requirement_id: None,
            reason: UnresolvedRequirementReason::UnsupportedCapability,
            detail: "no trusted execution providers; add execution.providers to reproit.yaml"
                .into(),
        }]));
    }
    let catalog = load_catalog(root, None, None)?;
    let mut requested_bindings = BTreeMap::new();
    let mut blockers = Vec::new();
    for requirement in package
        .assessment
        .requirements
        .iter()
        .filter(|requirement| requirement.level == RequirementLevel::Required)
    {
        let candidates: Vec<_> = catalog
            .providers
            .iter()
            .filter(|(_, provider)| provider.phase == requirement_phase(requirement))
            .filter(|(_, provider)| {
                required_trusted_capability(requirement)
                    .is_none_or(|capability| provider.capabilities.contains(&capability))
            })
            .filter(|(_, provider)| {
                provider
                    .observation
                    .as_ref()
                    .is_none_or(|observation| observation.identity == identity)
            })
            .map(|(provider_id, _)| provider_id.as_str())
            .collect();
        match candidates.as_slice() {
            [provider_id] => {
                requested_bindings.insert(requirement.id.clone(), (*provider_id).to_string());
            }
            [] => blockers.push(CompilationBlocker {
                code: "unsupported-trusted-capability",
                requirement_id: Some(requirement.id.clone()),
                reason: UnresolvedRequirementReason::UnsupportedCapability,
                detail: format!(
                    "requirement `{}` has no trusted {:?} provider compatible with `{identity}`",
                    requirement.id,
                    requirement_phase(requirement)
                ),
            }),
            _ => blockers.push(CompilationBlocker {
                code: "ambiguous-trusted-provider",
                requirement_id: Some(requirement.id.clone()),
                reason: UnresolvedRequirementReason::AmbiguousMapping,
                detail: format!(
                    "requirement `{}` is ambiguous across trusted providers: {}",
                    requirement.id,
                    candidates.join(", ")
                ),
            }),
        }
    }
    if !blockers.is_empty() {
        return Ok(AutomaticCompilation::Blocked(blockers));
    }
    compile_package(root, package, &requested_bindings, identity)
        .map(Box::new)
        .map(AutomaticCompilation::Compiled)
}

pub(crate) fn assess_package_readiness(
    root: &Path,
    package: &ReproductionPackage,
) -> Result<reproit_protocol::ReadinessAssessment> {
    package
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid reproduction package: {error}"))?;
    let debug_gaps = debug_gaps(root, package);
    let fidelity = fidelity_claims(package);
    reproit_protocol::assess_readiness(
        &package.occurrence,
        &package.assessment,
        package.plan.as_ref(),
        debug_gaps,
        fidelity,
    )
    .map_err(|error| anyhow::anyhow!("invalid readiness assessment: {error}"))
}

fn debug_gaps(root: &Path, package: &ReproductionPackage) -> Vec<reproit_protocol::CapabilityGap> {
    let Some(plan) = &package.plan else {
        return Vec::new();
    };
    let catalog = match load_catalog(root, Some(&plan.occurrence_id), Some(&plan.id)) {
        Ok(catalog) => catalog,
        Err(error) => {
            return vec![gap(
                "debug-provider",
                reproit_protocol::CapabilityGapReason::UnsupportedExecutor,
                &format!("trusted debug provider resolution failed: {error}"),
                Some("add a trusted execution provider with a debug profile"),
            )];
        }
    };
    match &plan.destination {
        ExecutionDestination::LocalCompose => compose_debug_gaps(plan, &catalog),
        ExecutionDestination::LocalProcess
        | ExecutionDestination::Simulator { .. }
        | ExecutionDestination::PhysicalDevice { .. }
        | ExecutionDestination::LocalVm { .. } => provider_debug_gaps(plan, &catalog),
        ExecutionDestination::CustomerCi
        | ExecutionDestination::PrivateWorker { .. }
        | ExecutionDestination::HostedWorker { .. } => vec![gap(
            "debug-executor",
            reproit_protocol::CapabilityGapReason::Unauthorized,
            "the selected remote executor does not expose an authorized local debugger endpoint",
            Some("run the occurrence on a trusted local executor with a debug capability"),
        )],
    }
}

fn compose_debug_gaps(
    plan: &ReproductionPlan,
    catalog: &ProviderCatalog,
) -> Vec<reproit_protocol::CapabilityGap> {
    let cells = plan
        .bindings
        .iter()
        .filter_map(|binding| catalog.providers.get(&binding.provider_id))
        .filter_map(|provider| provider.cell.as_deref())
        .collect::<BTreeSet<_>>();
    if cells.len() != 1 {
        return vec![gap(
            "debug-cell-identity",
            reproit_protocol::CapabilityGapReason::AmbiguousMapping,
            "the replay plan does not resolve to exactly one execution cell",
            None,
        )];
    }
    let cell_id = *cells.iter().next().expect("one cell was checked");
    match catalog.cells.get(cell_id) {
        Some(ReproductionCell::DockerCompose(cell)) if cell.debug.is_some() => Vec::new(),
        Some(ReproductionCell::DockerCompose(_)) => vec![gap(
            "debug-profile",
            reproit_protocol::CapabilityGapReason::UnsupportedExecutor,
            &format!("execution cell `{cell_id}` has no debugger profile"),
            Some("declare the debugger command, port, and source mapping in reproit.yaml"),
        )],
        None => vec![gap(
            "debug-cell-identity",
            reproit_protocol::CapabilityGapReason::MissingEvidence,
            &format!("execution cell `{cell_id}` is unavailable"),
            None,
        )],
    }
}

fn provider_debug_gaps(
    plan: &ReproductionPlan,
    catalog: &ProviderCatalog,
) -> Vec<reproit_protocol::CapabilityGap> {
    let provider_ids = plan
        .bindings
        .iter()
        .map(|binding| binding.provider_id.as_str())
        .collect::<BTreeSet<_>>();
    let debug_providers = provider_ids
        .iter()
        .filter_map(|provider_id| {
            catalog
                .providers
                .get(*provider_id)
                .filter(|provider| provider.debug.is_some())
                .map(|_| *provider_id)
        })
        .collect::<Vec<_>>();
    match debug_providers.as_slice() {
        [_] => Vec::new(),
        [] => vec![gap(
            "debug-provider",
            reproit_protocol::CapabilityGapReason::UnsupportedExecutor,
            "the replay plan has no bound trusted provider with a debug capability",
            Some("add debug argv, port, debugger, and source mapping to the trigger provider"),
        )],
        _ => vec![gap(
            "debug-provider",
            reproit_protocol::CapabilityGapReason::AmbiguousMapping,
            "the replay plan resolves to more than one provider debug capability",
            Some("retain exactly one debug capability across the bound providers"),
        )],
    }
}

fn fidelity_claims(package: &ReproductionPackage) -> Vec<reproit_protocol::FidelityClaim> {
    let Some(deployment) = &package.occurrence.deployment else {
        return Vec::new();
    };
    let destination = package
        .plan
        .as_ref()
        .map(|plan| destination_name(&plan.destination));
    deployment
        .platforms
        .iter()
        .map(|evidence| {
            let source = platform_name(&evidence.platform);
            let status = match (source, package.plan.as_ref().map(|plan| &plan.destination)) {
                ("docker-compose", Some(ExecutionDestination::LocalCompose)) => {
                    reproit_protocol::FidelityStatus::Exact
                }
                (_, Some(_)) => reproit_protocol::FidelityStatus::Changed,
                (_, None) => reproit_protocol::FidelityStatus::Unavailable,
            };
            reproit_protocol::FidelityClaim {
                dimension: "deployment-platform".into(),
                status,
                source_value: source.into(),
                replay_value: destination.map(str::to_string),
                evidence_source: evidence.collector.clone(),
            }
        })
        .collect()
}

fn gap(
    capability: &str,
    reason: reproit_protocol::CapabilityGapReason,
    detail: &str,
    next_action: Option<&str>,
) -> reproit_protocol::CapabilityGap {
    reproit_protocol::CapabilityGap {
        capability: capability.into(),
        reason,
        detail: detail.into(),
        next_action: next_action.map(str::to_string),
    }
}

fn platform_name(platform: &reproit_protocol::PlatformIdentity) -> &'static str {
    match platform {
        reproit_protocol::PlatformIdentity::Kubernetes { .. } => "kubernetes",
        reproit_protocol::PlatformIdentity::DockerCompose { .. } => "docker-compose",
        reproit_protocol::PlatformIdentity::Ecs { .. } => "ecs",
        reproit_protocol::PlatformIdentity::Serverless { .. } => "serverless",
        reproit_protocol::PlatformIdentity::NativeService { .. } => "native-service",
        reproit_protocol::PlatformIdentity::Ci { .. } => "ci",
        reproit_protocol::PlatformIdentity::Android { .. } => "android",
        reproit_protocol::PlatformIdentity::Ios { .. } => "ios",
    }
}

fn destination_name(destination: &ExecutionDestination) -> &'static str {
    match destination {
        ExecutionDestination::LocalProcess => "local-process",
        ExecutionDestination::LocalCompose => "docker-compose",
        ExecutionDestination::Simulator { .. } => "simulator",
        ExecutionDestination::PhysicalDevice { .. } => "physical-device",
        ExecutionDestination::LocalVm { .. } => "local-vm",
        ExecutionDestination::CustomerCi => "customer-ci",
        ExecutionDestination::PrivateWorker { .. } => "private-worker",
        ExecutionDestination::HostedWorker { .. } => "hosted-worker",
    }
}
