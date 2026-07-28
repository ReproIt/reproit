use crate::adapters::execution::PlanRun;
use crate::domain::execution::ExecutionVerdict;
use crate::domain::repro::{CheckResult, Outcome};

pub(super) fn plan_verification_summary(runs: &[PlanRun]) -> serde_json::Value {
    let observation_reached_runs = runs
        .iter()
        .filter(|run| {
            matches!(
                run.verdict,
                ExecutionVerdict::Reproduced | ExecutionVerdict::NotReproduced
            )
        })
        .count();
    let exact_identity_runs = runs
        .iter()
        .filter(|run| run.verdict == ExecutionVerdict::Reproduced)
        .count();
    serde_json::json!({
        "contract": "exact-observation-v1",
        "cleanLaunchRuns": observation_reached_runs,
        "observationReachedRuns": observation_reached_runs,
        "exactIdentityRuns": exact_identity_runs,
    })
}

pub(super) fn guard_verification_summary(result: &CheckResult) -> serde_json::Value {
    let observation_reached_runs = if result.outcome == Outcome::Stale {
        0
    } else {
        result.total
    };
    let exact_identity_runs = if result.outcome == Outcome::Stale {
        0
    } else {
        result.total.saturating_sub(result.green)
    };
    serde_json::json!({
        "contract": "exact-observation-v1",
        "cleanLaunchRuns": observation_reached_runs,
        "observationReachedRuns": observation_reached_runs,
        "exactIdentityRuns": exact_identity_runs,
    })
}
