//! Replay application for the shared environment-minimization proof.

use super::Capsule;
pub use reproit_protocol::{EnvironmentEnvelope, EnvironmentOutcome, EnvironmentTrial};
use std::collections::BTreeSet;

impl Capsule {
    /// Apply the minimized environment envelope to a replay launch. Required
    /// dimensions are pinned to their captured values; relaxed dimensions are
    /// omitted from both config-level and caller-provided defines.
    pub fn apply_replay_environment(
        &self,
        defines: &mut Vec<(String, String)>,
        excluded_defines: &mut BTreeSet<String>,
    ) {
        for (dimension, value) in &self.environment {
            let Some(name) = dimension.strip_prefix("define:") else {
                continue;
            };
            if self
                .environment_envelope
                .relaxed_dimensions
                .contains(dimension)
            {
                if !defines.iter().any(|(defined, _)| defined == name) {
                    excluded_defines.insert(name.to_string());
                }
            } else if !defines.iter().any(|(defined, _)| defined == name) {
                defines.push((name.to_string(), value.clone()));
            }
        }
        defines.retain(|(name, _)| !excluded_defines.contains(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsule::FindingIdentity;

    fn capsule() -> Capsule {
        Capsule::new(
            "app",
            FindingIdentity {
                oracle: "crash".into(),
                invariant: "no-exception".into(),
                kind: "exception".into(),
                message: "boom".into(),
                frame: String::new(),
                trigger: String::new(),
                boundary: None,
            },
        )
    }

    #[test]
    fn replay_pins_required_defines_and_omits_relaxed_config_defines() {
        let mut capsule = capsule();
        capsule
            .environment
            .insert("define:CHECKOUT_V2".into(), "true".into());
        capsule
            .environment
            .insert("define:COLOR_MODE".into(), "dark".into());
        capsule
            .environment_envelope
            .relaxed_dimensions
            .insert("define:COLOR_MODE".into());
        let mut defines = Vec::new();
        let mut excluded = BTreeSet::new();

        capsule.apply_replay_environment(&mut defines, &mut excluded);

        assert_eq!(defines, vec![("CHECKOUT_V2".into(), "true".into())]);
        assert_eq!(excluded, BTreeSet::from(["COLOR_MODE".into()]));
    }

    #[test]
    fn explicit_replay_override_wins_over_the_recorded_envelope() {
        let mut capsule = capsule();
        capsule
            .environment
            .insert("define:REPROIT_LOCALE".into(), "tr".into());
        capsule
            .environment_envelope
            .relaxed_dimensions
            .insert("define:REPROIT_LOCALE".into());
        let mut defines = vec![("REPROIT_LOCALE".into(), "de".into())];
        let mut excluded = BTreeSet::new();

        capsule.apply_replay_environment(&mut defines, &mut excluded);

        assert_eq!(defines, vec![("REPROIT_LOCALE".into(), "de".into())]);
        assert!(excluded.is_empty());
    }
}
