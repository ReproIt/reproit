//! Replay-time rechecks for recorded graph invariant violations.

use crate::model::map::RunObs;

/// Re-evaluation outcome for a single recorded graph-invariant violation,
/// replayed by `check`. Distinguishes "the invariant tripped again" (a real
/// regression) from "it held" (the fix worked) from "the replay never reached
/// the violating context" (re-record). Maps 1:1 onto the per-run verdict
/// `check` aggregates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphRecheck {
    /// The recorded state is present and the finding still violates its
    /// predicate.
    StillViolating,
    /// The recorded state is present but the finding predicate no longer fails.
    Fixed,
    /// The recorded state never appeared in the replay graph: the path to the
    /// finding's context is gone, so the invariant could not be re-evaluated.
    NotReached,
}

/// Re-confirm an older flicker finding over a replay log, mirroring
/// the recorded violating state sig (`trigger.sig`, the
/// transition's FROM state) is re-evaluated against the replay's
/// presented-frame `EXPLORE:FLICKER` records. DOM identity churn alone is not
/// visual evidence and intentionally cannot re-confirm a finding.
///   - the replay shows a transient frame divergence FROM that sig ->
///     StillViolating
///   - the sig is reached but no transition from it churned -> Fixed (held)
///   - the sig never appears in the replay graph -> NotReached (re-record)
pub fn recheck_rerender_flicker(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    let flickers = obs.paint_flickers.keys().any(|(from, _)| from == sig);
    if flickers {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY flicker signal (used by `check` for a
/// flicker repro that recorded no specific violating sig).
pub fn any_rerender_flicker(obs: &RunObs) -> bool {
    !obs.paint_flickers.is_empty()
}

/// Re-confirm a `no-broken-render` (content-bug) finding over a replay log,
/// mirroring `recheck_overflow`: the recorded violating state sig is
/// re-evaluated against the replay's `EXPLORE:CONTENTBUG` records.
///   - the replay still renders broken content at that sig -> StillViolating
///   - the sig is reached but renders no broken content -> Fixed (the fix held)
///   - the sig never appears in the replay graph -> NotReached (re-record).
pub fn recheck_content_bug(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    if obs
        .content_bugs
        .get(sig)
        .is_some_and(|items| !items.is_empty())
    {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY broken-content signal (used by `check` for
/// a content-bug repro that recorded no specific violating sig).
pub fn any_content_bug(obs: &RunObs) -> bool {
    obs.content_bugs.values().any(|items| !items.is_empty())
}

/// Re-confirm an explicit detached-indicator relationship at its recorded
/// state. UNKNOWN relationships emit no marker and therefore evaluate as fixed
/// only after the state itself was reached; an unreachable state remains stale.
pub fn recheck_detached_indicator(
    obs: &RunObs,
    sig: &str,
    dependent_key: Option<&str>,
) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    if let Some(want) = dependent_key.filter(|value| !value.is_empty()) {
        let Some(checks) = obs.relation_checks.get(sig) else {
            return GraphRecheck::NotReached;
        };
        let Some(check) = checks
            .iter()
            .find(|check| check.kind == "indicator-anchor" && check.dependent_key == want)
        else {
            // The exact declared relationship vanished or became ambiguous.
            // That is UNKNOWN, not proof that the UI was fixed.
            return GraphRecheck::NotReached;
        };
        return if check.outcome == "PROVEN" {
            GraphRecheck::StillViolating
        } else {
            GraphRecheck::Fixed
        };
    }
    // Older saved repros have no dependent key. Re-confirm only when the marker
    // contains a proven indicator relation; a VALID status means the class fixed,
    // while a missing/empty status remains UNKNOWN.
    if obs
        .relations
        .get(sig)
        .is_some_and(|items| items.iter().any(|item| item.kind == "indicator-anchor"))
    {
        return GraphRecheck::StillViolating;
    }
    if obs
        .relation_checks
        .get(sig)
        .is_some_and(|checks| checks.iter().any(|check| check.outcome == "VALID"))
    {
        GraphRecheck::Fixed
    } else {
        GraphRecheck::NotReached
    }
}

pub fn any_detached_indicator(obs: &RunObs) -> bool {
    obs.relations
        .values()
        .any(|items| items.iter().any(|item| item.kind == "indicator-anchor"))
}

/// Re-confirm a `no-jank` (web jank) finding over a replay log. A jank stall is
/// keyed by (from, action), so re-evaluate whether ANY transition FROM the
/// recorded sig still janks.
///   - a transition from that sig still janks -> StillViolating
///   - the sig is reached but no transition from it janks -> Fixed
///   - the sig never appears in the replay graph -> NotReached.
pub fn recheck_jank(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    if obs.janks.keys().any(|(from, _)| from == sig) {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY jank signal (used by `check` for a jank
/// repro that recorded no specific violating sig).
pub fn any_jank(obs: &RunObs) -> bool {
    !obs.janks.is_empty()
}

/// Re-confirm a `no-hang` (freeze) finding over a replay log, mirroring
/// `recheck_jank` against the `EXPLORE:HANG` records.
pub fn recheck_hang(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    if obs.hangs.keys().any(|(from, _)| from == sig) {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY hang signal (used by `check` for a hang
/// repro that recorded no specific violating sig).
pub fn any_hang(obs: &RunObs) -> bool {
    !obs.hangs.is_empty()
}
