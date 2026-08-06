use super::*;

pub(super) fn runtime_oracle(batch: &CaptureBatch) -> Option<&crate::FailureRecord> {
    batch.events.iter().find_map(|event| {
        let CaptureEventKind::Observation { failure } = &event.event else {
            return None;
        };
        (failure.authority != ObservationAuthority::SourceClaim
            && failure
                .signature
                .as_deref()
                .is_some_and(|value| !value.is_empty()))
        .then_some(failure)
    })
}

pub(super) fn has_determinism_envelope(batch: &CaptureBatch) -> bool {
    batch.events.iter().any(|event| {
        let CaptureEventKind::Checkpoint { name, attributes } = &event.event else {
            return false;
        };
        if name != "determinism-envelope" {
            return false;
        }
        let Some(values) = attributes.as_object() else {
            return false;
        };
        let observed_at = values
            .get("observedAtMs")
            .and_then(serde_json::Value::as_i64);
        let replay_seed = values.get("replaySeed");
        let timezone = values.get("tz").and_then(serde_json::Value::as_str);
        observed_at.is_some_and(|value| value >= 0)
            && replay_seed.is_some_and(valid_replay_seed)
            && timezone.is_none_or(|value| !value.trim().is_empty())
    })
}

pub(super) fn valid_replay_seed(value: &serde_json::Value) -> bool {
    value
        .as_str()
        .is_some_and(|seed| !seed.is_empty() && seed.len() <= 128)
        || value.as_u64().is_some()
}

pub(super) fn artifact_is_environment_bound(
    batch: &CaptureBatch,
    artifact_id: &str,
    scope: CaptureAssessmentScope,
) -> bool {
    scope == CaptureAssessmentScope::Portable
        && batch.artifacts.iter().any(|artifact| {
            artifact.id == artifact_id && artifact.policy != crate::ArtifactPolicy::Exportable
        })
}

pub(super) fn is_ambiguous_boundary_effect(effect: &str) -> bool {
    matches!(
        effect,
        "read"
            | "write"
            | "delete"
            | "call"
            | "return"
            | "publish"
            | "consume"
            | "connect"
            | "disconnect"
    )
}

pub(super) fn requirement_id(number: usize, suffix: &str) -> String {
    format!("req_{number:03}_{suffix}")
}

pub(super) fn artifact_id(value: &CapturedValue) -> Option<String> {
    match value {
        CapturedValue::Artifact { artifact_id, .. } => Some(artifact_id.clone()),
        CapturedValue::Structural { .. }
        | CapturedValue::Replayable { .. }
        | CapturedValue::EnvironmentBound { .. } => None,
    }
}

pub(super) fn environment_bound(value: &CapturedValue, scope: CaptureAssessmentScope) -> bool {
    match value {
        CapturedValue::EnvironmentBound { .. } => true,
        CapturedValue::Artifact { policy, .. } => {
            scope == CaptureAssessmentScope::Portable
                && *policy != crate::ArtifactPolicy::Exportable
        }
        CapturedValue::Structural { .. } | CapturedValue::Replayable { .. } => false,
    }
}

pub(super) fn structural_only(value: &CapturedValue) -> bool {
    matches!(value, CapturedValue::Structural { .. })
}

pub(super) fn deterministic_required_value(
    subject: &str,
    value: Option<&CapturedValue>,
) -> Option<String> {
    match value {
        Some(CapturedValue::Artifact { artifact_id, .. }) => {
            Some(format!("artifact:{artifact_id}"))
        }
        Some(CapturedValue::Replayable { value, .. })
            if value.is_string() || value.is_number() || value.is_boolean() =>
        {
            let encoded = value.to_string();
            (encoded.len() <= crate::MAX_TEXT_BYTES).then(|| format!("{subject}:{encoded}"))
        }
        _ => Some(subject.to_string()),
    }
}

pub(super) fn requires_stream_artifact(environment: EnvironmentKind) -> bool {
    matches!(
        environment,
        EnvironmentKind::Clock
            | EnvironmentKind::WallClock
            | EnvironmentKind::MonotonicClock
            | EnvironmentKind::Randomness
            | EnvironmentKind::RandomBytes
    )
}

pub(super) fn capability_complete(
    capabilities: &BTreeMap<CaptureCapabilityKind, CaptureCompleteness>,
    required: CaptureCapabilityKind,
) -> bool {
    capabilities.get(&required) == Some(&CaptureCompleteness::Complete)
}

pub(super) fn trigger_capability(trigger: TriggerKind) -> CaptureCapabilityKind {
    match trigger {
        TriggerKind::UiAction => CaptureCapabilityKind::UserInterface,
        TriggerKind::HttpRequest => CaptureCapabilityKind::Http,
        TriggerKind::RpcRequest => CaptureCapabilityKind::Rpc,
        TriggerKind::Command
        | TriggerKind::Installer
        | TriggerKind::Upgrade
        | TriggerKind::Migration => CaptureCapabilityKind::Commands,
        TriggerKind::Message => CaptureCapabilityKind::Queue,
        TriggerKind::Timer => CaptureCapabilityKind::Timers,
        TriggerKind::ProcessStartup | TriggerKind::Signal => CaptureCapabilityKind::ProcessTree,
        TriggerKind::FilesystemEvent => CaptureCapabilityKind::Filesystem,
        TriggerKind::ResourcePressure => CaptureCapabilityKind::ResourcePressure,
        TriggerKind::ConcurrencySchedule => CaptureCapabilityKind::Concurrency,
        TriggerKind::DeviceInteraction => CaptureCapabilityKind::Device,
    }
}

pub(super) fn trigger_requires_value(trigger: TriggerKind) -> bool {
    matches!(
        trigger,
        TriggerKind::UiAction
            | TriggerKind::HttpRequest
            | TriggerKind::RpcRequest
            | TriggerKind::Message
            | TriggerKind::Installer
            | TriggerKind::Upgrade
            | TriggerKind::Migration
            | TriggerKind::FilesystemEvent
            | TriggerKind::ConcurrencySchedule
            | TriggerKind::DeviceInteraction
    )
}

pub(super) fn state_capability(state: StateKind) -> CaptureCapabilityKind {
    match state {
        StateKind::Filesystem => CaptureCapabilityKind::Filesystem,
        StateKind::Registry => CaptureCapabilityKind::Environment,
        StateKind::Database => CaptureCapabilityKind::Database,
        StateKind::Cache => CaptureCapabilityKind::Cache,
        StateKind::Queue => CaptureCapabilityKind::Queue,
        StateKind::ObjectStore => CaptureCapabilityKind::ObjectStore,
        StateKind::ApplicationStorage => CaptureCapabilityKind::Filesystem,
        StateKind::Device => CaptureCapabilityKind::Device,
    }
}

pub(super) fn environment_capability(environment: EnvironmentKind) -> CaptureCapabilityKind {
    match environment {
        EnvironmentKind::Clock | EnvironmentKind::WallClock | EnvironmentKind::MonotonicClock => {
            CaptureCapabilityKind::Clock
        }
        EnvironmentKind::Randomness
        | EnvironmentKind::RandomSeed
        | EnvironmentKind::RandomBytes => CaptureCapabilityKind::Randomness,
        _ => CaptureCapabilityKind::Environment,
    }
}

pub(super) fn missing_capability(
    requirement_id: String,
    capability: CaptureCapabilityKind,
) -> UnresolvedRequirement {
    UnresolvedRequirement {
        requirement_id,
        reason: UnresolvedRequirementReason::MissingEvidence,
        detail: format!("capture capability {capability:?} was not complete"),
    }
}

pub(super) fn missing_evidence(requirement_id: String, evidence: &str) -> UnresolvedRequirement {
    UnresolvedRequirement {
        requirement_id,
        reason: UnresolvedRequirementReason::MissingEvidence,
        detail: format!("capture did not retain required {evidence}"),
    }
}

pub(super) fn environment_bound_requirement(requirement_id: String) -> UnresolvedRequirement {
    UnresolvedRequirement {
        requirement_id,
        reason: UnresolvedRequirementReason::UnauthorizedDestination,
        detail: "required evidence is restricted to its source environment".into(),
    }
}
