//! Ledger gap 6: eight verdict vocabularies, and until this file nothing said
//! how they map onto one another.
//!
//! `HermeticVerdict`, `ReplayVerdict`, `ArtifactVerdict`, `ReproVerdict`,
//! `ExecutionVerdict`, `ProviderVerdict`, `RunVerdict` and `Outcome` all answer
//! the same question at different layers, and the CLI translates between them
//! at half a dozen points. A translation error between two of them would be
//! invisible until it produced a wrong verdict in the field: a bug reported as
//! fixed, or a fix reported as a bug.
//!
//! This file pins the whole lattice against one canonical axis. Each `fn`
//! below is an exhaustive `match`, so adding a variant to any of the eight
//! enums fails to compile here until someone states what it means. The tests
//! then assert that the real translation functions agree with the axis.

use super::backend_headless::hermetic::HermeticVerdict;
use super::backend_headless::replay::ReplayVerdict;
use super::backend_headless::replay_command::artifact_verdict;
use super::backend_headless::retraction::ArtifactVerdict;
use super::check::aggregate_plan_runs;
use super::triage::reproduction::{ReproVerdict, classify_repro};
use crate::adapters::execution::runner::fold_provider_verdicts;
use crate::adapters::execution::runner::model::{PlanRun, ProviderVerdict};
use crate::domain::execution::ExecutionVerdict;
use crate::domain::repro::{self, Outcome, RunVerdict};
use crate::interface::cli::context::Exit;

/// The one axis every verdict vocabulary in this CLI actually answers on:
/// "is this run evidence about the recorded failure, and what does it say?"
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disposition {
    /// The recorded failure fired again. A live bug.
    Reproduced,
    /// The failure did not fire AND the run was evaluable. The ONLY
    /// proof-of-fix in the product: absence of evidence is not one.
    Certified,
    /// It fired on some runs and not others. A real non-determinism bug.
    Flaky,
    /// The run happened, but it is not evidence about THIS bug: the app drifted,
    /// the path no longer exists, or a different failure fired first.
    NotAboutThisBug,
    /// The run could not be evaluated at all (no target, no observation, the
    /// harness itself failed).
    Unevaluable,
    /// The claim was withdrawn by an explicit, reviewable edit. Passes, but
    /// proves nothing about the implementation.
    Withdrawn,
}

impl Disposition {
    /// Only one disposition is a proof-of-fix. `Withdrawn` deliberately is not.
    fn certifies(self) -> bool {
        matches!(self, Disposition::Certified)
    }

    /// The CI contract: a run exits zero only when the bug is proven gone or the
    /// claim was withdrawn. Everything else fails closed.
    fn exits_zero(self) -> bool {
        matches!(self, Disposition::Certified | Disposition::Withdrawn)
    }
}

fn hermetic(v: HermeticVerdict) -> Disposition {
    match v {
        HermeticVerdict::Reproduced => Disposition::Reproduced,
        HermeticVerdict::Fixed => Disposition::Certified,
        HermeticVerdict::Diverged => Disposition::NotAboutThisBug,
        HermeticVerdict::Inconclusive => Disposition::Unevaluable,
    }
}

fn replay(v: ReplayVerdict) -> Disposition {
    match v {
        ReplayVerdict::Reproduced => Disposition::Reproduced,
        ReplayVerdict::Fixed => Disposition::Certified,
        ReplayVerdict::Inconclusive => Disposition::Unevaluable,
    }
}

fn artifact(v: &ArtifactVerdict) -> Disposition {
    match v {
        ArtifactVerdict::Reproduced => Disposition::Reproduced,
        ArtifactVerdict::Fixed => Disposition::Certified,
        ArtifactVerdict::Inconclusive => Disposition::Unevaluable,
        ArtifactVerdict::Retracted(_) => Disposition::Withdrawn,
    }
}

fn outcome(v: Outcome) -> Disposition {
    match v {
        Outcome::Fail => Disposition::Reproduced,
        Outcome::Pass => Disposition::Certified,
        Outcome::Flaky => Disposition::Flaky,
        Outcome::Stale => Disposition::NotAboutThisBug,
    }
}

fn run(v: RunVerdict) -> Disposition {
    match v {
        RunVerdict::Broke => Disposition::Reproduced,
        RunVerdict::Green => Disposition::Certified,
        RunVerdict::CouldNotReplay => Disposition::NotAboutThisBug,
    }
}

fn repro_verdict(v: &ReproVerdict) -> Disposition {
    match v {
        ReproVerdict::Reproduced => Disposition::Reproduced,
        ReproVerdict::NotReproduced => Disposition::Certified,
        ReproVerdict::Flaky => Disposition::Flaky,
        ReproVerdict::Stale => Disposition::NotAboutThisBug,
        // Reached only when `check` produced neither a readable verdict nor a
        // known exit code, so nothing at all was evaluated. Distinct from
        // `Stale`, which is the specific claim that the app drifted.
        ReproVerdict::CouldNotReplay => Disposition::Unevaluable,
    }
}

fn execution(v: ExecutionVerdict) -> Disposition {
    match v {
        ExecutionVerdict::Reproduced => Disposition::Reproduced,
        ExecutionVerdict::NotReproduced => Disposition::Certified,
        ExecutionVerdict::Flaky => Disposition::Flaky,
        ExecutionVerdict::Stale => Disposition::NotAboutThisBug,
        ExecutionVerdict::DifferentFailure => Disposition::NotAboutThisBug,
        ExecutionVerdict::Incomplete => Disposition::Unevaluable,
        ExecutionVerdict::Unsupported => Disposition::Unevaluable,
        ExecutionVerdict::InfrastructureFailed => Disposition::Unevaluable,
    }
}

/// `ProviderVerdict` is the only one of the eight that is not a verdict about
/// the bug: it is the result of ONE provider step, and setup steps observe
/// nothing. `None` says exactly that, so a setup that passed can never be read
/// as a bug that did not reproduce.
fn provider(v: ProviderVerdict) -> Option<Disposition> {
    match v {
        ProviderVerdict::SetupPassed => None,
        ProviderVerdict::Reproduced => Some(Disposition::Reproduced),
        ProviderVerdict::NotReproduced => Some(Disposition::Certified),
        ProviderVerdict::DifferentFailure => Some(Disposition::NotAboutThisBug),
        ProviderVerdict::InfrastructureFailed => Some(Disposition::Unevaluable),
    }
}

const HERMETIC: [HermeticVerdict; 4] = [
    HermeticVerdict::Reproduced,
    HermeticVerdict::Fixed,
    HermeticVerdict::Diverged,
    HermeticVerdict::Inconclusive,
];
const REPLAY: [ReplayVerdict; 3] = [
    ReplayVerdict::Reproduced,
    ReplayVerdict::Fixed,
    ReplayVerdict::Inconclusive,
];
const OUTCOMES: [Outcome; 4] = [Outcome::Pass, Outcome::Stale, Outcome::Flaky, Outcome::Fail];
const RUNS: [RunVerdict; 3] = [
    RunVerdict::Green,
    RunVerdict::Broke,
    RunVerdict::CouldNotReplay,
];
const PROVIDERS: [ProviderVerdict; 5] = [
    ProviderVerdict::SetupPassed,
    ProviderVerdict::Reproduced,
    ProviderVerdict::NotReproduced,
    ProviderVerdict::DifferentFailure,
    ProviderVerdict::InfrastructureFailed,
];

fn artifacts() -> Vec<ArtifactVerdict> {
    vec![
        ArtifactVerdict::Reproduced,
        ArtifactVerdict::Fixed,
        ArtifactVerdict::Inconclusive,
        ArtifactVerdict::Retracted("schema changed".into()),
    ]
}

fn plan_run(verdict: ExecutionVerdict) -> PlanRun {
    PlanRun {
        plan_id: "plan".into(),
        occurrence_id: "occ".into(),
        verdict,
        phases: Vec::new(),
        provider_runs: Vec::new(),
    }
}

// The product promise, restated as assertions over every vocabulary at once.

#[test]
fn exactly_one_disposition_per_vocabulary_is_a_proof_of_fix() {
    // If a second variant of any enum ever starts certifying, this catches it.
    assert_eq!(
        HERMETIC.iter().filter(|v| hermetic(**v).certifies()).count(),
        1
    );
    assert_eq!(REPLAY.iter().filter(|v| replay(**v).certifies()).count(), 1);
    assert_eq!(
        artifacts().iter().filter(|v| artifact(v).certifies()).count(),
        1
    );
    assert_eq!(
        OUTCOMES.iter().filter(|v| outcome(**v).certifies()).count(),
        1
    );
    assert_eq!(RUNS.iter().filter(|v| run(**v).certifies()).count(), 1);
    assert_eq!(
        PROVIDERS
            .iter()
            .filter(|v| provider(**v).is_some_and(Disposition::certifies))
            .count(),
        1
    );
}

#[test]
fn nothing_unevaluable_or_drifted_ever_exits_zero() {
    for d in [Disposition::Unevaluable, Disposition::NotAboutThisBug] {
        assert!(!d.exits_zero(), "{d:?} must fail closed");
        assert!(!d.certifies(), "{d:?} must never be a proof of fix");
    }
    // A withdrawn claim passes, but is not evidence about the implementation.
    assert!(Disposition::Withdrawn.exits_zero());
    assert!(!Disposition::Withdrawn.certifies());
}

#[test]
fn the_exit_code_contract_matches_the_axis_in_both_vocabularies() {
    for o in OUTCOMES {
        assert_eq!(
            o.exit_code() == Exit::Clean as u8,
            outcome(o).exits_zero(),
            "{o:?} exit code contradicts its disposition"
        );
    }
    for h in HERMETIC {
        assert_eq!(
            h.exit_code() == Exit::Clean as u8,
            hermetic(h).exits_zero(),
            "{h:?} exit code contradicts its disposition"
        );
    }
    // The two blocking codes stay distinguishable: a live regression is not the
    // same signal as a run that could not be trusted.
    assert_eq!(Outcome::Fail.exit_code(), Exit::Regression as u8);
    assert_eq!(
        HermeticVerdict::Reproduced.exit_code(),
        Exit::Regression as u8
    );
    assert_ne!(
        HermeticVerdict::Diverged.exit_code(),
        HermeticVerdict::Reproduced.exit_code()
    );
}

#[test]
fn artifact_blocking_agrees_with_the_axis() {
    for v in artifacts() {
        assert_eq!(
            v.blocks(),
            !artifact(&v).exits_zero(),
            "{v:?} blocks() contradicts its disposition"
        );
    }
}

#[test]
fn replay_to_artifact_preserves_the_disposition_unless_the_claim_is_withdrawn() {
    for r in REPLAY {
        // No contract edit: the verdict passes through unchanged.
        assert_eq!(artifact(&artifact_verdict(r, None, "op")), replay(r));
        // A contract edit only retracts on an EVALUABLE non-reproduction.
        for recheck in REPLAY {
            let got = artifact_verdict(r, Some(recheck), "op");
            let expected = match (replay(r), replay(recheck)) {
                (Disposition::Reproduced, Disposition::Certified) => Disposition::Withdrawn,
                (d, _) => d,
            };
            assert_eq!(artifact(&got), expected, "{r:?} rechecked as {recheck:?}");
        }
    }
    // Stated directly, because it is the invariant that matters: a re-check
    // that could not be evaluated can never retract a live bug.
    assert_eq!(
        artifact_verdict(
            ReplayVerdict::Reproduced,
            Some(ReplayVerdict::Inconclusive),
            "op"
        ),
        ArtifactVerdict::Reproduced
    );
}

#[test]
fn folding_provider_verdicts_never_invents_evidence() {
    for p in PROVIDERS {
        let folded = execution(fold_provider_verdicts(&[p]));
        // A setup step alone evaluated nothing, so it must fold to unevaluable
        // rather than to "the bug did not reproduce".
        assert_eq!(folded, provider(p).unwrap_or(Disposition::Unevaluable));
    }
    // Severity precedence: anything that means "not evidence about this bug"
    // outranks a reproduction, which outranks a clean run.
    let unevaluable = [ProviderVerdict::Reproduced, ProviderVerdict::InfrastructureFailed];
    assert_eq!(
        execution(fold_provider_verdicts(&unevaluable)),
        Disposition::Unevaluable
    );
    let different = [ProviderVerdict::Reproduced, ProviderVerdict::DifferentFailure];
    assert_eq!(
        execution(fold_provider_verdicts(&different)),
        Disposition::NotAboutThisBug
    );
    let mixed = [ProviderVerdict::NotReproduced, ProviderVerdict::Reproduced];
    assert_eq!(
        execution(fold_provider_verdicts(&mixed)),
        Disposition::Reproduced
    );
    assert_eq!(
        execution(fold_provider_verdicts(&[])),
        Disposition::Unevaluable
    );
}

#[test]
fn execution_verdicts_aggregate_into_outcomes_without_changing_meaning() {
    for v in [
        ExecutionVerdict::Reproduced,
        ExecutionVerdict::NotReproduced,
        ExecutionVerdict::DifferentFailure,
        ExecutionVerdict::Incomplete,
        ExecutionVerdict::InfrastructureFailed,
        ExecutionVerdict::Unsupported,
    ] {
        let got = outcome(aggregate_plan_runs(&[plan_run(v)]));
        // Only a certifying execution may certify. Everything else fails closed,
        // though the aggregate is allowed to be coarser (Stale absorbs the
        // several distinct reasons a single run was not evidence).
        assert_eq!(got.certifies(), execution(v).certifies(), "{v:?}");
        assert_eq!(got.exits_zero(), execution(v).exits_zero(), "{v:?}");
    }
    // A run that reproduced and a run that did not, in the same plan, is flaky
    // in both vocabularies rather than either one alone.
    let both = [
        plan_run(ExecutionVerdict::Reproduced),
        plan_run(ExecutionVerdict::NotReproduced),
    ];
    assert_eq!(outcome(aggregate_plan_runs(&both)), Disposition::Flaky);
}

#[test]
fn run_verdicts_classify_into_outcomes_without_changing_meaning() {
    for v in RUNS {
        assert_eq!(outcome(repro::classify(&[v])), run(v), "{v:?}");
    }
    assert_eq!(
        outcome(repro::classify(&[RunVerdict::Green, RunVerdict::Broke])),
        Disposition::Flaky
    );
    // No runs at all is not a pass.
    assert!(!outcome(repro::classify(&[])).exits_zero());
}

#[test]
fn the_cross_process_hop_survives_both_the_json_and_the_exit_code() {
    // `reproduce` re-reads `check`'s verdict out of another process. Both the
    // JSON name and the exit-code fallback must land on the same meaning, or a
    // bug reported to the cloud is the opposite of what was observed.
    for o in OUTCOMES {
        let by_name = classify_repro(Some(o.as_str()), None);
        let by_code = classify_repro(None, Some(i32::from(o.exit_code())));
        assert_eq!(repro_verdict(&by_name), outcome(o), "{o:?} by name");
        assert_eq!(repro_verdict(&by_code), outcome(o), "{o:?} by exit code");
    }
    // Neither readable: no verdict at all, and it does not certify.
    let unknown = classify_repro(None, None);
    assert_eq!(repro_verdict(&unknown), Disposition::Unevaluable);
    assert!(!repro_verdict(&unknown).exits_zero());
    // An unrecognized name falls back to the code rather than guessing.
    assert_eq!(
        repro_verdict(&classify_repro(Some("weather"), Some(1))),
        Disposition::Reproduced
    );
}
