//! Typed assessment, planning, packaging, and offline-bundle contracts.
//!
//! Evidence can identify requirements and supply bounded parameters. Only a
//! trusted checkout, built-in adapter, organization policy, or explicit local
//! approval may authorize an execution mechanism.

use crate::{
    valid_hash, validate_optional_text, validate_text, validate_token, validate_value,
    OccurrenceEnvelope, ProtocolError, ReasonCode, MAX_CONTEXT_BYTES, MAX_TEXT_BYTES,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const PLAN_VERSION: u16 = 1;
pub const PACKAGE_VERSION: u16 = 1;
pub const SUPPORT_BUNDLE_VERSION: u16 = 1;
pub const MAX_REQUIREMENTS: usize = 512;
pub const MAX_PLAN_BINDINGS: usize = 512;
pub const MAX_LEGACY_ACTIONS: usize = 512;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessOperation {
    Build,
    Launch,
    Attach,
    Stop,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerKind {
    UiAction,
    HttpRequest,
    RpcRequest,
    Command,
    Message,
    Timer,
    ProcessStartup,
    Installer,
    Upgrade,
    Migration,
    FilesystemEvent,
    Signal,
    ResourcePressure,
    ConcurrencySchedule,
    DeviceInteraction,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum StateKind {
    Filesystem,
    Registry,
    Database,
    Cache,
    Queue,
    ObjectStore,
    ApplicationStorage,
    Device,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyKind {
    FirstPartyCurrentCheckout,
    CapturedReplay,
    ApprovedSandbox,
    DeterministicStub,
    EnvironmentBound,
    DistributedSystem,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentKind {
    OperatingSystem,
    Architecture,
    Runtime,
    Locale,
    Timezone,
    Clock,
    Randomness,
    Network,
    Concurrency,
    Device,
    Driver,
    Graphics,
    Hardware,
    Kernel,
    Performance,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DebuggerKind {
    ChromeDevtools,
    NodeInspector,
    Lldb,
    Gdb,
    Jdwp,
    Dotnet,
    LanguageSpecific,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RequirementKind {
    Process {
        role: String,
        operation: ProcessOperation,
    },
    Trigger {
        trigger: TriggerKind,
        subject: String,
    },
    State {
        state: StateKind,
        subject: String,
    },
    Dependency {
        dependency: DependencyKind,
        subject: String,
    },
    Environment {
        environment: EnvironmentKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        required_value: Option<String>,
    },
    Observation {
        observation: crate::ObservationKind,
        subject: String,
    },
    Debugger {
        debugger: DebuggerKind,
    },
}

impl RequirementKind {
    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Process { role, .. } => validate_token(role),
            Self::Trigger { subject, .. }
            | Self::State { subject, .. }
            | Self::Dependency { subject, .. }
            | Self::Observation { subject, .. } => validate_text(subject, MAX_TEXT_BYTES),
            Self::Environment { required_value, .. } => {
                validate_optional_text(required_value, MAX_TEXT_BYTES)
            }
            Self::Debugger { .. } => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequirementLevel {
    Required,
    Optional,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReproductionRequirement {
    pub id: String,
    pub level: RequirementLevel,
    pub requirement: RequirementKind,
    #[serde(default)]
    pub evidence_artifact_ids: Vec<String>,
}

impl ReproductionRequirement {
    fn validate(&self, artifact_ids: &BTreeSet<&str>) -> Result<(), ProtocolError> {
        validate_token(&self.id)?;
        if !self.id.starts_with("req_") {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        self.requirement.validate()?;
        if self.evidence_artifact_ids.len() > crate::MAX_OCCURRENCE_ARTIFACTS {
            return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
        }
        let mut seen = BTreeSet::new();
        for artifact_id in &self.evidence_artifact_ids {
            if !artifact_ids.contains(artifact_id.as_str()) || !seen.insert(artifact_id) {
                return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnresolvedRequirementReason {
    MissingEvidence,
    UnsupportedCapability,
    UnauthorizedDestination,
    EnvironmentUnavailable,
    AmbiguousMapping,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnresolvedRequirement {
    pub requirement_id: String,
    pub reason: UnresolvedRequirementReason,
    pub detail: String,
}

impl UnresolvedRequirement {
    fn validate(&self, requirement_ids: &BTreeSet<&str>) -> Result<(), ProtocolError> {
        if !requirement_ids.contains(self.requirement_id.as_str()) {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        validate_text(&self.detail, MAX_TEXT_BYTES)?;
        if self.detail.is_empty() {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssessmentStatus {
    Eligible,
    Incomplete,
    Unsupported,
    EnvironmentBound,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityAssessment {
    pub occurrence_id: String,
    pub status: AssessmentStatus,
    pub requirements: Vec<ReproductionRequirement>,
    #[serde(default)]
    pub unresolved: Vec<UnresolvedRequirement>,
}

impl CapabilityAssessment {
    pub fn validate(&self, occurrence: &OccurrenceEnvelope) -> Result<(), ProtocolError> {
        if self.occurrence_id != occurrence.occurrence_id
            || self.requirements.len() > MAX_REQUIREMENTS
            || self.unresolved.len() > MAX_REQUIREMENTS
        {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        let artifact_ids = occurrence
            .artifacts
            .iter()
            .map(|artifact| artifact.id.as_str())
            .collect();
        let mut requirement_ids = BTreeSet::new();
        for requirement in &self.requirements {
            requirement.validate(&artifact_ids)?;
            if !requirement_ids.insert(requirement.id.as_str()) {
                return Err(ProtocolError::new(ReasonCode::InvalidEvent));
            }
        }
        let mut unresolved_ids = BTreeSet::new();
        for unresolved in &self.unresolved {
            unresolved.validate(&requirement_ids)?;
            if !unresolved_ids.insert(unresolved.requirement_id.as_str()) {
                return Err(ProtocolError::new(ReasonCode::InvalidEvent));
            }
        }
        let required_unresolved = self.requirements.iter().any(|requirement| {
            requirement.level == RequirementLevel::Required
                && unresolved_ids.contains(requirement.id.as_str())
        });
        if self.status == AssessmentStatus::Eligible && required_unresolved {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        if self.status != AssessmentStatus::Eligible && !required_unresolved {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ExecutionDestination {
    LocalProcess,
    LocalCompose,
    Simulator { platform: String },
    PhysicalDevice { platform: String },
    LocalVm { platform: String },
    CustomerCi,
    PrivateWorker { worker_class: String },
    HostedWorker { worker_class: String },
}

impl ExecutionDestination {
    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Simulator { platform }
            | Self::PhysicalDevice { platform }
            | Self::LocalVm { platform } => validate_token(platform),
            Self::PrivateWorker { worker_class } | Self::HostedWorker { worker_class } => {
                validate_token(worker_class)
            }
            Self::LocalProcess | Self::LocalCompose | Self::CustomerCi => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MechanismAuthority {
    TrustedCheckout,
    BuiltInAdapter,
    OrganizationPolicy,
    ExplicitLocalApproval,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanBinding {
    pub requirement_id: String,
    pub provider_id: String,
    pub mechanism_authority: MechanismAuthority,
    /// Digest of the trusted template. Evidence cannot supply this template.
    pub template_digest: String,
    #[serde(default)]
    pub evidence_artifact_ids: Vec<String>,
}

impl PlanBinding {
    fn validate(
        &self,
        requirement_ids: &BTreeSet<&str>,
        artifact_ids: &BTreeSet<&str>,
    ) -> Result<(), ProtocolError> {
        if !requirement_ids.contains(self.requirement_id.as_str()) {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        validate_token(&self.provider_id)?;
        if !self.template_digest.starts_with("sha256:")
            || !valid_hash(&self.template_digest[7..], 64)
        {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        let mut seen = BTreeSet::new();
        for artifact_id in &self.evidence_artifact_ids {
            if !artifact_ids.contains(artifact_id.as_str()) || !seen.insert(artifact_id) {
                return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservationTarget {
    pub observation: crate::ObservationKind,
    pub identity: String,
    pub authority: crate::ObservationAuthority,
}

impl ObservationTarget {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_text(&self.identity, MAX_TEXT_BYTES)?;
        if self.identity.is_empty() {
            return Err(ProtocolError::new(ReasonCode::NoObservations));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReproductionPlan {
    pub version: u16,
    pub id: String,
    pub occurrence_id: String,
    pub target: String,
    pub destination: ExecutionDestination,
    pub bindings: Vec<PlanBinding>,
    pub observation: ObservationTarget,
}

impl ReproductionPlan {
    pub fn finalize_id(&mut self) -> Result<String, ProtocolError> {
        self.id.clear();
        let bytes =
            serde_json::to_vec(self).map_err(|_| ProtocolError::new(ReasonCode::InvalidEvent))?;
        self.id = format!("plan_{}", &hex::encode(Sha256::digest(bytes))[..16]);
        Ok(self.id.clone())
    }

    pub fn validate(
        &self,
        occurrence: &OccurrenceEnvelope,
        assessment: &CapabilityAssessment,
    ) -> Result<(), ProtocolError> {
        if self.version != PLAN_VERSION
            || self.occurrence_id != occurrence.occurrence_id
            || assessment.status != AssessmentStatus::Eligible
            || self.bindings.len() > MAX_PLAN_BINDINGS
        {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        validate_token(&self.target)?;
        self.destination.validate()?;
        self.observation.validate()?;
        let requirement_ids = assessment
            .requirements
            .iter()
            .map(|requirement| requirement.id.as_str())
            .collect();
        let artifact_ids = occurrence
            .artifacts
            .iter()
            .map(|artifact| artifact.id.as_str())
            .collect();
        let mut bound = BTreeSet::new();
        for binding in &self.bindings {
            binding.validate(&requirement_ids, &artifact_ids)?;
            if !bound.insert(binding.requirement_id.as_str()) {
                return Err(ProtocolError::new(ReasonCode::InvalidEvent));
            }
        }
        let all_required_bound = assessment
            .requirements
            .iter()
            .filter(|requirement| requirement.level == RequirementLevel::Required)
            .all(|requirement| bound.contains(requirement.id.as_str()));
        if !all_required_bound {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        let mut canonical = self.clone();
        canonical.finalize_id()?;
        if canonical.id != self.id {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyReplay {
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub fixture: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crash_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_signature: Option<String>,
}

impl LegacyReplay {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.actions.len() > MAX_LEGACY_ACTIONS {
            return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
        }
        for action in &self.actions {
            validate_text(action, MAX_TEXT_BYTES)?;
        }
        validate_value(&self.fixture, MAX_CONTEXT_BYTES)?;
        validate_optional_text(&self.crash_signature, MAX_TEXT_BYTES)?;
        validate_optional_text(&self.start_signature, MAX_TEXT_BYTES)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReproductionPackage {
    pub version: u16,
    pub id: String,
    pub occurrence: OccurrenceEnvelope,
    pub assessment: CapabilityAssessment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<ReproductionPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capsule: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy: Option<LegacyReplay>,
}

impl ReproductionPackage {
    pub fn finalize_id(&mut self) -> Result<String, ProtocolError> {
        self.id.clear();
        let bytes =
            serde_json::to_vec(self).map_err(|_| ProtocolError::new(ReasonCode::InvalidEvent))?;
        self.id = format!("pkg_{}", &hex::encode(Sha256::digest(bytes))[..16]);
        Ok(self.id.clone())
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != PACKAGE_VERSION {
            return Err(ProtocolError::new(ReasonCode::UnsupportedVersion));
        }
        self.occurrence.validate()?;
        self.assessment.validate(&self.occurrence)?;
        if let Some(plan) = &self.plan {
            plan.validate(&self.occurrence, &self.assessment)?;
        }
        if self.assessment.status == AssessmentStatus::Eligible
            && self.plan.is_none()
            && self.legacy.is_none()
            && self.capsule.is_none()
        {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        if let Some(capsule) = &self.capsule {
            validate_value(capsule, crate::MAX_FRAME_BYTES)?;
        }
        if let Some(legacy) = &self.legacy {
            legacy.validate()?;
        }
        let mut canonical = self.clone();
        canonical.finalize_id()?;
        if canonical.id != self.id {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BundleSignatureAlgorithm {
    Ed25519,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleSignature {
    pub algorithm: BundleSignatureAlgorithm,
    pub key_id: String,
    pub public_key: String,
    pub signature: String,
}

impl BundleSignature {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_token(&self.key_id)?;
        if self.public_key.len() != 64
            || !valid_hash(&self.public_key, 64)
            || self.signature.len() != 128
            || !valid_hash(&self.signature, 128)
        {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BundleEncryptionAlgorithm {
    Xchacha20Poly1305,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleEncryption {
    pub algorithm: BundleEncryptionAlgorithm,
    pub recipient_key_id: String,
    pub nonce: String,
}

impl BundleEncryption {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_token(&self.recipient_key_id)?;
        if self.nonce.len() != 48 || !valid_hash(&self.nonce, 48) {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupportBundleManifest {
    pub version: u16,
    pub bundle_id: String,
    pub occurrence: OccurrenceEnvelope,
    pub encryption: BundleEncryption,
    pub payload_sha256: String,
    pub signature: BundleSignature,
}

impl SupportBundleManifest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != SUPPORT_BUNDLE_VERSION {
            return Err(ProtocolError::new(ReasonCode::UnsupportedVersion));
        }
        if !self.bundle_id.starts_with("rpb_")
            || self.bundle_id.len() != 68
            || !valid_hash(&self.bundle_id[4..], 64)
        {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        if !self.payload_sha256.starts_with("sha256:")
            || !valid_hash(&self.payload_sha256[7..], 64)
            || self.bundle_id[4..] != self.payload_sha256[7..]
        {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        self.occurrence.validate()?;
        self.encryption.validate()?;
        self.signature.validate()
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct SigningPayload<'a> {
            version: u16,
            bundle_id: &'a str,
            occurrence: &'a OccurrenceEnvelope,
            encryption: &'a BundleEncryption,
            payload_sha256: &'a str,
            signature_algorithm: BundleSignatureAlgorithm,
            signature_key_id: &'a str,
            signature_public_key: &'a str,
        }

        serde_json::to_vec(&SigningPayload {
            version: self.version,
            bundle_id: &self.bundle_id,
            occurrence: &self.occurrence,
            encryption: &self.encryption,
            payload_sha256: &self.payload_sha256,
            signature_algorithm: self.signature.algorithm,
            signature_key_id: &self.signature.key_id,
            signature_public_key: &self.signature.public_key,
        })
        .map_err(|_| ProtocolError::new(ReasonCode::InvalidArtifact))
    }
}
