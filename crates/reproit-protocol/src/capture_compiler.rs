//! Deterministic capture-to-occurrence compilation.
//!
//! This stage derives evidence requirements only. It never chooses executable
//! commands or grants a provider authority.

use crate::{
    AssessmentStatus, CapabilityAssessment, CaptureBatch, CaptureCapabilityKind,
    CaptureCompleteness, CaptureDefect, CaptureDefectKind, CaptureEventKind, CapturedValue,
    CollectionMethod, DependencyKind, EnvironmentKind, EvidenceSource, FailureObservation,
    ObservationAuthority, OccurrenceEnvelope, ProtocolError, ReasonCode, ReproductionRequirement,
    RequirementKind, RequirementLevel, StateKind, SubjectIdentity, TriggerKind,
    UnresolvedRequirement, UnresolvedRequirementReason, OCCURRENCE_VERSION,
};
use std::collections::{BTreeMap, BTreeSet};

mod policy;
use policy::*;

pub struct CaptureCompilation {
    pub occurrence: OccurrenceEnvelope,
    pub assessment: CapabilityAssessment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureAssessmentScope {
    SourceEnvironment,
    Portable,
}

pub fn compile_capture_failure(
    batch: &CaptureBatch,
    received_at: &str,
    scope: CaptureAssessmentScope,
) -> Result<Option<CaptureCompilation>, ProtocolError> {
    batch.validate()?;
    let observations = failure_observations(batch);
    if observations.is_empty() {
        return Ok(None);
    }
    let occurrence_id = occurrence_id(batch)?;
    let occurrence = OccurrenceEnvelope {
        version: OCCURRENCE_VERSION,
        occurrence_id: occurrence_id.clone(),
        source: EvidenceSource::ReproitCapture,
        subject: SubjectIdentity {
            product: batch.project_id.clone(),
            component: batch.emitter.component.clone(),
            platform: batch.emitter.runtime.clone(),
        },
        observed_at: batch.observed_at.clone(),
        received_at: received_at.to_string(),
        deployment: batch.deployment.clone(),
        observations,
        artifacts: batch
            .artifacts
            .iter()
            .cloned()
            .map(|mut artifact| {
                artifact.collection = CollectionMethod::FlightRecorder;
                artifact
            })
            .collect(),
        capture_defects: capture_defects(batch),
        policy: batch.policy.clone(),
    };
    occurrence.validate()?;
    let assessment = assess(batch, &occurrence_id, scope);
    assessment.validate(&occurrence)?;
    Ok(Some(CaptureCompilation {
        occurrence,
        assessment,
    }))
}

fn occurrence_id(batch: &CaptureBatch) -> Result<String, ProtocolError> {
    let bytes =
        serde_json::to_vec(batch).map_err(|_| ProtocolError::new(ReasonCode::InvalidEvent))?;
    Ok(crate::derive_occurrence_id("capture-batch-v1", &bytes))
}

fn failure_observations(batch: &CaptureBatch) -> Vec<FailureObservation> {
    batch
        .events
        .iter()
        .filter_map(|event| {
            let CaptureEventKind::Observation { failure } = &event.event else {
                return None;
            };
            Some(FailureObservation {
                kind: failure.observation,
                authority: failure.authority,
                summary: failure.summary.clone(),
                signature: failure.signature.clone(),
                observation_point: failure.observation_point.clone(),
                artifact_ids: failure.artifact_ids.clone(),
            })
        })
        .collect()
}

fn capture_defects(batch: &CaptureBatch) -> Vec<CaptureDefect> {
    let mut defects = Vec::new();
    for event in &batch.events {
        if let CaptureEventKind::Defect {
            defect,
            detail,
            artifact_id,
        } = &event.event
        {
            defects.push(CaptureDefect {
                kind: *defect,
                detail: detail.clone(),
                artifact_id: artifact_id.clone(),
            });
        }
    }
    for capability in &batch.capabilities {
        let kind = match capability.completeness {
            CaptureCompleteness::Complete => continue,
            CaptureCompleteness::Partial => CaptureDefectKind::Truncated,
            CaptureCompleteness::Unavailable => CaptureDefectKind::Unavailable,
        };
        defects.push(CaptureDefect {
            kind,
            detail: format!(
                "{:?}: {}",
                capability.capability,
                capability.detail.as_deref().unwrap_or("unavailable")
            ),
            artifact_id: None,
        });
    }
    defects
}

fn assess(
    batch: &CaptureBatch,
    occurrence_id: &str,
    scope: CaptureAssessmentScope,
) -> CapabilityAssessment {
    let capabilities = batch
        .capabilities
        .iter()
        .map(|capability| (capability.capability, capability.completeness))
        .collect::<BTreeMap<_, _>>();
    let inputs = batch
        .events
        .iter()
        .filter_map(|event| match &event.event {
            CaptureEventKind::Input { name, value } => Some((name.as_str(), value)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut requirements = Vec::new();
    let mut unresolved = Vec::new();
    let mut seen = BTreeSet::new();
    let mut requirement_number = 1usize;

    for event in &batch.events {
        match &event.event {
            CaptureEventKind::Trigger {
                trigger,
                subject,
                value,
            } => {
                let key = format!("trigger:{trigger:?}:{subject}");
                if !seen.insert(key) {
                    continue;
                }
                let mut artifacts = inputs
                    .iter()
                    .filter_map(|(_, value)| artifact_id(value))
                    .collect::<Vec<_>>();
                if let Some(id) = value.as_ref().and_then(artifact_id) {
                    artifacts.push(id);
                }
                artifacts.sort();
                artifacts.dedup();
                let id = requirement_id(requirement_number, "trigger");
                requirement_number += 1;
                requirements.push(ReproductionRequirement {
                    id: id.clone(),
                    level: RequirementLevel::Required,
                    requirement: RequirementKind::Trigger {
                        trigger: *trigger,
                        subject: subject.clone(),
                    },
                    evidence_artifact_ids: artifacts,
                });
                if !capability_complete(&capabilities, trigger_capability(*trigger)) {
                    unresolved.push(missing_capability(id.clone(), trigger_capability(*trigger)));
                } else if (trigger_requires_value(*trigger) && value.is_none())
                    || value.as_ref().is_some_and(structural_only)
                {
                    unresolved.push(missing_evidence(
                        id.clone(),
                        "replayable trigger value or construction recipe",
                    ));
                } else if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    unresolved.push(environment_bound_requirement(id.clone()));
                }
                for (name, input) in &inputs {
                    if structural_only(input) {
                        unresolved.push(missing_evidence(
                            id.clone(),
                            &format!("replayable input {name}"),
                        ));
                    } else if environment_bound(input, scope) {
                        unresolved.push(environment_bound_requirement(id.clone()));
                    }
                }
            }
            CaptureEventKind::StateAccess {
                state,
                subject,
                value,
                ..
            } => {
                let key = format!("state:{state:?}:{subject}");
                if !seen.insert(key) {
                    continue;
                }
                let id = requirement_id(requirement_number, "state");
                requirement_number += 1;
                requirements.push(ReproductionRequirement {
                    id: id.clone(),
                    level: RequirementLevel::Required,
                    requirement: RequirementKind::State {
                        state: *state,
                        subject: subject.clone(),
                    },
                    evidence_artifact_ids: value
                        .as_ref()
                        .and_then(artifact_id)
                        .into_iter()
                        .collect(),
                });
                if value.is_none() {
                    unresolved.push(missing_evidence(id, "state value or construction recipe"));
                } else if value.as_ref().is_some_and(structural_only) {
                    unresolved.push(missing_evidence(
                        id,
                        "replayable state value or construction recipe",
                    ));
                } else if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    unresolved.push(environment_bound_requirement(id));
                } else if !capability_complete(&capabilities, state_capability(*state)) {
                    unresolved.push(missing_capability(id, state_capability(*state)));
                }
            }
            CaptureEventKind::EnvironmentRead {
                environment,
                subject,
                value,
            } => {
                let key = format!("environment:{environment:?}:{subject}");
                if !seen.insert(key) {
                    continue;
                }
                let id = requirement_id(requirement_number, "environment");
                requirement_number += 1;
                let artifact = value.as_ref().and_then(artifact_id);
                requirements.push(ReproductionRequirement {
                    id: id.clone(),
                    level: RequirementLevel::Required,
                    requirement: RequirementKind::Environment {
                        environment: *environment,
                        required_value: deterministic_required_value(subject, value.as_ref()),
                    },
                    evidence_artifact_ids: artifact.clone().into_iter().collect(),
                });
                if value.is_none() {
                    unresolved.push(missing_evidence(id, "deterministic environment value"));
                } else if value.as_ref().is_some_and(structural_only) {
                    unresolved.push(missing_evidence(id, "replayable deterministic value"));
                } else if requires_stream_artifact(*environment) && artifact.is_none() {
                    unresolved.push(missing_evidence(id, "artifact-backed deterministic stream"));
                } else if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    unresolved.push(environment_bound_requirement(id));
                } else if !capability_complete(&capabilities, environment_capability(*environment))
                {
                    unresolved.push(missing_capability(id, environment_capability(*environment)));
                }
            }
            CaptureEventKind::Dependency {
                system,
                subject,
                value,
                ..
            } => {
                let key = format!("dependency:{system}:{subject}");
                if !seen.insert(key) {
                    continue;
                }
                let id = requirement_id(requirement_number, "dependency");
                requirement_number += 1;
                let dependency = if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    DependencyKind::EnvironmentBound
                } else {
                    DependencyKind::CapturedReplay
                };
                requirements.push(ReproductionRequirement {
                    id: id.clone(),
                    level: RequirementLevel::Required,
                    requirement: RequirementKind::Dependency {
                        dependency,
                        subject: format!("{system}:{subject}"),
                    },
                    evidence_artifact_ids: value
                        .as_ref()
                        .and_then(artifact_id)
                        .into_iter()
                        .collect(),
                });
                if value.is_none() {
                    unresolved.push(missing_evidence(id, "dependency result"));
                } else if value.as_ref().is_some_and(structural_only) {
                    unresolved.push(missing_evidence(id, "replayable dependency result"));
                } else if value
                    .as_ref()
                    .is_some_and(|value| environment_bound(value, scope))
                {
                    unresolved.push(environment_bound_requirement(id));
                } else if !capability_complete(&capabilities, CaptureCapabilityKind::Network) {
                    unresolved.push(missing_capability(id, CaptureCapabilityKind::Network));
                }
            }
            CaptureEventKind::Effect {
                effect, subject, ..
            } if is_ambiguous_boundary_effect(effect) => {
                let key = format!("ambiguous-effect:{effect}:{subject}");
                if !seen.insert(key) {
                    continue;
                }
                let id = requirement_id(requirement_number, "typed-boundary");
                requirement_number += 1;
                requirements.push(ReproductionRequirement {
                    id: id.clone(),
                    level: RequirementLevel::Required,
                    requirement: RequirementKind::Dependency {
                        dependency: DependencyKind::CapturedReplay,
                        subject: format!("{effect}:{subject}"),
                    },
                    evidence_artifact_ids: vec![],
                });
                unresolved.push(UnresolvedRequirement {
                    requirement_id: id,
                    reason: UnresolvedRequirementReason::AmbiguousMapping,
                    detail: "a dependency or state boundary used an untyped effect event".into(),
                });
            }
            _ => {}
        }
    }

    let has_trigger = requirements
        .iter()
        .any(|requirement| matches!(requirement.requirement, RequirementKind::Trigger { .. }));
    if !has_trigger {
        if let Some(process) = batch.events.iter().find_map(|event| match &event.event {
            CaptureEventKind::ProcessStart { process, .. } => Some(process),
            _ => None,
        }) {
            let id = requirement_id(requirement_number, "startup");
            requirement_number += 1;
            requirements.push(ReproductionRequirement {
                id: id.clone(),
                level: RequirementLevel::Required,
                requirement: RequirementKind::Trigger {
                    trigger: TriggerKind::ProcessStartup,
                    subject: process.executable.clone(),
                },
                evidence_artifact_ids: vec![],
            });
            if !capability_complete(&capabilities, CaptureCapabilityKind::ProcessTree) {
                unresolved.push(missing_capability(id, CaptureCapabilityKind::ProcessTree));
            }
        } else {
            let id = requirement_id(requirement_number, "trigger");
            requirement_number += 1;
            requirements.push(ReproductionRequirement {
                id: id.clone(),
                level: RequirementLevel::Required,
                requirement: RequirementKind::Trigger {
                    trigger: TriggerKind::ProcessStartup,
                    subject: batch.emitter.component.clone(),
                },
                evidence_artifact_ids: vec![],
            });
            unresolved.push(UnresolvedRequirement {
                requirement_id: id,
                reason: UnresolvedRequirementReason::MissingEvidence,
                detail: "the capture does not identify the action that starts the failure".into(),
            });
        }
    }

    let oracle = runtime_oracle(batch);
    let oracle_environment_bound = oracle.is_some_and(|failure| {
        failure
            .artifact_ids
            .iter()
            .any(|artifact_id| artifact_is_environment_bound(batch, artifact_id, scope))
    });
    if oracle.is_none() || oracle_environment_bound {
        let id = requirement_id(requirement_number, "oracle");
        requirement_number += 1;
        requirements.push(ReproductionRequirement {
            id: id.clone(),
            level: RequirementLevel::Required,
            requirement: RequirementKind::Observation {
                observation: oracle
                    .map(|failure| failure.observation)
                    .or_else(|| {
                        batch.events.iter().find_map(|event| match &event.event {
                            CaptureEventKind::Observation { failure } => Some(failure.observation),
                            _ => None,
                        })
                    })
                    .unwrap_or(crate::ObservationKind::Diagnostic),
                subject: batch.emitter.component.clone(),
            },
            evidence_artifact_ids: oracle
                .into_iter()
                .flat_map(|failure| failure.artifact_ids.iter().cloned())
                .collect(),
        });
        let unresolved_requirement = if oracle_environment_bound {
            environment_bound_requirement(id)
        } else {
            UnresolvedRequirement {
                requirement_id: id,
                reason: UnresolvedRequirementReason::MissingEvidence,
                detail: "the capture does not contain a runtime oracle with a stable signature"
                    .into(),
            }
        };
        unresolved.push(unresolved_requirement);
    }

    if !has_determinism_envelope(batch) {
        let id = requirement_id(requirement_number, "envelope");
        requirement_number += 1;
        requirements.push(ReproductionRequirement {
            id: id.clone(),
            level: RequirementLevel::Required,
            requirement: RequirementKind::Environment {
                environment: EnvironmentKind::Runtime,
                required_value: None,
            },
            evidence_artifact_ids: vec![],
        });
        unresolved.push(UnresolvedRequirement {
            requirement_id: id,
            reason: UnresolvedRequirementReason::MissingEvidence,
            detail: "the capture does not contain a valid determinism envelope".into(),
        });
    }

    let explicit_defects = batch
        .events
        .iter()
        .filter(|event| matches!(event.event, CaptureEventKind::Defect { .. }));
    for event in explicit_defects {
        let CaptureEventKind::Defect { detail, .. } = &event.event else {
            continue;
        };
        let id = requirement_id(requirement_number, "capture-integrity");
        requirement_number += 1;
        requirements.push(ReproductionRequirement {
            id: id.clone(),
            level: RequirementLevel::Required,
            requirement: RequirementKind::Environment {
                environment: EnvironmentKind::Runtime,
                required_value: None,
            },
            evidence_artifact_ids: vec![],
        });
        unresolved.push(UnresolvedRequirement {
            requirement_id: id,
            reason: UnresolvedRequirementReason::MissingEvidence,
            detail: format!("the producer reported a capture defect: {detail}"),
        });
    }

    let status = if unresolved.is_empty() {
        AssessmentStatus::Eligible
    } else if unresolved
        .iter()
        .any(|item| item.reason == UnresolvedRequirementReason::UnauthorizedDestination)
    {
        AssessmentStatus::EnvironmentBound
    } else {
        AssessmentStatus::Incomplete
    };
    CapabilityAssessment {
        occurrence_id: occurrence_id.to_string(),
        status,
        requirements,
        unresolved,
    }
}

#[cfg(test)]
mod tests;
