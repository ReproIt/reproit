//! Conservative aggregation of explicit oracle evidence.

use serde::Serialize;
use serde_json::Value;

pub use reproit_protocol::EvaluationStatus as EvidenceStatus;

/// Oracle families whose findings are already represented by an explicit
/// tri-state runner marker parsed by `EvidenceCounts::from_log`.
pub fn has_explicit_status_marker(oracle: &str) -> bool {
    matches!(oracle, "detached-indicator" | "accessibility-state")
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct EvidenceCounts {
    #[serde(rename = "VIOLATION")]
    pub violation: u64,
    #[serde(rename = "SATISFIED")]
    pub satisfied: u64,
    #[serde(rename = "ABSTAIN")]
    pub abstain: u64,
    pub evaluated: u64,
}

impl EvidenceCounts {
    pub fn from_log(log: &str) -> Self {
        let mut counts = Self::default();
        for event in crate::model::runner::parse(log) {
            let crate::model::runner::RunnerEvent::Explore(line) = event else {
                continue;
            };
            let payload = ["EXPLORE:RELATIONSTATUS ", "EXPLORE:A11YSTATESTATUS "]
                .iter()
                .find_map(|marker| line.strip_prefix(marker));
            let Some(payload) = payload else {
                continue;
            };
            let Some(outcome) = serde_json::from_str::<Value>(payload)
                .ok()
                .and_then(|value| validated_marker_outcome(&value).map(str::to_owned))
            else {
                continue;
            };
            counts.observe(&outcome);
        }
        counts
    }

    /// Add violations from finding families that do not emit a status marker.
    /// Marker-backed findings must be excluded by the caller to avoid counting
    /// the same evaluation both here and in `from_log`.
    pub fn observe_unreported_violations(&mut self, count: usize) {
        let count = u64::try_from(count).unwrap_or(u64::MAX);
        self.violation = self.violation.saturating_add(count);
        self.evaluated = self.evaluated.saturating_add(count);
    }

    pub fn merge(&mut self, other: &Self) {
        self.violation = self.violation.saturating_add(other.violation);
        self.satisfied = self.satisfied.saturating_add(other.satisfied);
        self.abstain = self.abstain.saturating_add(other.abstain);
        self.evaluated = self.evaluated.saturating_add(other.evaluated);
    }

    pub fn status(&self, complete: bool) -> EvidenceStatus {
        if self.violation > 0 {
            EvidenceStatus::Violation
        } else if !complete || self.abstain > 0 || self.evaluated == 0 {
            EvidenceStatus::Abstain
        } else {
            EvidenceStatus::Satisfied
        }
    }

    fn observe(&mut self, outcome: &str) {
        match outcome {
            "VIOLATION" => self.violation = self.violation.saturating_add(1),
            "SATISFIED" => self.satisfied = self.satisfied.saturating_add(1),
            "ABSTAIN" => self.abstain = self.abstain.saturating_add(1),
            _ => return,
        }
        self.evaluated = self.evaluated.saturating_add(1);
    }
}

fn validated_marker_outcome(value: &Value) -> Option<&str> {
    let sig = value.get("sig")?.as_str()?;
    let reported = value.get("outcome")?.as_str()?;
    let checks = value.get("checks")?.as_array()?;
    if sig.is_empty() || !matches!(reported, "VIOLATION" | "SATISFIED" | "ABSTAIN") {
        return None;
    }
    let mut derived = if checks.is_empty() {
        "ABSTAIN"
    } else {
        "SATISFIED"
    };
    for check in checks {
        match check.get("outcome").and_then(Value::as_str)? {
            "VIOLATION" => derived = "VIOLATION",
            "ABSTAIN" if derived != "VIOLATION" => derived = "ABSTAIN",
            "SATISFIED" => {}
            _ => return None,
        }
    }
    (reported == derived).then_some(reported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_authority_abstains() {
        let counts = EvidenceCounts::from_log("EXPLORE:STATE {}\n");
        assert_eq!(counts.evaluated, 0);
        assert_eq!(counts.status(true), EvidenceStatus::Abstain);
    }

    #[test]
    fn all_satisfied_is_satisfied() {
        let log = concat!(
            "EXPLORE:RELATIONSTATUS {\"sig\":\"a\",\"outcome\":\"SATISFIED\",",
            "\"checks\":[{\"outcome\":\"SATISFIED\"}]}\n",
            "EXPLORE:A11YSTATESTATUS {\"sig\":\"b\",\"outcome\":\"SATISFIED\",",
            "\"checks\":[{\"outcome\":\"SATISFIED\"}]}\n",
        );
        let counts = EvidenceCounts::from_log(log);
        assert_eq!(counts.satisfied, 2);
        assert_eq!(counts.status(true), EvidenceStatus::Satisfied);
    }

    #[test]
    fn abstain_wins_over_satisfied() {
        let log = concat!(
            "EXPLORE:RELATIONSTATUS {\"sig\":\"a\",\"outcome\":\"SATISFIED\",",
            "\"checks\":[{\"outcome\":\"SATISFIED\"}]}\n",
            "EXPLORE:A11YSTATESTATUS {\"sig\":\"b\",\"outcome\":\"ABSTAIN\",",
            "\"checks\":[]}\n",
        );
        assert_eq!(
            EvidenceCounts::from_log(log).status(true),
            EvidenceStatus::Abstain
        );
    }

    #[test]
    fn violation_has_precedence() {
        let log = concat!(
            "EXPLORE:RELATIONSTATUS {\"sig\":\"a\",\"outcome\":\"VIOLATION\",",
            "\"checks\":[{\"outcome\":\"VIOLATION\"}]}\n",
            "EXPLORE:A11YSTATESTATUS {\"sig\":\"b\",\"outcome\":\"ABSTAIN\",",
            "\"checks\":[]}\n",
        );
        assert_eq!(
            EvidenceCounts::from_log(log).status(false),
            EvidenceStatus::Violation
        );
    }

    #[test]
    fn incomplete_all_satisfied_abstains() {
        let counts = EvidenceCounts::from_log(concat!(
            "EXPLORE:RELATIONSTATUS {\"sig\":\"a\",\"outcome\":\"SATISFIED\",",
            "\"checks\":[{\"outcome\":\"SATISFIED\"}]}\n",
        ));
        assert_eq!(counts.status(false), EvidenceStatus::Abstain);
    }

    #[test]
    fn unknown_or_malformed_outcomes_do_not_become_evidence() {
        let log = concat!(
            "EXPLORE:RELATIONSTATUS {\"sig\":\"a\",\"outcome\":\"PASS\",",
            "\"checks\":[]}\n",
            "EXPLORE:A11YSTATESTATUS not-json\n",
        );
        assert_eq!(EvidenceCounts::from_log(log), EvidenceCounts::default());
    }

    #[test]
    fn inconsistent_marker_abstains_instead_of_claiming_satisfaction() {
        let log = concat!(
            "EXPLORE:RELATIONSTATUS {\"sig\":\"a\",\"outcome\":\"SATISFIED\",",
            "\"checks\":[]}\n",
        );
        assert_eq!(EvidenceCounts::from_log(log), EvidenceCounts::default());
    }

    #[test]
    fn marker_text_inside_another_payload_is_not_evidence() {
        let log = concat!(
            "EXPLORE:STATE {\"label\":",
            "\"EXPLORE:RELATIONSTATUS {\\\"outcome\\\":\\\"SATISFIED\\\"}\"}\n",
        );
        assert_eq!(EvidenceCounts::from_log(log), EvidenceCounts::default());
    }

    #[test]
    fn only_current_status_marker_families_are_marked_as_reported() {
        assert!(has_explicit_status_marker("detached-indicator"));
        assert!(has_explicit_status_marker("accessibility-state"));
        assert!(!has_explicit_status_marker("crash"));
        assert!(!has_explicit_status_marker("backend-contract"));
    }
}
