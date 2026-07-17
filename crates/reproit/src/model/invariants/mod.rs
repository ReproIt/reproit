//! INVARIANTS / PROPERTIES oracle (property-based testing).
//!
//! The earlier oracles (uncaught exception, jank threshold,
//! operability semantics) were ad-hoc checks scattered through
//! `modes/fuzz/mod.rs`. This module generalizes them into NAMED invariants
//! evaluated over a single run's observations (the parsed
//! `EXPLORE:STATE`/`EXPLORE:EDGE` records, plus the exception + perf findings
//! that the existing oracles already produce).
//!
//! Three scopes, all pure functions over a run's observations:
//!   - State invariants (node predicates): `no-jank` (sim), plus custom
//!     label-presence/absence regex.
//!   - Edge   invariants: `no-exception` (the existing exception oracle,
//!     named).
//!   - Graph  invariants: `no-occluded-control`, plus `no-leak` (reuse the
//!     soak/memory teardown signal when present). The general graph-sink oracle
//!     was removed as crawler-budget FP-prone; its sink predicate survives only
//!     for permission-walk.
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

#[cfg(test)]
use crate::config::{InvariantScope, InvariantsCfg};
use crate::model::map::RunObs;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;

mod custom;
mod evaluate;
mod finding;
mod graph;
mod recheck;

pub use evaluate::evaluate;
#[allow(unused_imports)] // Preserve the existing finding-constructor façade for callers/tests.
pub use finding::{advisory_finding, finding};
#[cfg(test)]
use graph::permission_traps;
pub use recheck::{
    any_content_bug, any_detached_indicator, any_hang, any_jank, any_rerender_flicker,
    recheck_content_bug, recheck_detached_indicator, recheck_hang, recheck_jank,
    recheck_rerender_flicker, GraphRecheck,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn advisory_finding_is_flagged_but_still_classifies() {
        // paint-flicker is a raw-pixel signal: reported (advisory) but never a
        // verdict-bearing repro. It must carry the advisory flag yet still map to
        // its oracle so the report can group it.
        let f = advisory_finding("paint-flicker", "FLICKER", "flash".into(), Some("s"));
        assert_eq!(f.get("advisory").and_then(Value::as_bool), Some(true));
        assert_eq!(
            f.get("invariant").and_then(Value::as_str),
            Some("paint-flicker")
        );
        assert_eq!(
            crate::crosscut::classify(&f),
            crate::crosscut::Oracle::Flicker
        );
    }

    #[test]
    fn stuck_keyboard_fires_per_sig_and_respects_gate() {
        let mut o = obs_with(&[("s1", &["Detail"])], &[], Some("s1"));
        o.obs.stuck_keyboards.insert("s1".to_string());
        let f = evaluate(&o, &InvariantsCfg::default());
        let hit = f
            .iter()
            .find(|x| x["invariant"] == "no-stuck-keyboard")
            .expect("stuck-keyboard finding for s1");
        assert_eq!(hit["kind"], "STUCKKEYBOARD");
        assert_eq!(hit["sig"], "s1");
        assert_eq!(
            crate::crosscut::classify(hit),
            crate::crosscut::Oracle::StuckKeyboard
        );
        // The message keeps essentials before any parenthesis (scan detail
        // truncates at the first " (").
        let msg = hit["message"].as_str().unwrap();
        assert!(msg.contains("soft keyboard open with no text field focused"));
        // Gated off: no finding.
        let cfg = InvariantsCfg {
            no_stuck_keyboard: false,
            ..Default::default()
        };
        assert!(!evaluate(&o, &cfg)
            .iter()
            .any(|x| x["invariant"] == "no-stuck-keyboard"));
        // A clean run (no marker) stays silent.
        let clean = obs_with(&[("s1", &["Detail"])], &[], Some("s1"));
        assert!(!evaluate(&clean, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-stuck-keyboard"));
    }

    #[test]
    fn wakelock_leak_fires_per_sig_and_respects_gate() {
        let mut o = obs_with(&[("video", &["Player"])], &[], Some("video"));
        o.obs.wakelock_leaks.insert(
            "video".to_string(),
            vec![
                ("com.app:VideoPlayback".to_string(), "wakelock".to_string()),
                ("KEEP_SCREEN_ON".to_string(), "keep-screen-on".to_string()),
            ],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let hit = f
            .iter()
            .find(|x| x["invariant"] == "no-wakelock-leak")
            .expect("wakelock finding for video");
        assert_eq!(hit["kind"], "WAKELOCK");
        assert_eq!(hit["sig"], "video");
        assert_eq!(
            crate::crosscut::classify(hit),
            crate::crosscut::Oracle::WakeLock
        );
        // The message keeps the essentials (the leaked tag) before any parenthesis
        // (scan detail truncates at the first " (").
        let msg = hit["message"].as_str().unwrap();
        let head = msg.split(" (").next().unwrap();
        assert!(head.contains("com.app:VideoPlayback"));
        assert!(head.contains("navigate away"));
        // Gated off: no finding.
        let cfg = InvariantsCfg {
            no_wakelock_leak: false,
            ..Default::default()
        };
        assert!(!evaluate(&o, &cfg)
            .iter()
            .any(|x| x["invariant"] == "no-wakelock-leak"));
        // A clean run (no marker) stays silent.
        let clean = obs_with(&[("video", &["Player"])], &[], Some("video"));
        assert!(!evaluate(&clean, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-wakelock-leak"));
    }

    fn obs_with(
        states: &[(&str, &[&str])],
        edges: &[(&str, &str, &str)],
        start: Option<&str>,
    ) -> Observations {
        let mut s = BTreeMap::new();
        for (sig, labels) in states {
            s.insert(
                sig.to_string(),
                labels.iter().map(|x| x.to_string()).collect(),
            );
        }
        Observations {
            obs: RunObs {
                states: s,
                routes: Default::default(),
                tappables: Default::default(),
                elements: Default::default(),
                texts: Default::default(),
                occlusions: Default::default(),
                security: Default::default(),
                blank_screens: Default::default(),
                broken_assets: Default::default(),
                zoom_reflows: Default::default(),
                scroll_round_trips: Default::default(),
                rotation_losses: Default::default(),
                background_losses: Default::default(),
                stuck_keyboards: Default::default(),
                edges: edges
                    .iter()
                    .map(|(f, a, t)| (f.to_string(), a.to_string(), t.to_string()))
                    .collect(),
                start: start.map(String::from),
                escapable_route_labels: Default::default(),
                gaps: Default::default(),
                rerenders: Default::default(),
                paint_flickers: Default::default(),
                content_bugs: Default::default(),
                relations: Default::default(),
                relation_checks: Default::default(),
                janks: Default::default(),
                duplicate_submits: Default::default(),
                focus_losses: Default::default(),
                hangs: Default::default(),
                choice_bugs: Default::default(),
                broken_routes: Default::default(),
                app_invariants: Default::default(),
                listener_leaks: Default::default(),
                wakelock_leaks: Default::default(),
                safe_areas: Default::default(),
                permission_screens: Default::default(),
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
    fn app_invariant_violation_becomes_a_finding() {
        // An app-registered invariant the SDK reported as failed in a state
        // becomes an `app-invariant` finding (kind INVARIANT), naming the state
        // and carrying the SDK's message. Disabling the flag silences it, and a
        // clean run produces none.
        let mut o = obs_with(&[("s1", &["Cart"])], &[], Some("s1"));
        o.obs.app_invariants.insert(
            "s1".to_string(),
            vec![(
                "cart total never negative".to_string(),
                "total was -5".to_string(),
            )],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f
            .iter()
            .find(|x| x["invariant"] == "app-invariant")
            .expect("app-invariant finding");
        assert_eq!(v["kind"], "INVARIANT");
        assert_eq!(v["sig"], "s1");
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("cart total never negative"), "got {msg}");
        assert!(msg.contains("total was -5"), "got {msg}");

        let cfg = InvariantsCfg {
            no_invariant_violation: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"app-invariant".to_string()));

        let clean = obs_with(&[("s1", &["Cart"])], &[], Some("s1"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"app-invariant".to_string()));
    }

    #[test]
    fn dom_identity_churn_is_not_a_flicker() {
        // Replacing unchanged DOM anchors is an implementation detail. Without a
        // transient presented frame, it must remain silent.
        let mut o = obs_with(&[("s1", &["My App"])], &[], Some("s1"));
        o.obs.rerenders.insert(
            ("s1".to_string(), "tap:key:id:bad".to_string()),
            vec!["id:hdr".to_string(), "id:nav".to_string()],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(!kinds(&f).contains(&"rerender-flicker".to_string()));
    }

    #[test]
    fn no_duplicate_submit_flags_a_double_fired_request() {
        // The double-dispatch probe found a pay button that fired the same POST
        // twice: the finding carries the from-sig, action, method, url, and
        // count (all before any parenthesis, so scan detail keeps them).
        let mut o = obs_with(&[("s1", &["Checkout"])], &[], Some("s1"));
        o.obs.duplicate_submits.insert(
            ("s1".to_string(), "tap:key:id:pay".to_string()),
            (
                "POST".to_string(),
                "https://app.example/api/orders".to_string(),
                2,
            ),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-duplicate-submit".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-duplicate-submit")
            .unwrap();
        assert_eq!(v["kind"], "DUPSUBMIT");
        let msg = v["message"].as_str().unwrap();
        for needle in [
            "s1",
            "tap:key:id:pay",
            "POST",
            "https://app.example/api/orders",
            "2 times",
        ] {
            assert!(msg.contains(needle), "message misses {needle}: {msg}");
        }
        // The finding classifies to its own oracle category.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::DuplicateSubmit
        );
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_duplicate_submit: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-duplicate-submit".to_string()));
        // A run with no DUPSUBMIT records (probe off or every handler guarded)
        // stays silent.
        let clean = obs_with(&[("s1", &["Checkout"])], &[], Some("s1"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-duplicate-submit".to_string()));
    }

    #[test]
    fn no_focus_loss_flags_a_dropped_focus_transition() {
        // A tap that left keyboard focus on <body> while the control survived:
        // the finding names the from-sig and the action (essentials before any
        // parenthesis, so scan detail keeps them).
        let mut o = obs_with(&[("s1", &["Todo"])], &[], Some("s1"));
        o.obs
            .focus_losses
            .insert(("s1".to_string(), "tap:key:id:add".to_string()));
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-focus-loss".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-focus-loss")
            .unwrap();
        assert_eq!(v["kind"], "FOCUSLOSS");
        let msg = v["message"].as_str().unwrap();
        for needle in ["s1", "tap:key:id:add", "document.body"] {
            assert!(msg.contains(needle), "message misses {needle}: {msg}");
        }
        // The finding classifies to its own oracle category.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::FocusLoss
        );
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_focus_loss: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-focus-loss".to_string()));
        // A run with no FOCUSLOSS records stays silent.
        let clean = obs_with(&[("s1", &["Todo"])], &[], Some("s1"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-focus-loss".to_string()));
    }

    #[test]
    fn recheck_rerender_flicker_distinguishes_back_held_unreached() {
        // DOM churn alone does not re-confirm a visible flicker.
        let mut o = obs_with(&[("s1", &["x"])], &[], Some("s1"));
        o.obs.rerenders.insert(
            ("s1".to_string(), "tap:key:id:bad".to_string()),
            vec!["id:hdr".to_string()],
        );
        assert_eq!(recheck_rerender_flicker(&o.obs, "s1"), GraphRecheck::Fixed);
        // A transient presented-frame divergence does re-confirm it.
        o.obs
            .paint_flickers
            .insert(("s1".to_string(), "tap:key:id:bad".to_string()), 0.42);
        assert_eq!(
            recheck_rerender_flicker(&o.obs, "s1"),
            GraphRecheck::StillViolating
        );
        // Fixed: the sig is observed but nothing churns from it (the fix held).
        let held = obs_with(&[("s1", &["x"])], &[], Some("s1"));
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
    fn no_broken_render_flags_a_state_with_a_broken_label() {
        // A state rendering [object Object] fires; a clean state stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("acct", &["Account"])],
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
    fn rotation_and_background_loss_fire_and_respect_flags() {
        // A state that regressed its structure across a rotation round-trip fires
        // no-rotation-loss; one that regressed across background/restore fires
        // no-background-loss. A clean run is silent; each flag gates its finding.
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs
            .rotation_losses
            .insert("home".to_string(), ("abc".to_string(), "def".to_string()));
        o.obs
            .background_losses
            .insert("home".to_string(), ("abc".to_string(), "xyz".to_string()));
        let f = evaluate(&o, &InvariantsCfg::default());
        let rot = f
            .iter()
            .find(|x| x["invariant"] == "no-rotation-loss")
            .expect("rotation finding");
        assert_eq!(rot["kind"], "ROTATION");
        assert_eq!(rot["sig"], "home");
        assert_eq!(
            crate::crosscut::classify(rot),
            crate::crosscut::Oracle::Rotation
        );
        // Essentials before any parenthesis: the message states the loss first.
        assert!(rot["message"]
            .as_str()
            .unwrap()
            .contains("does not survive rotation"));
        let bg = f
            .iter()
            .find(|x| x["invariant"] == "no-background-loss")
            .expect("background finding");
        assert_eq!(bg["kind"], "BGRESTORE");
        assert_eq!(
            crate::crosscut::classify(bg),
            crate::crosscut::Oracle::BackgroundRestore
        );
        // A clean run reports neither.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        let cf = kinds(&evaluate(&clean, &InvariantsCfg::default()));
        assert!(!cf.contains(&"no-rotation-loss".to_string()));
        assert!(!cf.contains(&"no-background-loss".to_string()));
        // Each flag suppresses its own finding independently.
        let cfg = InvariantsCfg {
            no_rotation_loss: false,
            no_background_loss: false,
            ..Default::default()
        };
        let gated = kinds(&evaluate(&o, &cfg));
        assert!(!gated.contains(&"no-rotation-loss".to_string()));
        assert!(!gated.contains(&"no-background-loss".to_string()));
    }

    #[test]
    fn no_blank_screen_flags_an_empty_state() {
        // A state that rendered nothing (zero visible text nodes, zero
        // tappables, non-empty viewport) fires; a clean state stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("dead", &[])],
            &[("home", "tap:Go", "dead")],
            Some("home"),
        );
        o.obs.blank_screens.insert(
            "dead".to_string(),
            vec![("tag:body".to_string(), 1280, 720)],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-blank-screen".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-blank-screen")
            .unwrap();
        assert_eq!(v["sig"], "dead");
        assert_eq!(v["kind"], "BLANKSCREEN");
        // Essentials before any parenthesis: the message names the viewport.
        assert!(v["message"].as_str().unwrap().contains("1280x720"));
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::BlankScreen
        );
        // The content-bearing `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-blank-screen" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-blank-screen".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_blank_screen: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-blank-screen".to_string()));
    }

    #[test]
    fn no_safe_area_flags_a_control_in_an_inset() {
        // A control whose hit rect overlaps a device inset fires; a screen with
        // no control in an inset stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("scan", &["Done"])],
            &[("home", "tap:Go", "scan")],
            Some("home"),
        );
        o.obs.safe_areas.insert(
            "scan".to_string(),
            vec![("key:done".to_string(), "top".to_string(), 18)],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-safe-area-collision".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-safe-area-collision")
            .unwrap();
        assert_eq!(v["sig"], "scan");
        assert_eq!(v["kind"], "SAFEAREA");
        // Essentials before any parenthesis: the control, edge, and depth.
        let msg = v["message"].as_str().unwrap();
        let head = msg.split(" (").next().unwrap();
        assert!(head.contains("key:done"));
        assert!(head.contains("top"));
        assert!(head.contains("18px"));
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::SafeArea
        );
        // The clean `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-safe-area-collision" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-safe-area-collision".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_safe_area: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-safe-area-collision".to_string()));
    }

    #[test]
    fn no_permission_dead_end_flags_a_denied_permission_sink() {
        // Under a denial sweep, a post-denial screen that is ALSO a graph dead
        // end fires and names the permission; a post-denial screen that has a
        // forward exit does NOT fire (it is not a dead end), and a dead end that
        // was NOT marked as post-denial is ignored.
        // `perm` (post-denial sink) is a genuine sink; `flow` has a forward exit.
        let mut o = obs_with(
            &[
                ("home", &["Scan"]),
                ("perm", &["Enable Camera"]),
                ("flow", &["Manual"]),
                ("ok", &["Home"]),
            ],
            &[
                ("home", "tap:Scan", "perm"),
                ("home", "tap:Manual", "flow"),
                ("flow", "tap:Manual", "ok"),
            ],
            Some("home"),
        );
        o.obs
            .permission_screens
            .insert("perm".to_string(), "camera".to_string());
        o.obs
            .permission_screens
            .insert("flow".to_string(), "camera".to_string());
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-permission-dead-end")
            .expect("permission dead-end fires on the post-denial sink");
        assert_eq!(v["sig"], "perm");
        assert_eq!(v["kind"], "PERMISSIONWALK");
        // Essentials before any parenthesis: the permission and the state.
        let head = v["message"].as_str().unwrap().split(" (").next().unwrap();
        assert!(head.contains("camera"));
        assert!(head.contains("perm"));
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::PermissionWalk
        );
        // `flow` has a forward exit, so the permission oracle does NOT flag it.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-permission-dead-end" && x["sig"] == "flow"));
        // Outside a denial sweep (no marked screens) the oracle is silent, even
        // when the graph has the same dead end.
        let mut clean = obs_with(
            &[("home", &["Scan"]), ("perm", &["Enable Camera"])],
            &[("home", "tap:Scan", "perm")],
            Some("home"),
        );
        clean.obs.permission_screens.clear();
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-permission-dead-end".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_permission_dead_end: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-permission-dead-end".to_string()));
    }

    #[test]
    fn no_listener_leak_flags_a_monotonic_climb_per_route() {
        // A route whose listeners/nodes climb across revisits fires; a stable
        // route stays silent. The runner only emits a monotonic climb, so the
        // Rust side simply surfaces every reported metric.
        let mut o = obs_with(&[("home", &["Home"])], &[], Some("home"));
        o.obs.listener_leaks.insert(
            "/detail".to_string(),
            (
                5,
                vec![
                    ("listeners".to_string(), 8, 40),
                    ("nodes".to_string(), 120, 180),
                ],
            ),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let leak: Vec<_> = f
            .iter()
            .filter(|x| x["invariant"] == "no-listener-leak")
            .collect();
        // One finding per leaking metric (listeners + nodes).
        assert_eq!(leak.len(), 2, "got {:?}", kinds(&f));
        assert_eq!(leak[0]["kind"], "LISTENERLEAK");
        assert_eq!(leak[0]["sig"], "/detail");
        // Route + climb lead the message, before any " (".
        let msg = leak[0]["message"].as_str().unwrap();
        assert!(msg.contains("/detail") && msg.contains("40"), "got {msg}");
        // Classifies to the Leak oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(leak[0]),
            crate::crosscut::Oracle::Leak
        );
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Home"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-listener-leak".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_listener_leak: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-listener-leak".to_string()));
    }

    #[test]
    fn no_reflow_break_flags_a_route_that_breaks_at_zoom() {
        // A route that grows a horizontal scrollbar or collapses a tappable at
        // 200% zoom fires; a cleanly reflowing route stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("table", &["Report"])],
            &[("home", "tap:Go", "table")],
            Some("home"),
        );
        o.obs.zoom_reflows.insert(
            "table".to_string(),
            vec![
                ("tag:html".to_string(), "hscroll".to_string(), 560),
                ("key:id:save".to_string(), "collapsed".to_string(), 0),
            ],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-reflow-break".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-reflow-break")
            .unwrap();
        assert_eq!(v["sig"], "table");
        assert_eq!(v["kind"], "ZOOMREFLOW");
        // Essentials before any parenthesis: count + per-item break detail.
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("2 reflow violation(s)"), "message: {msg}");
        assert!(
            msg.contains("tag:html scrolls horizontally by 560px"),
            "message: {msg}"
        );
        assert!(
            msg.contains("key:id:save collapses to 0px"),
            "message: {msg}"
        );
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::ZoomReflow
        );
        // The cleanly reflowing `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-reflow-break" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-reflow-break".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_zoom_reflow: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-reflow-break".to_string()));
    }

    #[test]
    fn no_scroll_recycle_flags_content_that_differs_after_round_trip() {
        // A list whose content at a pinned offset differs after scrolling away
        // and back fires; a stable list stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("feed", &["Feed"])],
            &[("home", "tap:Go", "feed")],
            Some("home"),
        );
        o.obs.scroll_round_trips.insert(
            "feed".to_string(),
            vec![(
                "y=0".to_string(),
                "Alpha|Bravo".to_string(),
                "Charlie|Delta".to_string(),
            )],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-scroll-recycle".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-scroll-recycle")
            .unwrap();
        assert_eq!(v["sig"], "feed");
        assert_eq!(v["kind"], "SCROLLROUNDTRIP");
        // Essentials before any parenthesis: count + before/after content.
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("1 scroll position(s)"), "message: {msg}");
        assert!(
            msg.contains("at y=0 \"Alpha|Bravo\" became \"Charlie|Delta\""),
            "message: {msg}"
        );
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::ScrollRoundTrip
        );
        // The `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-scroll-recycle" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-scroll-recycle".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_scroll_round_trip: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-scroll-recycle".to_string()));
    }

    #[test]
    fn no_broken_asset_flags_dead_subresources() {
        // A state with visible and critical network asset failures fires once,
        // carrying the reasons in the detail; a clean state stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("shop", &["Shop"])],
            &[("home", "tap:Go", "shop")],
            Some("home"),
        );
        o.obs.broken_assets.insert(
            "shop".to_string(),
            vec![
                (
                    "key:id:hero".to_string(),
                    "img".to_string(),
                    "missing.png".to_string(),
                ),
                (
                    "tag:link".to_string(),
                    "stylesheet-http".to_string(),
                    "https://app.test/app.css status=404 content-type=text/css".to_string(),
                ),
                (
                    "key:id:desc".to_string(),
                    "tofu".to_string(),
                    "glitch \u{FFFD}".to_string(),
                ),
            ],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-broken-asset".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-broken-asset")
            .unwrap();
        assert_eq!(v["sig"], "shop");
        assert_eq!(v["kind"], "BROKENASSET");
        // Essentials before any parenthesis: count + per-item reason detail.
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("3 broken critical asset(s)"), "message: {msg}");
        assert!(msg.contains("[img] missing.png"), "message: {msg}");
        assert!(msg.contains("[stylesheet-http]"), "message: {msg}");
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::BrokenAsset
        );
        // The clean `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-broken-asset" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-broken-asset".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_broken_asset: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-broken-asset".to_string()));
    }

    #[test]
    fn recheck_content_bug_distinguishes_held_unreached() {
        let mut o = obs_with(&[("s1", &["x"])], &[], Some("s1"));
        o.obs.content_bugs.insert(
            "s1".to_string(),
            vec![("id:x".to_string(), "null".to_string(), "null".to_string())],
        );
        assert_eq!(
            recheck_content_bug(&o.obs, "s1"),
            GraphRecheck::StillViolating
        );
        let held = obs_with(&[("s1", &["x"])], &[], Some("s1"));
        assert_eq!(recheck_content_bug(&held.obs, "s1"), GraphRecheck::Fixed);
        assert_eq!(
            recheck_content_bug(&held.obs, "other"),
            GraphRecheck::NotReached
        );
    }

    #[test]
    fn no_choice_anomaly_flags_an_outlier_choice() {
        // A multi-choice component reported one option that shifted the global
        // layout while its siblings did not. The differential outlier fires; a
        // run with no choice-bugs stays silent.
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs.choice_bugs.push((
            "home".to_string(),
            "tab".to_string(),
            "Go".to_string(),
            "role:tab#3".to_string(),
            720,
        ));
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-choice-anomaly")
            .unwrap();
        assert_eq!(v["sig"], "home");
        assert!(v["message"].as_str().unwrap().contains("Go"));
        // Empty -> no finding.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!evaluate(&clean, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-choice-anomaly"));
        // Toggle off suppresses it.
        let cfg = InvariantsCfg {
            no_choice_anomaly: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-choice-anomaly".to_string()));
    }

    #[test]
    fn no_broken_route_flags_a_4xx_state() {
        // A visited route whose document responded >= 400 is a dead route the app
        // linked to. It fires once per broken route; a clean run stays silent.
        let mut o = obs_with(&[("dl", &["Page not found"])], &[], Some("dl"));
        o.obs.broken_routes.push((
            "dl".to_string(),
            "/download".to_string(),
            404,
            Some("home".to_string()),
        ));
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-broken-route")
            .unwrap();
        assert_eq!(v["sig"], "dl");
        assert!(v["message"].as_str().unwrap().contains("/download"));
        assert!(v["message"].as_str().unwrap().contains("404"));
        // Visited shape (from != sig): the flagged screen IS the dead document.
        assert!(v["message"]
            .as_str()
            .unwrap()
            .contains("this screen's document"));
        // Link-check shape (from == sig): the finding sits on the healthy SOURCE
        // screen, so the copy must say the LINK target is what is broken.
        let mut o2 = obs_with(&[("home", &["Classes"])], &[], Some("home"));
        o2.obs.broken_routes.push((
            "home".to_string(),
            "/gone".to_string(),
            404,
            Some("home".to_string()),
        ));
        let f2 = evaluate(&o2, &InvariantsCfg::default());
        let v2 = f2
            .iter()
            .find(|x| x["invariant"] == "no-broken-route")
            .unwrap();
        let m2 = v2["message"].as_str().unwrap();
        assert!(m2.contains("dead link on this screen"));
        assert!(m2.contains("/gone"));
        assert!(m2.contains("loads fine"));
        // Cloudflare email-protection links get the specific remedy, not the
        // generic dead-route line.
        let mut o3 = obs_with(&[("home", &["Contact"])], &[], Some("home"));
        o3.obs.broken_routes.push((
            "home".to_string(),
            "/cdn-cgi/l/email-protection".to_string(),
            404,
            Some("home".to_string()),
        ));
        let f3 = evaluate(&o3, &InvariantsCfg::default());
        let m3 = f3
            .iter()
            .find(|x| x["invariant"] == "no-broken-route")
            .unwrap()["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(m3.contains("mailto:"));
        assert!(m3.contains("Cloudflare email-protection"));
        // Empty -> no finding.
        let clean = obs_with(&[("dl", &["x"])], &[], Some("dl"));
        assert!(!evaluate(&clean, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-broken-route"));
        // Toggle off suppresses it.
        let cfg = InvariantsCfg {
            no_broken_route: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-broken-route".to_string()));
    }

    #[test]
    fn no_jank_fires_on_a_web_longtask_stall_without_sim() {
        // The web jank path is NOT gated on sim: a longtask stall on a transition
        // fires headless. A clean walk (no janks) stays silent.
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs.janks.insert(
            ("home".to_string(), "tap:key:testid:recompute".to_string()),
            (200, "ms".to_string()),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-jank").unwrap();
        assert_eq!(v["kind"], "PERF");
        assert_eq!(v["sig"], "home");
        assert!(v["message"].as_str().unwrap().contains(">= 200ms"));
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
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs.hangs.insert(
            ("home".to_string(), "tap:key:testid:export".to_string()),
            (2000, "ms".to_string()),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-hang").unwrap();
        assert_eq!(v["kind"], "HANG");
        assert_eq!(v["sig"], "home");
        assert!(v["message"].as_str().unwrap().contains("froze"));
        assert!(v["message"].as_str().unwrap().contains(">= 2000ms"));
        assert_eq!(recheck_hang(&o.obs, "home"), GraphRecheck::StillViolating);
        let cfg = InvariantsCfg {
            no_hang: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-hang".to_string()));
    }

    #[test]
    fn hang_message_renders_a_non_ms_unit_without_claiming_milliseconds() {
        // The TUI hang bucket is a count of ignored keypresses, not wall-clock ms;
        // the message must say so ("14 keypresses"), not "14ms".
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs.hangs.insert(
            ("home".to_string(), "key:Enter".to_string()),
            (14, "keypresses".to_string()),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-hang").unwrap();
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains(">= 14 keypresses"), "got: {msg}");
        assert!(!msg.contains("14ms"), "must not claim ms: {msg}");
    }

    #[test]
    fn permission_trap_predicate_flags_a_reached_sink() {
        // home -> advanced; advanced has NO outgoing edge: a sink (PLANTED-BUG 6).
        let o = obs_with(
            &[
                ("home", &["Go"]),
                ("advanced", &["Advanced", "Verbose logging"]),
            ],
            &[("home", "tap:Advanced", "advanced")],
            Some("home"),
        );
        let de = permission_traps(&o.obs);
        assert!(de.iter().any(|s| s == "advanced"));
        // home is not a dead end (it has a forward exit).
        assert!(!de.iter().any(|s| s == "home"));
    }

    #[test]
    fn back_only_exit_is_still_a_dead_end() {
        // advanced has only a `back` edge out: still a dead end (no forward exit).
        let o = obs_with(
            &[("home", &["Go"]), ("advanced", &["Advanced"])],
            &[
                ("home", "tap:Advanced", "advanced"),
                ("advanced", "back", "home"),
            ],
            Some("home"),
        );
        assert!(permission_traps(&o.obs).iter().any(|s| s == "advanced"));
    }

    #[test]
    fn single_url_distinct_screen_sink_is_a_dead_end() {
        // A single-URL app (JS section toggle, no hash/path change): the dashboard
        // and a content-only "Advanced" pane share route "/". The dashboard has a
        // forward exit and tappables; Advanced is a genuine sink (reached by an
        // action, no exit, no controls). Because every state shares the one route,
        // route-only suppression would wrongly excuse Advanced via the dashboard's
        // exit/tappables. Its labels are NOT a subset of the dashboard's, so the
        // label-aware predicate keeps it flagged. (Regression: corpus
        // dead-end-advanced went silent under route-only suppression.)
        let mut o = obs_with(
            &[
                (
                    "dashboard",
                    &["Dashboard", "Open advanced settings", "Refresh queue"],
                ),
                ("advanced", &["Advanced", "Nothing to configure yet."]),
            ],
            &[("dashboard", "tap:advanced", "advanced")],
            Some("dashboard"),
        );
        for s in ["dashboard", "advanced"] {
            o.obs.routes.insert(s.to_string(), "/".to_string());
        }
        assert!(
            permission_traps(&o.obs).iter().any(|s| s == "advanced"),
            "a distinct content-only screen sharing the URL is a real dead end"
        );
    }

    #[test]
    fn same_route_snapshots_are_not_dead_ends() {
        // A dynamic single-page site: one route "/" churns into three structural
        // snapshots as it animates. The walk ends at s2 (budget exhausted), which
        // has no recorded exit, but its same-route siblings s0/s1 DO, so s2 is an
        // animation artifact, not a sink. (Regression: the archastro.ai false
        // positive.)
        let mut o = obs_with(
            &[("s0", &["Home"]), ("s1", &["Home"]), ("s2", &["Home"])],
            &[("s0", "tap:link", "s1"), ("s1", "tap:link", "s2")],
            Some("s0"),
        );
        for s in ["s0", "s1", "s2"] {
            o.obs.routes.insert(s.to_string(), "/".to_string());
        }
        assert!(
            permission_traps(&o.obs).is_empty(),
            "no snapshot of an escapable single-page route should be a dead end"
        );
    }

    #[test]
    fn lone_start_state_with_no_edges_is_not_a_dead_end() {
        // The actual archastro.ai seed shape: the walk observed only the start
        // state and recorded no edge (it churned without a clean transition). An
        // unproductive walk is not a proven sink, so the landing page must not be
        // flagged. (A non-start reached sink still is: see
        // no_dead_end_flags_a_sink_node.)
        let o = obs_with(&[("home", &["Home"])], &[], Some("home"));
        assert!(permission_traps(&o.obs).is_empty());
    }

    #[test]
    fn unexplored_leaf_with_untapped_nav_is_not_a_dead_end() {
        // A leaf reached as the budget terminus that still offers tappable nav the
        // walk never tapped (header links deduped after being tried elsewhere) is
        // not a trap. Regression: the cloud.google.com /blog/<article> dead-end FP.
        let mut o = obs_with(
            &[("home", &["Home"]), ("article", &["Cloud", "Blog"])],
            &[("home", "tap:role:link#0", "article")],
            Some("home"),
        );
        o.obs.tappables.insert("article".into(), 4); // offered 4 nav links, tapped 0
        assert!(!permission_traps(&o.obs).iter().any(|s| s == "article"));
    }

    #[test]
    fn exhausted_sink_with_all_tappables_tried_is_still_a_dead_end() {
        // The walk DID tap the screen's action and it self-looped (no forward
        // exit). Tappables exhausted -> a genuine sink, still flagged.
        let mut o = obs_with(
            &[("home", &["Home"]), ("trap", &["Stuck"])],
            &[
                ("home", "tap:role:link#0", "trap"),
                ("trap", "tap:role:button#0", "trap"),
            ],
            Some("home"),
        );
        o.obs.tappables.insert("trap".into(), 1); // offered 1, tapped 1 -> exhausted
        assert!(permission_traps(&o.obs).iter().any(|s| s == "trap"));
    }

    #[test]
    fn tui_exhausted_sink_counts_key_actions_as_tried() {
        // A TUI sink (forward actions are `key:*`, not `tap:`) that offered 2
        // elements and tried both via key presses, self-looping with no forward
        // exit, IS a genuine dead end. The old `tap:`-only count read the key
        // presses as untried (offered > tapped), so it suppressed every TUI sink
        // and the oracle never fired on the TUI despite being marked covered.
        let mut o = obs_with(
            &[("home", &["Home"]), ("trap", &["Stuck"])],
            &[
                ("home", "key:Enter", "trap"),
                ("trap", "key:Down", "trap"),
                ("trap", "key:Enter", "trap"),
            ],
            Some("home"),
        );
        o.obs.tappables.insert("trap".into(), 2); // offered 2, tried 2 keys -> exhausted
        let de = permission_traps(&o.obs);
        assert!(
            de.iter().any(|s| s == "trap"),
            "a TUI sink with all key actions tried must be a dead end: {de:?}"
        );
    }

    #[test]
    fn distinct_route_sink_is_still_a_dead_end() {
        // home (/) -> trap (/trap); trap has no exit AND its own route, so the
        // same-route suppression does not apply: still a real dead end.
        let mut o = obs_with(
            &[("home", &["Go"]), ("trap", &["Stuck"])],
            &[("home", "tap:Go", "trap")],
            Some("home"),
        );
        o.obs.routes.insert("home".into(), "/".into());
        o.obs.routes.insert("trap".into(), "/trap".into());
        assert!(permission_traps(&o.obs).iter().any(|s| s == "trap"));
    }

    #[test]
    fn no_exception_wraps_the_existing_exception_finding() {
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
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
        let mut o = obs_with(&[("feed", &["Feed"])], &[], Some("feed"));
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
    fn custom_label_regex() {
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
            &[("settings", &["Profile", "Logout"])],
            &[],
            Some("settings"),
        );
        let f = evaluate(&o, &cfg);
        assert!(f.iter().any(|x| x["invariant"] == "settings-has-save"));
    }

    #[test]
    fn detached_indicator_fires_only_for_proven_explicit_relationship() {
        let mut observations = obs_with(&[("nav", &["Liked You"])], &[], Some("nav"));
        observations.obs.relations.insert(
            "nav".into(),
            vec![crate::model::map::RelationViolation {
                kind: "indicator-anchor".into(),
                dependent_key: "key:id:dot".into(),
                owner_key: "key:id:liked".into(),
                container_key: "key:id:tabs".into(),
                violation: "detached".into(),
                max_gap: 8,
                gap_centipx: 12_345,
            }],
        );
        let findings = evaluate(&observations, &InvariantsCfg::default());
        let finding = findings
            .iter()
            .find(|finding| finding["invariant"] == "no-detached-indicator")
            .expect("detached indicator finding");
        assert_eq!(finding["kind"], "DETACHEDINDICATOR");
        assert_eq!(finding["selector"], "key:id:dot");
        assert_eq!(finding["relationship"]["ownerKey"], "key:id:liked");

        let disabled = InvariantsCfg {
            no_detached_indicator: false,
            ..InvariantsCfg::default()
        };
        assert!(!evaluate(&observations, &disabled)
            .iter()
            .any(|finding| finding["invariant"] == "no-detached-indicator"));
    }

    #[test]
    fn detached_indicator_recheck_distinguishes_proven_valid_and_unknown() {
        let log = concat!(
            "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[\"Liked You\"]}\n",
            "EXPLORE:RELATIONSTATUS {\"sig\":\"nav\",\"outcome\":\"VALID\",\"checks\":[",
            "{\"kind\":\"indicator-anchor\",\"dependentKey\":\"key:id:dot\",",
            "\"ownerKey\":\"key:id:liked\",\"containerKey\":\"key:id:tabs\",\"outcome\":\"VALID\"\
             }]}\n",
        );
        let valid = crate::model::map::parse_run(log);
        assert_eq!(
            recheck_detached_indicator(&valid, "nav", Some("key:id:dot")),
            GraphRecheck::Fixed
        );
        assert_eq!(
            recheck_detached_indicator(&valid, "nav", Some("key:id:other")),
            GraphRecheck::NotReached
        );
        let unknown = crate::model::map::parse_run(
            "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[]}\nEXPLORE:RELATIONSTATUS \
             {\"sig\":\"nav\",\"outcome\":\"UNKNOWN\",\"checks\":[]}",
        );
        assert_eq!(
            recheck_detached_indicator(&unknown, "nav", Some("key:id:dot")),
            GraphRecheck::NotReached
        );
    }
}
