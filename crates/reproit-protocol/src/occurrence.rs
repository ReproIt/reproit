//! Source-neutral failure occurrences and their immutable evidence inventory.
//!
//! An occurrence records facts received from an evidence source. It contains no
//! executable command, host path, or launch mechanism. Those belong to a later
//! reproduction plan and must be authorized by a trusted checkout or adapter.

use crate::{
    valid_hash, validate_optional_text, validate_optional_token, validate_text, validate_token,
    ProtocolError, ReasonCode, MAX_CONTEXT_BYTES, MAX_TEXT_BYTES, MAX_TOKEN_BYTES,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const OCCURRENCE_VERSION: u16 = 1;
pub const MAX_OCCURRENCE_ARTIFACTS: usize = 256;
pub const MAX_OCCURRENCE_OBSERVATIONS: usize = 256;
pub const MAX_CAPTURE_DEFECTS: usize = 256;
pub const MAX_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_OCCURRENCE_ARTIFACT_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceSource {
    ReproitSdk,
    ReproitCapture,
    Sentry,
    Datadog,
    OpenTelemetry,
    SupportBundle,
    CrashReport,
    SystemLog,
    ManualReport,
    Preproduction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubjectIdentity {
    pub product: String,
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

impl SubjectIdentity {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_token(&self.product)?;
        validate_token(&self.component)?;
        validate_optional_token(&self.platform)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceArtifactKind {
    StructuredLog,
    TextLog,
    TraceGraph,
    CrashDump,
    CoreDump,
    DiagnosticReport,
    EnvironmentInventory,
    ConfigurationInventory,
    ProcessLifecycle,
    InteractionTrace,
    RequestTrace,
    MessageTrace,
    StateManifest,
    FilesystemManifest,
    DatabaseManifest,
    ModuleSymbolManifest,
    Screenshot,
    Recording,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactPolicy {
    Exportable,
    LocalAnalysisOnly,
    EnvironmentBound,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RedactionState {
    NotRequired,
    RedactedAtSource,
    RedactedBeforeStorage,
    UnredactedRestricted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CollectionMethod {
    FlightRecorder,
    CrashCollector,
    SupportCollector,
    TelemetryExport,
    ManualAttachment,
    Derived,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceArtifact {
    /// Content identity of the exact stored bytes.
    pub id: String,
    pub kind: EvidenceArtifactKind,
    pub media_type: String,
    pub bytes: u64,
    pub policy: ArtifactPolicy,
    pub redaction: RedactionState,
    pub collection: CollectionMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl EvidenceArtifact {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if !self.id.starts_with("sha256:") || !valid_hash(&self.id[7..], 64) {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        if self.bytes > MAX_ARTIFACT_BYTES {
            return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
        }
        validate_text(&self.media_type, MAX_TOKEN_BYTES)?;
        if self.media_type.is_empty()
            || !self
                .media_type
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace())
        {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        validate_optional_token(&self.encryption_key_id)?;
        validate_optional_text(&self.name, MAX_TEXT_BYTES)?;
        if self.redaction == RedactionState::UnredactedRestricted
            && self.policy == ArtifactPolicy::Exportable
        {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationKind {
    Exception,
    Crash,
    Exit,
    Hang,
    Diagnostic,
    ContractViolation,
    DataCorruption,
    Performance,
    UserReport,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationAuthority {
    SourceClaim,
    RuntimeDiagnosis,
    AuthoredContract,
    PublishedStandard,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureObservation {
    pub kind: ObservationKind,
    pub authority: ObservationAuthority,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_point: Option<String>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
}

impl FailureObservation {
    fn validate(&self, artifacts: &BTreeSet<&str>) -> Result<(), ProtocolError> {
        validate_text(&self.summary, MAX_TEXT_BYTES)?;
        if self.summary.is_empty() {
            return Err(ProtocolError::new(ReasonCode::NoObservations));
        }
        validate_optional_text(&self.signature, MAX_TEXT_BYTES)?;
        validate_optional_text(&self.observation_point, MAX_TEXT_BYTES)?;
        if self.artifact_ids.len() > MAX_OCCURRENCE_ARTIFACTS {
            return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
        }
        let mut seen = BTreeSet::new();
        for artifact_id in &self.artifact_ids {
            if !artifacts.contains(artifact_id.as_str()) || !seen.insert(artifact_id) {
                return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureDefectKind {
    Dropped,
    Unavailable,
    Unsupported,
    Truncated,
    SampledOut,
    Rejected,
    ClockUncertain,
    SequenceGap,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureDefect {
    pub kind: CaptureDefectKind,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
}

impl CaptureDefect {
    fn validate(&self, artifacts: &BTreeSet<&str>) -> Result<(), ProtocolError> {
        validate_text(&self.detail, MAX_TEXT_BYTES)?;
        if self.detail.is_empty()
            || self
                .artifact_id
                .as_deref()
                .is_some_and(|id| !artifacts.contains(id))
        {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsentClass {
    ApplicationTelemetry,
    SupportExport,
    LocalAnalysis,
    Preproduction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidencePolicy {
    pub consent: ConsentClass,
    pub retention_class: String,
}

impl EvidencePolicy {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_token(&self.retention_class)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OccurrenceEnvelope {
    pub version: u16,
    pub occurrence_id: String,
    pub source: EvidenceSource,
    pub subject: SubjectIdentity,
    pub observed_at: String,
    pub received_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<crate::DeploymentIdentity>,
    pub observations: Vec<FailureObservation>,
    #[serde(default)]
    pub artifacts: Vec<EvidenceArtifact>,
    #[serde(default)]
    pub capture_defects: Vec<CaptureDefect>,
    pub policy: EvidencePolicy,
}

impl OccurrenceEnvelope {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != OCCURRENCE_VERSION {
            return Err(ProtocolError::new(ReasonCode::UnsupportedVersion));
        }
        validate_token(&self.occurrence_id)?;
        if !self.occurrence_id.starts_with("occ_") {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        self.subject.validate()?;
        validate_text(&self.observed_at, MAX_TOKEN_BYTES)?;
        validate_text(&self.received_at, MAX_TOKEN_BYTES)?;
        if self.observed_at.is_empty() || self.received_at.is_empty() {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        if let Some(deployment) = &self.deployment {
            deployment.validate()?;
        }
        if self.observations.is_empty()
            || self.observations.len() > MAX_OCCURRENCE_OBSERVATIONS
            || self.artifacts.len() > MAX_OCCURRENCE_ARTIFACTS
            || self.capture_defects.len() > MAX_CAPTURE_DEFECTS
        {
            return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
        }
        let mut artifact_ids = BTreeSet::new();
        let mut total_bytes = 0u64;
        for artifact in &self.artifacts {
            artifact.validate()?;
            if !artifact_ids.insert(artifact.id.as_str()) {
                return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
            }
            total_bytes = total_bytes
                .checked_add(artifact.bytes)
                .ok_or_else(|| ProtocolError::new(ReasonCode::BatchTooLarge))?;
        }
        if total_bytes > MAX_OCCURRENCE_ARTIFACT_BYTES {
            return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
        }
        for observation in &self.observations {
            observation.validate(&artifact_ids)?;
        }
        for defect in &self.capture_defects {
            defect.validate(&artifact_ids)?;
        }
        self.policy.validate()?;
        let encoded =
            serde_json::to_value(self).map_err(|_| ProtocolError::new(ReasonCode::InvalidEvent))?;
        crate::validate_value(&encoded, MAX_CONTEXT_BYTES)?;
        Ok(())
    }
}
