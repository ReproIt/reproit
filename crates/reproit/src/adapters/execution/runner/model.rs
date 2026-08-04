use crate::domain::execution::{ExecutionPhase, ExecutionVerdict, PhaseRecord};
use reproit_protocol::{
    DebuggerKind, DependencyKind, EnvironmentKind, MechanismAuthority, ObservationKind,
    ReproductionRequirement, RequirementKind, TriggerKind,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ProviderCatalog {
    pub(super) version: u16,
    #[serde(default)]
    pub(super) cells: BTreeMap<String, ReproductionCell>,
    pub(super) providers: BTreeMap<String, CommandProvider>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "driver", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum ReproductionCell {
    DockerCompose(DockerComposeCell),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DockerComposeCell {
    pub(super) compose_file: PathBuf,
    pub(super) application_service: String,
    #[serde(default)]
    pub(super) dependency_services: Vec<String>,
    #[serde(default)]
    pub(super) allow_local_build: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) platform: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub(super) timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) debug: Option<DebugProfile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DebugProfile {
    pub(super) debugger: DebuggerKind,
    pub(super) argv: Vec<String>,
    pub(super) port: u16,
    pub(super) local_source_root: PathBuf,
    pub(super) target_source_root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CommandProvider {
    pub(super) authority: MechanismAuthority,
    pub(super) phase: ExecutionPhase,
    #[serde(default)]
    pub(super) capabilities: BTreeSet<TrustedCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) source: Option<ProviderSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) debug: Option<DebugProfile>,
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
    pub(super) state_fingerprint: Option<StateFingerprint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cleanup: Option<CommandTemplate>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ProviderSource {
    pub(super) path: PathBuf,
    pub(super) sha256: String,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StateFingerprint {
    #[serde(flatten)]
    pub(super) command: CommandTemplate,
    pub(super) expected_sha256: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cell_receipt: Option<reproit_protocol::CellReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) diagnostic_receipt: Option<reproit_protocol::DiagnosticReceipt>,
    pub(crate) authoritative: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct DebugLaunchOptions {
    pub(crate) ide: String,
    pub(crate) open: bool,
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
    pub(super) expected_state_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) actual_state_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) state_verified: Option<bool>,
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
pub(crate) enum ProviderVerdict {
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
