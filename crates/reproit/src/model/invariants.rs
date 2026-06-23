//! INVARIANTS / PROPERTIES oracle (Antithesis-inspired).
//!
//! The earlier oracles (uncaught exception, jank threshold, graph dead-end,
//! unlabeled-semantics) were ad-hoc checks scattered through `modes/fuzz.rs`.
//! This module generalizes them into NAMED invariants evaluated over a single
//! run's observations (the parsed `EXPLORE:STATE`/`EXPLORE:EDGE` records, plus
//! the exception + perf findings that the existing oracles already produce).
//!
//! Three scopes, all pure functions over a run's observations:
//!   - State  invariants (node predicates): `all-labeled`, `no-jank` (sim),
//!     plus custom label-presence/absence regex and `unlabeled<=N`.
//!   - Edge   invariants: `no-exception` (the existing exception oracle, named).
//!   - Graph  invariants: `no-dead-end` (no non-terminal sink node), plus
//!     `no-leak` (reuse the soak/memory teardown signal when present).
//!
//! Every violation is returned in the SAME shape `all_findings` already
//! produces (`{kind, message, frames}`, plus an `invariant` id), so the
//! downstream find -> shrink -> reproduce -> report pipeline is unchanged.
//!
//! Tier honesty: graph / label / exception invariants run on the HEADLESS tier
//! (default). `no-jank` needs real frame timing and is SIM-ONLY; `no-leak`
//! relies on a memory/teardown signal that only the live runtime surfaces, so
//! it is best-effort headless (it fires on a teardown exception block, which
//! the headless explorer DOES emit) and authoritative under `--sim`.

use crate::config::{InvariantScope, InvariantsCfg};
use crate::map::RunObs;
use serde_json::{json, Value};

/// Everything the invariant set needs to evaluate one run. Built by the caller
/// from the per-seed log slice (+ the sim manifest, when on the sim tier).
pub struct Observations {
    /// Parsed `EXPLORE:STATE`/`EXPLORE:EDGE` records for this run.
    pub obs: RunObs,
    /// App exception findings already parsed (`exceptions_in_log` /
    /// `app_exceptions`): the `no-exception` edge oracle reuses these verbatim.
    pub exceptions: Vec<Value>,
    /// Per-state max jank percent, keyed by state sig, when the sim tier
    /// attributed frame timing per state. Empty on the headless tier
    /// (`no-jank` then reports nothing and is noted sim-only).
    pub jank_by_sig: std::collections::BTreeMap<String, f64>,
    /// True when a leaked-resource / teardown signal was observed (a teardown
    /// exception block headless, or a soak memory-growth signal under --sim).
    pub leak_signal: Option<String>,
    /// Whether this run is on the simulator tier (enables `no-jank`).
    pub sim: bool,
}

/// A single invariant finding, shaped like every other finding so the existing
/// report/shrink path consumes it unchanged.
pub fn finding(invariant: &str, kind: &str, message: String, sig: Option<&str>) -> Value {
    json!({
        "kind": kind,
        "invariant": invariant,
        "message": message,
        "sig": sig,
        "frames": [],
    })
}

/// Evaluate the full invariant set (built-ins gated by config + any custom
/// invariants) over one run's observations. Returns all violations.
pub fn evaluate(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
    let mut out = Vec::new();

    // ---- Edge invariants -------------------------------------------------
    // no-exception: the existing exception oracle, now a named edge invariant.
    if cfg.no_exception {
        for ex in &obs.exceptions {
            let kind = ex
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("EXCEPTION");
            let msg = ex.get("message").and_then(Value::as_str).unwrap_or("");
            // Preserve the original record (frames!) but tag it as the named
            // invariant so the report can attribute it.
            let mut rec = ex.clone();
            rec["invariant"] = json!("no-exception");
            if rec
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .is_empty()
            {
                rec["message"] = json!(format!("uncaught app exception: {kind}"));
            }
            let _ = msg;
            out.push(rec);
        }
    }

    // rerender-flicker: a transition that tore down and rebuilt persistent chrome
    // which did NOT change (the runner detects the DOM node-identity churn and
    // emits EXPLORE:RERENDER). A full re-render is the mechanism behind transition
    // flicker the settled-frame visual oracle cannot see, so we surface it per
    // transition. Deterministic: pure DOM, no frame timing, so it re-confirms on
    // replay.
    if cfg.rerender_flicker {
        for ((from, action), churned) in &obs.obs.rerenders {
            out.push(finding(
                "rerender-flicker",
                "FLICKER",
                format!(
                    "transition {from} --{action}--> rebuilt {} unchanged persistent element(s) ({}); a full re-render that flickers (the chrome is torn down and recreated, not reconciled)",
                    churned.len(),
                    churned.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                ),
                Some(from),
            ));
        }
        // paint-flicker: the gated Tier-2 pixel signal (EXPLORE:FLICKER). A frame
        // that diverged from both endpoints mid-transition then settled. Same
        // flicker oracle/toggle; timing-sensitive, so it is only emitted when the
        // runner ran with REPROIT_FLICKER_PIXELS and re-confirmed across repeats.
        for ((from, action), peak) in &obs.obs.paint_flickers {
            out.push(finding(
                "paint-flicker",
                "FLICKER",
                format!(
                    "transition {from} --{action}--> showed a transient frame {:.0}% different from both the start and the settled result (a flash the settled-frame oracle misses)",
                    peak * 100.0
                ),
                Some(from),
            ));
        }
    }

    // ---- State invariants ------------------------------------------------
    // all-labeled: every observed state must have zero unlabeled tappables.
    if cfg.all_labeled {
        for (sig, (labels, unlabeled)) in &obs.obs.states {
            if *unlabeled > 0 {
                out.push(finding(
                    "all-labeled",
                    "SEMANTICS",
                    format!(
                        "state {sig} has {unlabeled} unlabeled tappable(s) (missing semantics; invisible to screen readers and label automation){}",
                        screen_hint(labels)
                    ),
                    Some(sig),
                ));
            }
        }
    }

    // no-overflow: every observed state must have NO clipped/overflowing node.
    // The web runner measures this structurally (scrollWidth>clientWidth, a child
    // escaping its parent's content box, offsetWidth<scrollWidth for clipped text)
    // and emits EXPLORE:OVERFLOW per state. This is the i18n / long-string / RTL
    // failure class (a German or RTL label overflowing a fixed-width button).
    // Deterministic: pure layout measurement, no frame timing or pixels, so it
    // re-confirms on replay. Empty for runners that don't emit it (e.g. Flutter),
    // so this reports nothing there.
    if cfg.no_overflow {
        for (sig, items) in &obs.obs.overflows {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(key, kind, by)| format!("{key} ({kind} by {by}px)"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-overflow",
                "OVERFLOW",
                format!(
                    "state {sig} has {} overflowing/clipped element(s): {detail} (content does not fit its container/viewport; the i18n/long-string/RTL failure class)",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    // no-broken-render: every observed state must render NO broken-content
    // artifact (a label coerced from an object/undefined/null/NaN, or an
    // unrendered template). The web runner detects this from the DOM/labels and
    // emits EXPLORE:CONTENTBUG per state. This is the built-in version of the
    // user-declarable labelsAbsent custom invariant, so a render bug is caught
    // WITHOUT the developer first declaring it. Deterministic: pure DOM scan, no
    // pixels or timing, so it re-confirms on replay. Empty for runners/states
    // that render no broken content, so a clean app reports nothing.
    if cfg.no_broken_render {
        for (sig, items) in &obs.obs.content_bugs {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(key, reason, text)| format!("{key} ({reason}): {text:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-broken-render",
                "CONTENTBUG",
                format!(
                    "state {sig} renders {} broken-content label(s): {detail} (a stringify/template bug leaked a raw artifact like [object Object]/undefined/null/NaN/{{...}} to the screen)",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    // no-jank: a main-thread JANK stall on a transition. Two independent sources,
    // both gated by this one toggle so `--only jank` / `--no jank` cover them:
    //   - SIM tier: per-state frame-timing jank over budget (jank_by_sig). Headless
    //     has a fake clock, so jank_by_sig is empty there.
    //   - WEB tier: a Long Tasks stall on a transition (obs.janks). Deterministic
    //     (keyed off the browser's longtask trace, bucketed so timing jitter can't
    //     flip the verdict), so it re-confirms on replay. Empty unless an action
    //     blocked the main thread past the jank floor.
    if cfg.no_jank {
        if obs.sim {
            for (sig, jank) in &obs.jank_by_sig {
                if *jank > cfg.jank_pct_max {
                    out.push(finding(
                        "no-jank",
                        "PERF",
                        format!(
                            "state {sig} jank {jank:.1}% exceeds budget {:.0}% (sim tier)",
                            cfg.jank_pct_max
                        ),
                        Some(sig),
                    ));
                }
            }
        }
        for ((from, action), bucket) in &obs.obs.janks {
            out.push(finding(
                "no-jank",
                "PERF",
                format!(
                    "transition {from} --{action}--> blocked the main thread >= {bucket}ms (a dropped-frame jank stall; the handler ran a long synchronous task)"
                ),
                Some(from),
            ));
        }
    }

    // no-hang: a main-thread FREEZE on a transition (the app stopped making
    // progress). The web runner's watchdog reports an action whose synchronous
    // handler blocked the main thread past the hang floor (a far higher bucket
    // than jank), from the same Long Tasks trace, so it is deterministic and
    // re-confirms on replay. Empty unless an action froze the UI.
    if cfg.no_hang {
        for ((from, action), bucket) in &obs.obs.hangs {
            out.push(finding(
                "no-hang",
                "HANG",
                format!(
                    "transition {from} --{action}--> froze the main thread >= {bucket}ms with no progress (a synchronous hang: the app stopped responding for the duration)"
                ),
                Some(from),
            ));
        }
    }

    // no-leak: a leaked-resource / teardown signal. Headless surfaces a
    // teardown exception block (already in `exceptions` -> no-exception); this
    // adds a dedicated finding when a non-exception memory signal is present
    // (e.g. soak memory growth under --sim), so it is not double-counted.
    if cfg.no_leak {
        if let Some(detail) = &obs.leak_signal {
            out.push(finding(
                "no-leak",
                "LEAK",
                format!("resource leak signal: {detail}"),
                None,
            ));
        }
    }

    // ---- Graph invariants ------------------------------------------------
    // no-dead-end: a non-terminal state with no outgoing NON-back edge is a
    // sink the user can only escape via system back. Terminal states declared
    // in config are exempt (intended end screens).
    if cfg.no_dead_end {
        for sig in dead_ends(&obs.obs) {
            if cfg.terminal_states_match(&sig, label_set(&obs.obs, &sig)) {
                continue;
            }
            out.push(finding(
                "no-dead-end",
                "GRAPH",
                format!(
                    "state {sig} is a dead end: no outgoing action edge (escapable only via system back){}",
                    screen_hint(&label_set(&obs.obs, &sig))
                ),
                Some(&sig),
            ));
        }
    }

    // ---- Custom invariants ----------------------------------------------
    for c in &cfg.custom {
        out.extend(eval_custom(obs, c));
    }

    out
}

/// States that are dead ends in this run's observed graph: a state that was
/// observed AND has at least one outgoing edge recorded OR is the start, but
/// whose ONLY outgoing edges are `back`. A state with no outgoing edge at all
/// is a dead end iff it is not the start (the start with no edges just means an
/// empty walk). We treat "no non-back exit" as the dead-end condition, which is
/// exactly PLANTED-BUG 6 (the Advanced screen: reachable, but its only exit is
/// system back).
fn dead_ends(obs: &RunObs) -> Vec<String> {
    // Routes (URL path / framework anchor) that have a forward exit from SOME
    // state on them. On a dynamic single-page site, one logical page churns into
    // several structural snapshots (animation, lazy render) that share a route;
    // the snapshot where the walk's budget ran out has no recorded exit and would
    // look like a sink. If a same-route sibling does have a forward exit, it is
    // the same page and the walk could leave it, so the exit-less snapshot is an
    // artifact, not a dead end. A genuinely trapped screen has its own route and
    // is unaffected. Empty when no runner reports routes (TUI/desktop), so the
    // predicate is unchanged there.
    let mut routes_with_exit = std::collections::BTreeSet::new();
    for (from, action, to) in &obs.edges {
        if action != "back" && to != from {
            if let Some(route) = obs.routes.get(from) {
                routes_with_exit.insert(route.clone());
            }
        }
    }

    let mut out = Vec::new();
    for sig in obs.states.keys() {
        let is_start = obs.start.as_deref() == Some(sig.as_str());
        // Reachable as a destination of some edge, or the start state.
        let reachable = is_start || obs.edges.iter().any(|(_, _, to)| to == sig);
        if !reachable {
            continue;
        }
        // A START state the walk never acted from is an empty/unproductive walk,
        // not a proven sink (this fn's contract, and the common shape of a web
        // seed that churned without recording an exit). Only the start gets this
        // pass: a NON-start state reached with no exit IS a genuine sink (the
        // Advanced-screen planted bug), so it stays flagged.
        let acted_from = obs.edges.iter().any(|(from, _, _)| from == sig);
        if is_start && !acted_from {
            continue;
        }
        let has_forward_exit = obs
            .edges
            .iter()
            .any(|(from, action, to)| from == sig && action != "back" && to != sig);
        if has_forward_exit {
            continue;
        }
        // Same page has a forward exit -> this is a transient snapshot of an
        // escapable page, not a real sink. Two sources: a same-route sibling in
        // THIS seed's sparse graph, and the AGGREGATE map's escapable routes
        // folded in by the caller (covers the common case where one seed visited
        // the page only as its budget terminus).
        if let Some(route) = obs.routes.get(sig) {
            if routes_with_exit.contains(route) || obs.escapable_routes.contains(route) {
                continue;
            }
        }
        out.push(sig.clone());
    }
    out
}

/// Re-evaluation outcome for a single recorded graph-invariant violation,
/// replayed by `check`. Distinguishes "the invariant tripped again" (a real
/// regression) from "it held" (the fix worked) from "the replay never reached
/// the violating context" (re-record). Maps 1:1 onto the per-run verdict
/// `check` aggregates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphRecheck {
    /// The recorded state is present in the replay AND is still a dead end.
    StillViolating,
    /// The recorded state is present in the replay but is no longer a dead end
    /// (it gained a forward exit / Back control): the fix held.
    Fixed,
    /// The recorded state never appeared in the replay graph: the path to the
    /// finding's context is gone, so the invariant could not be re-evaluated.
    NotReached,
}

/// Re-evaluate the `no-dead-end` graph invariant for ONE recorded violating
/// state signature over a replay's observed graph. This is the SAME dead-end
/// predicate the fuzz oracle uses (`dead_ends`), applied to the single recorded
/// sig so `check` re-confirms the exact finding the repro was kept for rather
/// than scanning for exceptions.
///
///   - the sig is unobserved in the replay        -> NotReached (stale)
///   - the sig is observed and is still a dead end -> StillViolating (fail)
///   - the sig is observed but now has a forward exit -> Fixed (pass)
pub fn recheck_dead_end(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    if dead_ends(obs).iter().any(|s| s == sig) {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY dead end (used by `check` for an older
/// graph repro that recorded no specific violating sig).
pub fn any_dead_end(obs: &RunObs) -> bool {
    !dead_ends(obs).is_empty()
}

/// Re-confirm a `rerender-flicker` finding over a replay log, mirroring
/// `recheck_dead_end`: the recorded violating state sig (`trigger.sig`, the
/// transition's FROM state) is re-evaluated against the replay's
/// `EXPLORE:RERENDER` records.
///   - the replay shows a re-render churn FROM that sig -> StillViolating (fail)
///   - the sig is reached but no transition from it churned -> Fixed (held)
///   - the sig never appears in the replay graph -> NotReached (re-record)
pub fn recheck_rerender_flicker(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    // Either flicker signal from this sig (DOM churn or the gated pixel flash)
    // re-confirms the finding.
    let flickers = obs.rerenders.keys().any(|(from, _)| from == sig)
        || obs.paint_flickers.keys().any(|(from, _)| from == sig);
    if flickers {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY flicker signal (used by `check` for a
/// flicker repro that recorded no specific violating sig).
pub fn any_rerender_flicker(obs: &RunObs) -> bool {
    !obs.rerenders.is_empty() || !obs.paint_flickers.is_empty()
}

/// Re-confirm a `no-overflow` finding over a replay log, mirroring
/// `recheck_rerender_flicker`: the recorded violating state sig is re-evaluated
/// against the replay's `EXPLORE:OVERFLOW` records.
///   - the replay still overflows at that sig -> StillViolating (fail)
///   - the sig is reached but nothing overflows there -> Fixed (held)
///   - the sig never appears in the replay graph -> NotReached (re-record)
pub fn recheck_overflow(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    if obs
        .overflows
        .get(sig)
        .is_some_and(|items| !items.is_empty())
    {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY overflow signal (used by `check` for an
/// overflow repro that recorded no specific violating sig).
pub fn any_overflow(obs: &RunObs) -> bool {
    obs.overflows.values().any(|items| !items.is_empty())
}

/// Re-confirm a `no-broken-render` (content-bug) finding over a replay log,
/// mirroring `recheck_overflow`: the recorded violating state sig is re-evaluated
/// against the replay's `EXPLORE:CONTENTBUG` records.
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

/// Whether the replay graph has ANY broken-content signal (used by `check` for a
/// content-bug repro that recorded no specific violating sig).
pub fn any_content_bug(obs: &RunObs) -> bool {
    obs.content_bugs.values().any(|items| !items.is_empty())
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

fn label_set(obs: &RunObs, sig: &str) -> Vec<String> {
    obs.states
        .get(sig)
        .map(|(labels, _)| labels.clone())
        .unwrap_or_default()
}

fn screen_hint(labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!(
            " [{}]",
            labels
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Evaluate one custom invariant against the run.
fn eval_custom(obs: &Observations, c: &crate::config::CustomInvariant) -> Vec<Value> {
    let mut out = Vec::new();
    match &c.scope {
        InvariantScope::State => {
            for (sig, (labels, unlabeled)) in &obs.obs.states {
                // labels-match: every state's labels must contain a match.
                if let Some(re) = &c.labels_match {
                    if !labels.iter().any(|l| re.is_match(l)) {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "state {sig} violates {}: no label matches /{}/{}",
                                c.id,
                                re.as_str(),
                                screen_hint(labels)
                            ),
                            Some(sig),
                        ));
                    }
                }
                // labels-absent: no label may match.
                if let Some(re) = &c.labels_absent {
                    if let Some(hit) = labels.iter().find(|l| re.is_match(l)) {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "state {sig} violates {}: label {hit:?} matches forbidden /{}/",
                                c.id,
                                re.as_str()
                            ),
                            Some(sig),
                        ));
                    }
                }
                // unlabeled<=N.
                if let Some(max) = c.max_unlabeled {
                    if *unlabeled > max {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "state {sig} violates {}: {unlabeled} unlabeled tappables exceed max {max}",
                                c.id
                            ),
                            Some(sig),
                        ));
                    }
                }
            }
        }
        InvariantScope::Edge => {
            // Custom edge invariant: forbid an action (by regex) anywhere, e.g.
            // "no destructive tap reachable". Start simple: a forbidden-action
            // regex flags any edge whose action string matches.
            if let Some(re) = &c.action_absent {
                for (from, action, to) in &obs.obs.edges {
                    if re.is_match(action) {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "edge {from} --{action}--> {to} violates {}: forbidden action /{}/",
                                c.id,
                                re.as_str()
                            ),
                            Some(from),
                        ));
                    }
                }
            }
        }
        InvariantScope::Graph => {
            // Custom graph invariant: a no-dead-end toggle (with this id's own
            // terminal allowlist already folded into the global one), plus a
            // reachability requirement: a label that MUST be reachable.
            if c.no_dead_end {
                for sig in dead_ends(&obs.obs) {
                    out.push(finding(
                        &c.id,
                        "GRAPH",
                        format!("state {sig} violates {}: dead end", c.id),
                        Some(&sig),
                    ));
                }
            }
            if let Some(re) = &c.must_reach {
                let reached = obs
                    .obs
                    .states
                    .values()
                    .any(|(labels, _)| labels.iter().any(|l| re.is_match(l)));
                if !reached {
                    out.push(finding(
                        &c.id,
                        "GRAPH",
                        format!(
                            "invariant {} violated: no observed state has a label matching required /{}/",
                            c.id,
                            re.as_str()
                        ),
                        None,
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn obs_with(
        states: &[(&str, &[&str], u32)],
        edges: &[(&str, &str, &str)],
        start: Option<&str>,
    ) -> Observations {
        let mut s = BTreeMap::new();
        for (sig, labels, unlabeled) in states {
            s.insert(
                sig.to_string(),
                (labels.iter().map(|x| x.to_string()).collect(), *unlabeled),
            );
        }
        Observations {
            obs: RunObs {
                states: s,
                routes: Default::default(),
                edges: edges
                    .iter()
                    .map(|(f, a, t)| (f.to_string(), a.to_string(), t.to_string()))
                    .collect(),
                start: start.map(String::from),
                escapable_routes: Default::default(),
                gaps: Default::default(),
                rerenders: Default::default(),
                paint_flickers: Default::default(),
                overflows: Default::default(),
                content_bugs: Default::default(),
                janks: Default::default(),
                hangs: Default::default(),
            },
            exceptions: vec![],
            jank_by_sig: BTreeMap::new(),
            leak_signal: None,
            sim: false,
        }
    }

    fn kinds(findings: &[Value]) -> Vec<String> {
        findings
            .iter()
            .map(|f| {
                f.get("invariant")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn all_labeled_flags_a_state_with_unlabeled_tappables() {
        // home -> feed; feed has 1 unlabeled tappable (the bugzoo IconButton).
        let o = obs_with(
            &[("home", &["Go"], 0), ("feed", &["Feed", "Post 1"], 1)],
            &[("home", "tap:Go", "feed")],
            Some("home"),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let inv = kinds(&f);
        assert!(inv.contains(&"all-labeled".to_string()), "got {inv:?}");
        // The violation names the offending state sig.
        let v = f.iter().find(|x| x["invariant"] == "all-labeled").unwrap();
        assert_eq!(v["sig"], "feed");
        // The fully-labeled home state must NOT be flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "all-labeled" && x["sig"] == "home"));
    }

    #[test]
    fn rerender_flicker_flags_a_churning_transition() {
        // A transition that rebuilt unchanged persistent chrome is a flicker; a
        // reconciled transition (no churn recorded) is not.
        let mut o = obs_with(&[("s1", &["My App"], 0)], &[], Some("s1"));
        o.obs.rerenders.insert(
            ("s1".to_string(), "tap:key:id:bad".to_string()),
            vec!["id:hdr".to_string(), "id:nav".to_string()],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"rerender-flicker".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "rerender-flicker")
            .unwrap();
        assert_eq!(v["sig"], "s1");
        assert_eq!(v["kind"], "FLICKER");
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            rerender_flicker: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"rerender-flicker".to_string()));
    }

    #[test]
    fn recheck_rerender_flicker_distinguishes_back_held_unreached() {
        // StillViolating: the sig is observed and a transition from it churns.
        let mut o = obs_with(&[("s1", &["x"], 0)], &[], Some("s1"));
        o.obs.rerenders.insert(
            ("s1".to_string(), "tap:key:id:bad".to_string()),
            vec!["id:hdr".to_string()],
        );
        assert_eq!(
            recheck_rerender_flicker(&o.obs, "s1"),
            GraphRecheck::StillViolating
        );
        // Fixed: the sig is observed but nothing churns from it (the fix held).
        let held = obs_with(&[("s1", &["x"], 0)], &[], Some("s1"));
        assert_eq!(
            recheck_rerender_flicker(&held.obs, "s1"),
            GraphRecheck::Fixed
        );
        // NotReached: the sig never appeared in the replay graph.
        assert_eq!(
            recheck_rerender_flicker(&held.obs, "other"),
            GraphRecheck::NotReached
        );
    }

    #[test]
    fn no_overflow_flags_a_state_with_a_clipped_node() {
        // A state with an overflowing node fires; a state with none stays silent.
        let mut o = obs_with(
            &[("home", &["Go"], 0), ("settings", &["Settings"], 0)],
            &[("home", "tap:Go", "settings")],
            Some("home"),
        );
        o.obs.overflows.insert(
            "settings".to_string(),
            vec![("id:save".to_string(), "clip".to_string(), 84)],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-overflow".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f.iter().find(|x| x["invariant"] == "no-overflow").unwrap();
        assert_eq!(v["sig"], "settings");
        assert_eq!(v["kind"], "OVERFLOW");
        // The clean `home` state must NOT be flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-overflow" && x["sig"] == "home"));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_overflow: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-overflow".to_string()));
    }

    #[test]
    fn recheck_overflow_distinguishes_held_unreached() {
        // StillViolating: the sig is observed and still overflows.
        let mut o = obs_with(&[("s1", &["x"], 0)], &[], Some("s1"));
        o.obs.overflows.insert(
            "s1".to_string(),
            vec![("id:btn".to_string(), "spill".to_string(), 40)],
        );
        assert_eq!(recheck_overflow(&o.obs, "s1"), GraphRecheck::StillViolating);
        // Fixed: the sig is observed but nothing overflows there (the fix held).
        let held = obs_with(&[("s1", &["x"], 0)], &[], Some("s1"));
        assert_eq!(recheck_overflow(&held.obs, "s1"), GraphRecheck::Fixed);
        // NotReached: the sig never appeared in the replay graph.
        assert_eq!(
            recheck_overflow(&held.obs, "other"),
            GraphRecheck::NotReached
        );
    }

    #[test]
    fn no_broken_render_flags_a_state_with_a_broken_label() {
        // A state rendering [object Object] fires; a clean state stays silent.
        let mut o = obs_with(
            &[("home", &["Go"], 0), ("acct", &["Account"], 0)],
            &[("home", "tap:Go", "acct")],
            Some("home"),
        );
        o.obs.content_bugs.insert(
            "acct".to_string(),
            vec![(
                "id:acct-name".to_string(),
                "object-object".to_string(),
                "Account: [object Object]".to_string(),
            )],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-broken-render".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-broken-render")
            .unwrap();
        assert_eq!(v["sig"], "acct");
        assert_eq!(v["kind"], "CONTENTBUG");
        assert!(v["message"].as_str().unwrap().contains("object Object"));
        // The clean `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-broken-render" && x["sig"] == "home"));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_broken_render: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-broken-render".to_string()));
    }

    #[test]
    fn recheck_content_bug_distinguishes_held_unreached() {
        let mut o = obs_with(&[("s1", &["x"], 0)], &[], Some("s1"));
        o.obs.content_bugs.insert(
            "s1".to_string(),
            vec![("id:x".to_string(), "null".to_string(), "null".to_string())],
        );
        assert_eq!(
            recheck_content_bug(&o.obs, "s1"),
            GraphRecheck::StillViolating
        );
        let held = obs_with(&[("s1", &["x"], 0)], &[], Some("s1"));
        assert_eq!(recheck_content_bug(&held.obs, "s1"), GraphRecheck::Fixed);
        assert_eq!(
            recheck_content_bug(&held.obs, "other"),
            GraphRecheck::NotReached
        );
    }

    #[test]
    fn no_jank_fires_on_a_web_longtask_stall_without_sim() {
        // The web jank path is NOT gated on sim: a longtask stall on a transition
        // fires headless. A clean walk (no janks) stays silent.
        let mut o = obs_with(&[("home", &["Go"], 0)], &[], Some("home"));
        o.obs.janks.insert(
            ("home".to_string(), "tap:key:testid:recompute".to_string()),
            200,
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-jank").unwrap();
        assert_eq!(v["kind"], "PERF");
        assert_eq!(v["sig"], "home");
        // `recheck_jank` re-confirms by FROM-sig.
        assert_eq!(recheck_jank(&o.obs, "home"), GraphRecheck::StillViolating);
        // Disabling no-jank suppresses it.
        let cfg = InvariantsCfg {
            no_jank: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-jank".to_string()));
    }

    #[test]
    fn no_hang_fires_on_a_web_freeze() {
        let mut o = obs_with(&[("home", &["Go"], 0)], &[], Some("home"));
        o.obs.hangs.insert(
            ("home".to_string(), "tap:key:testid:export".to_string()),
            2000,
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-hang").unwrap();
        assert_eq!(v["kind"], "HANG");
        assert_eq!(v["sig"], "home");
        assert!(v["message"].as_str().unwrap().contains("froze"));
        assert_eq!(recheck_hang(&o.obs, "home"), GraphRecheck::StillViolating);
        let cfg = InvariantsCfg {
            no_hang: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-hang".to_string()));
    }

    #[test]
    fn no_dead_end_flags_a_sink_node() {
        // home -> advanced; advanced has NO outgoing edge: a sink (PLANTED-BUG 6).
        let o = obs_with(
            &[
                ("home", &["Go"], 0),
                ("advanced", &["Advanced", "Verbose logging"], 0),
            ],
            &[("home", "tap:Advanced", "advanced")],
            Some("home"),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(kinds(&f).contains(&"no-dead-end".to_string()));
        let v = f.iter().find(|x| x["invariant"] == "no-dead-end").unwrap();
        assert_eq!(v["sig"], "advanced");
        // home is not a dead end (it has a forward exit), so not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-dead-end" && x["sig"] == "home"));
    }

    #[test]
    fn back_only_exit_is_still_a_dead_end() {
        // advanced has only a `back` edge out: still a dead end (no forward exit).
        let o = obs_with(
            &[("home", &["Go"], 0), ("advanced", &["Advanced"], 0)],
            &[
                ("home", "tap:Advanced", "advanced"),
                ("advanced", "back", "home"),
            ],
            Some("home"),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(f
            .iter()
            .any(|x| x["invariant"] == "no-dead-end" && x["sig"] == "advanced"));
    }

    #[test]
    fn same_route_snapshots_are_not_dead_ends() {
        // A dynamic single-page site: one route "/" churns into three structural
        // snapshots as it animates. The walk ends at s2 (budget exhausted), which
        // has no recorded exit, but its same-route siblings s0/s1 DO, so s2 is an
        // animation artifact, not a sink. (Regression: the archastro.ai false
        // positive.)
        let mut o = obs_with(
            &[
                ("s0", &["Home"], 0),
                ("s1", &["Home"], 0),
                ("s2", &["Home"], 0),
            ],
            &[("s0", "tap:link", "s1"), ("s1", "tap:link", "s2")],
            Some("s0"),
        );
        for s in ["s0", "s1", "s2"] {
            o.obs.routes.insert(s.to_string(), "/".to_string());
        }
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            !f.iter().any(|x| x["invariant"] == "no-dead-end"),
            "no snapshot of an escapable single-page route should be a dead end"
        );
    }

    #[test]
    fn lone_start_state_with_no_edges_is_not_a_dead_end() {
        // The actual archastro.ai seed shape: the walk observed only the start
        // state and recorded no edge (it churned without a clean transition). An
        // unproductive walk is not a proven sink, so the landing page must not be
        // flagged. (A non-start reached sink still is: see no_dead_end_flags_a_sink_node.)
        let o = obs_with(&[("home", &["Home"], 0)], &[], Some("home"));
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(!f.iter().any(|x| x["invariant"] == "no-dead-end"));
    }

    #[test]
    fn distinct_route_sink_is_still_a_dead_end() {
        // home (/) -> trap (/trap); trap has no exit AND its own route, so the
        // same-route suppression does not apply: still a real dead end.
        let mut o = obs_with(
            &[("home", &["Go"], 0), ("trap", &["Stuck"], 0)],
            &[("home", "tap:Go", "trap")],
            Some("home"),
        );
        o.obs.routes.insert("home".into(), "/".into());
        o.obs.routes.insert("trap".into(), "/trap".into());
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(f
            .iter()
            .any(|x| x["invariant"] == "no-dead-end" && x["sig"] == "trap"));
    }

    #[test]
    fn terminal_states_allowlist_exempts_intended_end_screens() {
        let cfg = InvariantsCfg {
            terminal_states: vec!["advanced".to_string()],
            ..Default::default()
        };
        let o = obs_with(
            &[("home", &["Go"], 0), ("advanced", &["Advanced"], 0)],
            &[("home", "tap:Advanced", "advanced")],
            Some("home"),
        );
        let f = evaluate(&o, &cfg);
        assert!(
            !f.iter().any(|x| x["invariant"] == "no-dead-end"),
            "allowlisted terminal should not flag: {:?}",
            kinds(&f)
        );
    }

    #[test]
    fn no_exception_wraps_the_existing_exception_finding() {
        let mut o = obs_with(&[("home", &["Go"], 0)], &[], Some("home"));
        o.exceptions = vec![json!({
            "kind": "EXCEPTION CAUGHT BY WIDGETS LIBRARY",
            "message": "A leaked AnimationController was found",
            "frames": ["package:bugzoo/main.dart:210:5"],
        })];
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-exception").unwrap();
        assert_eq!(v["kind"], "EXCEPTION CAUGHT BY WIDGETS LIBRARY");
        // Frames are preserved so the report still points at code.
        assert!(v["frames"][0].as_str().unwrap().contains("main.dart:210"));
    }

    #[test]
    fn no_jank_is_sim_only() {
        let mut o = obs_with(&[("feed", &["Feed"], 0)], &[], Some("feed"));
        o.jank_by_sig.insert("feed".to_string(), 80.0);
        // Headless: jank reported but tier is not sim -> no finding.
        assert!(!evaluate(&o, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-jank"));
        // Sim: same data now fires.
        o.sim = true;
        assert!(evaluate(&o, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-jank"));
    }

    #[test]
    fn recheck_dead_end_still_violating_when_sig_is_still_a_sink() {
        // advanced is reachable and has no forward exit: still a dead end.
        let o = obs_with(
            &[("home", &["Go"], 0), ("advanced", &["Advanced"], 0)],
            &[("home", "tap:Advanced", "advanced")],
            Some("home"),
        );
        assert_eq!(
            recheck_dead_end(&o.obs, "advanced"),
            GraphRecheck::StillViolating
        );
    }

    #[test]
    fn recheck_dead_end_fixed_when_sig_gains_a_forward_exit() {
        // The fix added a forward exit out of advanced: no longer a dead end.
        let o = obs_with(
            &[
                ("home", &["Go"], 0),
                ("advanced", &["Advanced", "Continue"], 0),
                ("next", &["Next"], 0),
            ],
            &[
                ("home", "tap:Advanced", "advanced"),
                ("advanced", "tap:Continue", "next"),
            ],
            Some("home"),
        );
        assert_eq!(recheck_dead_end(&o.obs, "advanced"), GraphRecheck::Fixed);
    }

    #[test]
    fn recheck_dead_end_not_reached_when_sig_unobserved() {
        // The replay never reached `advanced` (the early path moved).
        let o = obs_with(
            &[("home", &["Go"], 0), ("feed", &["Feed"], 0)],
            &[("home", "tap:Go", "feed")],
            Some("home"),
        );
        assert_eq!(
            recheck_dead_end(&o.obs, "advanced"),
            GraphRecheck::NotReached
        );
    }

    #[test]
    fn any_dead_end_reflects_the_graph() {
        let sink = obs_with(
            &[("home", &["Go"], 0), ("advanced", &["Advanced"], 0)],
            &[("home", "tap:Advanced", "advanced")],
            Some("home"),
        );
        assert!(any_dead_end(&sink.obs));
        let clean = obs_with(
            &[("home", &["Go"], 0), ("feed", &["Feed"], 0)],
            &[("home", "tap:Go", "feed"), ("feed", "tap:Go", "home")],
            Some("home"),
        );
        assert!(!any_dead_end(&clean.obs));
    }

    #[test]
    fn custom_unlabeled_max_and_label_regex() {
        use crate::config::CustomInvariant;
        let cfg = InvariantsCfg {
            custom: vec![CustomInvariant {
                id: "settings-has-save".to_string(),
                scope: InvariantScope::State,
                labels_match: Some(regex::Regex::new("(?i)save").unwrap()),
                ..Default::default()
            }],
            ..Default::default()
        };
        // A state with no "Save" label violates the custom invariant.
        let o = obs_with(
            &[("settings", &["Profile", "Logout"], 0)],
            &[],
            Some("settings"),
        );
        let f = evaluate(&o, &cfg);
        assert!(f.iter().any(|x| x["invariant"] == "settings-has-save"));
    }
}
