use crate::{valid_hash, validate_text, validate_token, DebuggerKind, ProtocolError, ReasonCode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CELL_RECEIPT_VERSION: u16 = 1;
pub const DIAGNOSTIC_RECEIPT_VERSION: u16 = 1;
pub const DEBUG_SESSION_VERSION: u16 = 1;
pub const MAX_CELL_SERVICES: usize = 64;
pub const MAX_RECEIPT_ENTRIES: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CleanupStatus {
    Verified,
    Attempted,
    Failed,
    NotAttempted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CellReceipt {
    pub version: u16,
    pub receipt_id: String,
    pub run_id: String,
    pub cell_id: String,
    pub driver: String,
    pub project_name: String,
    pub configuration_sha256: String,
    pub services: Vec<String>,
    pub state_fingerprints: BTreeMap<String, String>,
    pub cleanup: CleanupStatus,
    pub missing_capabilities: Vec<String>,
}

impl CellReceipt {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != CELL_RECEIPT_VERSION {
            return Err(ProtocolError::new(ReasonCode::UnsupportedVersion));
        }
        validate_token(&self.receipt_id)?;
        validate_token(&self.run_id)?;
        validate_token(&self.cell_id)?;
        validate_token(&self.driver)?;
        validate_token(&self.project_name)?;
        validate_digest(&self.configuration_sha256)?;
        validate_tokens(&self.services, MAX_CELL_SERVICES)?;
        validate_map(&self.state_fingerprints)?;
        validate_tokens(&self.missing_capabilities, MAX_RECEIPT_ENTRIES)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugEndpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceMapping {
    pub local_root: String,
    pub target_root: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticReceipt {
    pub version: u16,
    pub receipt_id: String,
    pub occurrence_id: String,
    pub run_id: String,
    pub cell_receipt_id: String,
    pub debugger: DebuggerKind,
    pub endpoint: DebugEndpoint,
    pub source_mappings: Vec<SourceMapping>,
    pub pause_point: String,
    pub perturbations: Vec<String>,
    pub authoritative: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DebugSessionState {
    Preparing,
    WaitingForDebugger,
    PausedBeforeTrigger,
    Triggering,
    Observing,
    Cleaning,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugSessionDescriptor {
    pub version: u16,
    pub session_id: String,
    pub occurrence_id: String,
    pub diagnostic_receipt_id: String,
    pub state: DebugSessionState,
    pub control_endpoint: DebugEndpoint,
    pub authorization_token: String,
    pub debugger: DebuggerKind,
    pub debugger_endpoint: DebugEndpoint,
    pub source_mappings: Vec<SourceMapping>,
    pub authoritative: bool,
}

impl DebugSessionDescriptor {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != DEBUG_SESSION_VERSION || self.authoritative {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        validate_token(&self.session_id)?;
        validate_token(&self.occurrence_id)?;
        validate_token(&self.diagnostic_receipt_id)?;
        validate_loopback_endpoint(&self.control_endpoint)?;
        validate_loopback_endpoint(&self.debugger_endpoint)?;
        if self.authorization_token.len() < 32
            || self.authorization_token.len() > 128
            || !self
                .authorization_token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        if self.source_mappings.len() > MAX_RECEIPT_ENTRIES {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        for mapping in &self.source_mappings {
            validate_text(&mapping.local_root, 4096)?;
            validate_text(&mapping.target_root, 4096)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DebugSessionCommand {
    Status,
    DebuggerAttached,
    ReplayTrigger,
    Stop,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugSessionRequest {
    pub version: u16,
    pub authorization_token: String,
    pub command: DebugSessionCommand,
}

impl DebugSessionRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != DEBUG_SESSION_VERSION
            || self.authorization_token.len() < 32
            || self.authorization_token.len() > 128
            || !self
                .authorization_token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugSessionResponse {
    pub version: u16,
    pub accepted: bool,
    pub state: DebugSessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl DiagnosticReceipt {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != DIAGNOSTIC_RECEIPT_VERSION || self.authoritative {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        validate_token(&self.receipt_id)?;
        validate_token(&self.occurrence_id)?;
        validate_token(&self.run_id)?;
        validate_token(&self.cell_receipt_id)?;
        validate_text(&self.endpoint.host, 255)?;
        if self.endpoint.host != "127.0.0.1" || self.endpoint.port == 0 {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        if self.source_mappings.len() > MAX_RECEIPT_ENTRIES {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        for mapping in &self.source_mappings {
            validate_text(&mapping.local_root, 4096)?;
            validate_text(&mapping.target_root, 4096)?;
        }
        validate_token(&self.pause_point)?;
        if self.perturbations.is_empty() {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        for perturbation in &self.perturbations {
            validate_text(perturbation, 1024)?;
        }
        Ok(())
    }
}

fn validate_digest(value: &str) -> Result<(), ProtocolError> {
    if valid_hash(value, 64) {
        Ok(())
    } else {
        Err(ProtocolError::new(ReasonCode::InvalidEvent))
    }
}

fn validate_loopback_endpoint(endpoint: &DebugEndpoint) -> Result<(), ProtocolError> {
    validate_text(&endpoint.host, 255)?;
    if endpoint.host != "127.0.0.1" || endpoint.port == 0 {
        return Err(ProtocolError::new(ReasonCode::InvalidEvent));
    }
    Ok(())
}

fn validate_tokens(values: &[String], maximum: usize) -> Result<(), ProtocolError> {
    if values.len() > maximum {
        return Err(ProtocolError::new(ReasonCode::InvalidEvent));
    }
    for value in values {
        validate_token(value)?;
    }
    Ok(())
}

fn validate_map(values: &BTreeMap<String, String>) -> Result<(), ProtocolError> {
    if values.len() > MAX_RECEIPT_ENTRIES {
        return Err(ProtocolError::new(ReasonCode::InvalidEvent));
    }
    for (key, value) in values {
        validate_token(key)?;
        validate_digest(value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_receipts_can_never_be_authoritative() {
        let receipt = DiagnosticReceipt {
            version: DIAGNOSTIC_RECEIPT_VERSION,
            receipt_id: "diag_1".into(),
            occurrence_id: "occ_1".into(),
            run_id: "run_1".into(),
            cell_receipt_id: "cell_1".into(),
            debugger: DebuggerKind::Gdb,
            endpoint: DebugEndpoint {
                host: "127.0.0.1".into(),
                port: 1234,
            },
            source_mappings: Vec::new(),
            pause_point: "before-trigger".into(),
            perturbations: vec!["debugger-attached".into()],
            authoritative: true,
        };
        assert!(receipt.validate().is_err());
    }

    #[test]
    fn debug_control_endpoints_are_loopback_and_non_authoritative() {
        let descriptor = DebugSessionDescriptor {
            version: DEBUG_SESSION_VERSION,
            session_id: "session_1".into(),
            occurrence_id: "occ_1".into(),
            diagnostic_receipt_id: "diag_1".into(),
            state: DebugSessionState::PausedBeforeTrigger,
            control_endpoint: DebugEndpoint {
                host: "0.0.0.0".into(),
                port: 9000,
            },
            authorization_token: "a".repeat(48),
            debugger: DebuggerKind::Gdb,
            debugger_endpoint: DebugEndpoint {
                host: "127.0.0.1".into(),
                port: 9001,
            },
            source_mappings: Vec::new(),
            authoritative: false,
        };
        assert!(descriptor.validate().is_err());
    }
}
