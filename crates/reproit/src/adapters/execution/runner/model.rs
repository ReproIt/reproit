use crate::domain::execution::{ExecutionPhase, ExecutionVerdict, PhaseRecord};
use reproit_protocol::{
    DependencyKind, EnvironmentKind, MechanismAuthority, ObservationKind, ReproductionRequirement,
    RequirementKind, TriggerKind,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ProviderCatalog {
    pub(super) version: u16,
    pub(super) providers: BTreeMap<String, CommandProvider>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CommandProvider {
    pub(super) authority: MechanismAuthority,
    pub(super) phase: ExecutionPhase,
    #[serde(default)]
    pub(super) capabilities: BTreeSet<TrustedCapability>,
    pub(super) argv: Vec<String>,
    #[serde(default)]
    pub(super) environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) working_directory: Option<PathBuf>,
    #[serde(default = "default_timeout_ms")]
    pub(super) timeout_ms: u64,
    #[serde(default = "default_clean_exit_codes")]
    pub(super) clean_exit_codes: Vec<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) observation: Option<CommandObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cleanup: Option<CommandTemplate>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum TrustedCapability {
    Concurrency,
    DistributedSystems,
    Performance,
    Hardware,
    Kernel,
    Environment,
}

pub(super) fn required_trusted_capability(
    requirement: &ReproductionRequirement,
) -> Option<TrustedCapability> {
    match &requirement.requirement {
        RequirementKind::Trigger {
            trigger: TriggerKind::ConcurrencySchedule,
            ..
        } => Some(TrustedCapability::Concurrency),
        RequirementKind::Trigger {
            trigger: TriggerKind::ResourcePressure,
            ..
        }
        | RequirementKind::Observation {
            observation: ObservationKind::Performance,
            ..
        }
        | RequirementKind::Environment {
            environment: EnvironmentKind::Performance,
            ..
        } => Some(TrustedCapability::Performance),
        RequirementKind::Dependency {
            dependency: DependencyKind::DistributedSystem,
            ..
        } => Some(TrustedCapability::DistributedSystems),
        RequirementKind::Environment {
            environment: EnvironmentKind::Hardware,
            ..
        } => Some(TrustedCapability::Hardware),
        RequirementKind::Environment {
            environment: EnvironmentKind::Kernel,
            ..
        } => Some(TrustedCapability::Kernel),
        RequirementKind::Environment { .. } => Some(TrustedCapability::Environment),
        _ => None,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CommandTemplate {
    pub(super) argv: Vec<String>,
    #[serde(default)]
    pub(super) environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) working_directory: Option<PathBuf>,
    #[serde(default = "default_timeout_ms")]
    pub(super) timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CommandObservation {
    pub(super) identity: String,
    #[serde(flatten)]
    pub(super) matcher: ObservationMatcher,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum ObservationMatcher {
    ExitCode { code: i32 },
    Signal { number: i32 },
    StdoutContains { value: String },
    StderrContains { value: String },
    Timeout,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlanRun {
    pub(crate) plan_id: String,
    pub(crate) occurrence_id: String,
    pub(crate) verdict: ExecutionVerdict,
    pub(crate) phases: Vec<PhaseRecord>,
    pub(crate) provider_runs: Vec<ProviderRun>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderRun {
    pub(super) provider_id: String,
    pub(super) phase: ExecutionPhase,
    pub(super) exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) signal: Option<i32>,
    pub(super) timed_out: bool,
    pub(super) output_truncated: bool,
    pub(super) observation_matched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

#[derive(Debug)]
pub(super) struct CommandResult {
    pub(super) exit_code: Option<i32>,
    pub(super) signal: Option<i32>,
    pub(super) timed_out: bool,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) output_truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProviderVerdict {
    SetupPassed,
    Reproduced,
    NotReproduced,
    DifferentFailure,
    InfrastructureFailed,
}

fn default_timeout_ms() -> u64 {
    30_000
}

fn default_clean_exit_codes() -> Vec<i32> {
    vec![0]
}
