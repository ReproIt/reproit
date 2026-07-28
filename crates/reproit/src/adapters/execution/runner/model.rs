use crate::domain::execution::{ExecutionPhase, ExecutionVerdict, PhaseRecord};
use reproit_protocol::MechanismAuthority;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
