//! Bounded deployment evidence supplied by platform collectors.
//!
//! Application SDKs do not infer these values. A platform adapter observes
//! them and attaches the resulting evidence to the capture batch.

use crate::{validate_optional_text, validate_text, ProtocolError, ReasonCode, MAX_TEXT_BYTES};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PLATFORM_EVIDENCE_VERSION: u16 = 1;
pub const PLATFORM_EVIDENCE_BATCH_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkloadIdentity {
    pub service: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

impl WorkloadIdentity {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_required(&self.service)?;
        validate_optional_text(&self.workload, MAX_TEXT_BYTES)?;
        validate_optional_text(&self.instance, MAX_TEXT_BYTES)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_digest: Option<String>,
}

impl BuildIdentity {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.image_digest.is_none() && self.artifact_digest.is_none() {
            return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
        }
        for digest in [&self.image_digest, &self.artifact_digest]
            .into_iter()
            .flatten()
        {
            validate_sha256(digest)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_storage_bytes: Option<u64>,
}

impl ResourceLimits {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.cpu_millis == Some(0)
            || self.memory_bytes == Some(0)
            || self.ephemeral_storage_bytes == Some(0)
        {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PlatformIdentity {
    Kubernetes {
        namespace: String,
        workload_kind: String,
        workload_name: String,
        pod_uid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cluster: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        container: Option<String>,
    },
    DockerCompose {
        project: String,
        service: String,
        container_id: String,
    },
    Ecs {
        cluster: String,
        task_arn: String,
        container: String,
    },
    Serverless {
        provider: String,
        function: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instance: Option<String>,
    },
    NativeService {
        operating_system: String,
        service_manager: String,
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instance: Option<String>,
    },
    Ci {
        provider: String,
        pipeline: String,
        job: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runner: Option<String>,
    },
    Android {
        serial: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_level: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        architecture: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        application_id: Option<String>,
    },
    Ios {
        udid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runtime: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bundle_id: Option<String>,
    },
}

impl PlatformIdentity {
    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Kubernetes {
                namespace,
                workload_kind,
                workload_name,
                pod_uid,
                cluster,
                container,
            } => {
                validate_required_all(&[namespace, workload_kind, workload_name, pod_uid])?;
                validate_optional_text(cluster, MAX_TEXT_BYTES)?;
                validate_optional_text(container, MAX_TEXT_BYTES)
            }
            Self::DockerCompose {
                project,
                service,
                container_id,
            } => validate_required_all(&[project, service, container_id]),
            Self::Ecs {
                cluster,
                task_arn,
                container,
            } => validate_required_all(&[cluster, task_arn, container]),
            Self::Serverless {
                provider,
                function,
                region,
                instance,
            } => {
                validate_required_all(&[provider, function])?;
                validate_optional_text(region, MAX_TEXT_BYTES)?;
                validate_optional_text(instance, MAX_TEXT_BYTES)
            }
            Self::NativeService {
                operating_system,
                service_manager,
                service,
                instance,
            } => {
                validate_required_all(&[operating_system, service_manager, service])?;
                validate_optional_text(instance, MAX_TEXT_BYTES)
            }
            Self::Ci {
                provider,
                pipeline,
                job,
                runner,
            } => {
                validate_required_all(&[provider, pipeline, job])?;
                validate_optional_text(runner, MAX_TEXT_BYTES)
            }
            Self::Android {
                serial,
                api_level,
                architecture,
                application_id,
            } => {
                validate_required(serial)?;
                validate_optional_text(api_level, MAX_TEXT_BYTES)?;
                validate_optional_text(architecture, MAX_TEXT_BYTES)?;
                validate_optional_text(application_id, MAX_TEXT_BYTES)
            }
            Self::Ios {
                udid,
                runtime,
                device_type,
                bundle_id,
            } => {
                validate_required(udid)?;
                validate_optional_text(runtime, MAX_TEXT_BYTES)?;
                validate_optional_text(device_type, MAX_TEXT_BYTES)?;
                validate_optional_text(bundle_id, MAX_TEXT_BYTES)
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformEvidence {
    pub version: u16,
    pub collector: String,
    pub platform: PlatformIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload: Option<WorkloadIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceLimits>,
    #[serde(default)]
    pub missing_capabilities: Vec<String>,
}

impl PlatformEvidence {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != PLATFORM_EVIDENCE_VERSION || self.missing_capabilities.len() > 128 {
            return Err(ProtocolError::new(ReasonCode::UnsupportedVersion));
        }
        validate_required(&self.collector)?;
        self.platform.validate()?;
        if let Some(workload) = &self.workload {
            workload.validate()?;
        }
        if let Some(build) = &self.build {
            build.validate()?;
        }
        if let Some(resources) = &self.resources {
            resources.validate()?;
        }
        for capability in &self.missing_capabilities {
            validate_required(capability)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformEvidenceBatch {
    pub version: u16,
    pub batch_id: String,
    pub project_id: String,
    pub session_id: String,
    pub emitter_id: String,
    pub observed_at: String,
    #[serde(default)]
    pub evidence: Vec<PlatformEvidence>,
    #[serde(default)]
    pub gaps: Vec<String>,
}

impl PlatformEvidenceBatch {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != PLATFORM_EVIDENCE_BATCH_VERSION
            || self.evidence.len() > 16
            || self.gaps.len() > 128
            || (self.evidence.is_empty() && self.gaps.is_empty())
        {
            return Err(ProtocolError::new(ReasonCode::BatchTooLarge));
        }
        for value in [
            &self.batch_id,
            &self.project_id,
            &self.session_id,
            &self.emitter_id,
        ] {
            validate_required(value)?;
        }
        crate::validate_timestamp(&self.observed_at)?;
        for evidence in &self.evidence {
            evidence.validate()?;
        }
        for gap in &self.gaps {
            validate_required(gap)?;
        }
        Ok(())
    }
}

fn validate_required(value: &str) -> Result<(), ProtocolError> {
    validate_text(value, MAX_TEXT_BYTES)?;
    if value.is_empty() {
        return Err(ProtocolError::new(ReasonCode::InvalidEvent));
    }
    Ok(())
}

fn validate_required_all(values: &[&String]) -> Result<(), ProtocolError> {
    for value in values {
        validate_required(value)?;
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), ProtocolError> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ProtocolError::new(ReasonCode::InvalidArtifact));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_platform_has_a_strict_identity() {
        let platforms = vec![
            PlatformIdentity::Kubernetes {
                namespace: "commerce".into(),
                workload_kind: "deployment".into(),
                workload_name: "checkout".into(),
                pod_uid: "pod-1".into(),
                cluster: None,
                container: None,
            },
            PlatformIdentity::DockerCompose {
                project: "shop".into(),
                service: "api".into(),
                container_id: "container-1".into(),
            },
            PlatformIdentity::Ecs {
                cluster: "commerce".into(),
                task_arn: "arn:aws:ecs:region:account:task/1".into(),
                container: "api".into(),
            },
            PlatformIdentity::Serverless {
                provider: "aws-lambda".into(),
                function: "checkout".into(),
                region: None,
                instance: None,
            },
            PlatformIdentity::NativeService {
                operating_system: "linux".into(),
                service_manager: "systemd".into(),
                service: "checkout.service".into(),
                instance: None,
            },
            PlatformIdentity::Ci {
                provider: "github-actions".into(),
                pipeline: "run-1".into(),
                job: "test".into(),
                runner: None,
            },
            PlatformIdentity::Android {
                serial: "emulator-5554".into(),
                api_level: None,
                architecture: None,
                application_id: None,
            },
            PlatformIdentity::Ios {
                udid: "simulator-1".into(),
                runtime: None,
                device_type: None,
                bundle_id: None,
            },
        ];
        for platform in platforms {
            PlatformEvidence {
                version: PLATFORM_EVIDENCE_VERSION,
                collector: "reproit-platform".into(),
                platform,
                workload: None,
                build: None,
                resources: None,
                missing_capabilities: Vec::new(),
            }
            .validate()
            .unwrap();
        }
    }

    #[test]
    fn platform_batches_require_evidence_or_an_exact_gap() {
        let empty = PlatformEvidenceBatch {
            version: PLATFORM_EVIDENCE_BATCH_VERSION,
            batch_id: "platform_1".into(),
            project_id: "project_1".into(),
            session_id: "session_1".into(),
            emitter_id: "collector_1".into(),
            observed_at: "2026-08-03T00:00:00Z".into(),
            evidence: Vec::new(),
            gaps: Vec::new(),
        };
        assert!(empty.validate().is_err());
        let gap = PlatformEvidenceBatch {
            gaps: vec!["Kubernetes pod UID was not exposed by the downward API".into()],
            ..empty
        };
        gap.validate().unwrap();
    }

    #[test]
    fn platform_variant_fields_follow_the_camel_case_wire_contract() {
        let value = serde_json::to_value(PlatformIdentity::DockerCompose {
            project: "shop".into(),
            service: "api".into(),
            container_id: "container-1".into(),
        })
        .unwrap();
        assert_eq!(value["containerId"], "container-1");
        assert!(value.get("container_id").is_none());
    }
}
