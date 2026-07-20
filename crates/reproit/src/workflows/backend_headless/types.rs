use super::*;

#[derive(Debug, Clone)]
pub(super) struct Endpoint {
    pub(super) contract: OperationContract,
    pub(super) method: String,
    pub(super) path: String,
    pub(super) body_only: bool,
    pub(super) content_type: Option<String>,
    pub(super) response_field: Option<String>,
    pub(super) policy: BackendPolicy,
    pub(super) transport: Transport,
    pub(super) schema_source: Option<PathBuf>,
    pub(super) client_streaming: bool,
    pub(super) server_streaming: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Transport {
    #[default]
    Http,
    Grpc,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BackendPolicy {
    #[serde(default)]
    pub(super) invariants: Vec<BackendInvariant>,
    #[serde(default)]
    pub(super) resources: Vec<backend::ResourceLifecycleContract>,
    #[serde(default)]
    pub(super) proofs: Vec<backend::BackendProofContract>,
    #[serde(default)]
    pub(super) fleet: FleetInvariant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestArtifact {
    pub(super) operation: String,
    pub(super) method: String,
    pub(super) url: String,
    pub(super) input: Value,
    #[serde(default)]
    pub(super) headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) body: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) schema_source: Option<PathBuf>,
    #[serde(default)]
    pub(super) client_streaming: bool,
    #[serde(default)]
    pub(super) server_streaming: bool,
    /// Runtime values produced by earlier setup operations are rebound before
    /// replay. This keeps lifecycle reproductions exact when reset generates a
    /// fresh resource identity.
    #[serde(default)]
    pub(super) bindings: Vec<RequestBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RequestBinding {
    pub(super) source_step: usize,
    pub(super) source_output_path: String,
    pub(super) input_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BackendFindingArtifact {
    pub(super) format: String,
    pub(super) version: u32,
    pub(super) schema: String,
    pub(super) schema_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) reset_url: Option<String>,
    #[serde(default)]
    pub(super) setup: Vec<ReplayStep>,
    pub(super) failing: ReplayStep,
    pub(super) finding: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BackendSchemaFindingArtifact {
    pub(super) format: String,
    pub(super) version: u32,
    pub(super) schema: String,
    pub(super) schema_sha256: String,
    pub(super) violation: backend::BackendSchemaViolation,
    pub(super) finding: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReplayStep {
    pub(super) contract: OperationContract,
    pub(super) request: RequestArtifact,
    #[serde(default)]
    pub(super) policy: BackendPolicy,
}

pub(super) type FindingCase = (Endpoint, RequestArtifact, Vec<ReplayStep>, Value);

#[derive(Debug)]
pub(super) struct InvocationResult {
    pub(super) status: u16,
    pub(super) output: Value,
    pub(super) violations: Vec<BackendViolation>,
    pub(super) events: Vec<BackendEvent>,
}
