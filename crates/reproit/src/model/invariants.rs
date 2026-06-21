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

    // no-jank: SIM ONLY. Per-state jank over budget is a finding. Headless has
    // a fake clock (no real frame timing), so jank_by_sig is empty there and
    // this reports nothing (the caller notes it sim-only).
    if cfg.no_jank && obs.sim {
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
    let mut out = Vec::new();
    for sig in obs.states.keys() {
        // Reachable as a destination of some edge, or the start state.
        let reachable = obs.start.as_deref() == Some(sig.as_str())
            || obs.edges.iter().any(|(_, _, to)| to == sig);
        if !reachable {
            continue;
        }
        let has_forward_exit = obs
            .edges
            .iter()
            .any(|(from, action, to)| from == sig && action != "back" && to != sig);
        if !has_forward_exit {
            out.push(sig.clone());
        }
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
                gaps: Default::default(),
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
