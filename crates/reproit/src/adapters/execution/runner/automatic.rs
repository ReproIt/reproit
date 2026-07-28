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
    if read_project_catalog(&root.join("reproit.yaml"))?.is_none()
        && !root.join("reproit.execution.yaml").exists()
    {
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
