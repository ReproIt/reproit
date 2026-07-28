//! Mapping from typed execution requirements into capsule cause summaries.

use super::CauseCategory;
use reproit_protocol::{RequirementKind, TriggerKind};

pub(super) fn cause_from_assessment(
    assessment: &reproit_protocol::CapabilityAssessment,
) -> Option<CauseCategory> {
    assessment
        .requirements
        .iter()
        .find(|requirement| requirement.level == reproit_protocol::RequirementLevel::Required)
        .map(|requirement| match &requirement.requirement {
            RequirementKind::Process { .. } => CauseCategory::ProcessLifecycle,
            RequirementKind::Trigger { trigger, .. } => match trigger {
                TriggerKind::UiAction => CauseCategory::UserAction,
                TriggerKind::HttpRequest | TriggerKind::RpcRequest => {
                    CauseCategory::HttpTransaction
                }
                TriggerKind::Command => CauseCategory::Command,
                TriggerKind::Message => CauseCategory::Message,
                TriggerKind::Timer => CauseCategory::TimerOrBackgroundEvent,
                TriggerKind::ProcessStartup | TriggerKind::Signal => {
                    CauseCategory::ProcessLifecycle
                }
                TriggerKind::Installer | TriggerKind::Upgrade => CauseCategory::InstallerOrUpgrade,
                TriggerKind::Migration => CauseCategory::Migration,
                TriggerKind::FilesystemEvent => CauseCategory::FilesystemEvent,
                TriggerKind::ResourcePressure => CauseCategory::ResourcePressure,
                TriggerKind::ConcurrencySchedule => CauseCategory::ConcurrencySchedule,
                TriggerKind::DeviceInteraction => CauseCategory::DeviceInteraction,
            },
            RequirementKind::State { .. }
            | RequirementKind::Dependency { .. }
            | RequirementKind::Environment { .. } => CauseCategory::EnvironmentChange,
            RequirementKind::Observation { .. } | RequirementKind::Debugger { .. } => {
                CauseCategory::Unclassified
            }
        })
}
