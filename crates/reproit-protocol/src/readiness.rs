//! Independent readiness and environment-fidelity decisions.

use crate::{validate_text, validate_token, ProtocolError, ReasonCode, MAX_TEXT_BYTES};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const READINESS_VERSION: u16 = 1;
pub const MAX_READINESS_GAPS: usize = 128;
pub const MAX_FIDELITY_CLAIMS: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadinessDimension {
    Capture,
    Replay,
    Debug,
    Verification,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadinessStatus {
    Ready,
    Blocked,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityGapReason {
    MissingEvidence,
    UnsupportedCollector,
    UnsupportedExecutor,
    Unauthorized,
    EnvironmentUnavailable,
    AmbiguousMapping,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityGap {
    pub capability: String,
    pub reason: CapabilityGapReason,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

impl CapabilityGap {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_token(&self.capability)?;
        validate_text(&self.detail, MAX_TEXT_BYTES)?;
        if self.detail.is_empty() {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        if let Some(action) = &self.next_action {
            validate_text(action, MAX_TEXT_BYTES)?;
            if action.is_empty() {
                return Err(ProtocolError::new(ReasonCode::InvalidEvent));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DimensionReadiness {
    pub dimension: ReadinessDimension,
    pub status: ReadinessStatus,
    #[serde(default)]
    pub gaps: Vec<CapabilityGap>,
}

impl DimensionReadiness {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.gaps.len() > MAX_READINESS_GAPS
            || (self.status == ReadinessStatus::Ready && !self.gaps.is_empty())
            || (self.status == ReadinessStatus::Blocked && self.gaps.is_empty())
        {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        let mut capabilities = BTreeSet::new();
        for gap in &self.gaps {
            gap.validate()?;
            if !capabilities.insert(gap.capability.as_str()) {
                return Err(ProtocolError::new(ReasonCode::InvalidEvent));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FidelityStatus {
    Exact,
    Compatible,
    Changed,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FidelityClaim {
    pub dimension: String,
    pub status: FidelityStatus,
    pub source_value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_value: Option<String>,
    pub evidence_source: String,
}

impl FidelityClaim {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_token(&self.dimension)?;
        validate_text(&self.source_value, MAX_TEXT_BYTES)?;
        validate_text(&self.evidence_source, MAX_TEXT_BYTES)?;
        if self.source_value.is_empty() || self.evidence_source.is_empty() {
            return Err(ProtocolError::new(ReasonCode::InvalidEvent));
        }
        if let Some(value) = &self.replay_value {
            validate_text(value, MAX_TEXT_BYTES)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadinessAssessment {
    pub version: u16,
    pub occurrence_id: String,
    pub dimensions: Vec<DimensionReadiness>,
    #[serde(default)]
    pub fidelity: Vec<FidelityClaim>,
}

impl ReadinessAssessment {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != READINESS_VERSION
            || self.dimensions.len() != 4
            || self.fidelity.len() > MAX_FIDELITY_CLAIMS
        {
            return Err(ProtocolError::new(ReasonCode::UnsupportedVersion));
        }
        validate_token(&self.occurrence_id)?;
        let mut dimensions = BTreeSet::new();
        for dimension in &self.dimensions {
            dimension.validate()?;
            if !dimensions.insert(dimension.dimension) {
                return Err(ProtocolError::new(ReasonCode::InvalidEvent));
            }
        }
        for claim in &self.fidelity {
            claim.validate()?;
        }
        Ok(())
    }

    pub fn dimension(&self, dimension: ReadinessDimension) -> &DimensionReadiness {
        self.dimensions
            .iter()
            .find(|candidate| candidate.dimension == dimension)
            .expect("validated readiness contains every dimension")
    }
}

pub fn assess_readiness(
    occurrence: &crate::OccurrenceEnvelope,
    assessment: &crate::CapabilityAssessment,
    plan: Option<&crate::ReproductionPlan>,
    debug_gaps: Vec<CapabilityGap>,
    fidelity: Vec<FidelityClaim>,
) -> Result<ReadinessAssessment, ProtocolError> {
    occurrence.validate()?;
    assessment.validate(occurrence)?;
    let replay_gaps = replay_gaps(assessment, plan);
    let replay_ready = replay_gaps.is_empty();
    let mut debug_gaps = debug_gaps;
    if !replay_ready {
        debug_gaps.push(CapabilityGap {
            capability: "replay-readiness".into(),
            reason: CapabilityGapReason::UnsupportedExecutor,
            detail: "debugging requires a complete replay plan".into(),
            next_action: Some("resolve the replay capability gaps first".into()),
        });
    }
    let dimensions = vec![
        DimensionReadiness {
            dimension: ReadinessDimension::Capture,
            status: ReadinessStatus::Ready,
            gaps: Vec::new(),
        },
        DimensionReadiness {
            dimension: ReadinessDimension::Replay,
            status: status(&replay_gaps),
            gaps: replay_gaps.clone(),
        },
        DimensionReadiness {
            dimension: ReadinessDimension::Debug,
            status: status(&debug_gaps),
            gaps: debug_gaps,
        },
        DimensionReadiness {
            dimension: ReadinessDimension::Verification,
            status: status(&replay_gaps),
            gaps: replay_gaps,
        },
    ];
    let readiness = ReadinessAssessment {
        version: READINESS_VERSION,
        occurrence_id: occurrence.occurrence_id.clone(),
        dimensions,
        fidelity,
    };
    readiness.validate()?;
    Ok(readiness)
}

fn replay_gaps(
    assessment: &crate::CapabilityAssessment,
    plan: Option<&crate::ReproductionPlan>,
) -> Vec<CapabilityGap> {
    let mut gaps = assessment
        .unresolved
        .iter()
        .map(|unresolved| CapabilityGap {
            capability: unresolved.requirement_id.clone(),
            reason: match unresolved.reason {
                crate::UnresolvedRequirementReason::MissingEvidence => {
                    CapabilityGapReason::MissingEvidence
                }
                crate::UnresolvedRequirementReason::UnsupportedCapability => {
                    CapabilityGapReason::UnsupportedExecutor
                }
                crate::UnresolvedRequirementReason::UnauthorizedDestination => {
                    CapabilityGapReason::Unauthorized
                }
                crate::UnresolvedRequirementReason::EnvironmentUnavailable => {
                    CapabilityGapReason::EnvironmentUnavailable
                }
                crate::UnresolvedRequirementReason::AmbiguousMapping => {
                    CapabilityGapReason::AmbiguousMapping
                }
            },
            detail: unresolved.detail.clone(),
            next_action: None,
        })
        .collect::<Vec<_>>();
    if assessment.status == crate::AssessmentStatus::Eligible && plan.is_none() {
        gaps.push(CapabilityGap {
            capability: "execution-plan".into(),
            reason: CapabilityGapReason::UnsupportedExecutor,
            detail: "no trusted local execution plan has been resolved".into(),
            next_action: Some("run this occurrence from the application checkout".into()),
        });
    }
    gaps
}

fn status(gaps: &[CapabilityGap]) -> ReadinessStatus {
    if gaps.is_empty() {
        ReadinessStatus::Ready
    } else {
        ReadinessStatus::Blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_dimensions_cannot_hide_capability_gaps() {
        let dimension = DimensionReadiness {
            dimension: ReadinessDimension::Debug,
            status: ReadinessStatus::Ready,
            gaps: vec![CapabilityGap {
                capability: "workload-identity".into(),
                reason: CapabilityGapReason::MissingEvidence,
                detail: "missing".into(),
                next_action: None,
            }],
        };
        assert!(dimension.validate().is_err());
    }
}
