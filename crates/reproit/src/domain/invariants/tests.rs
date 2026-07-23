use super::*;
use std::collections::BTreeMap;

mod detectors;

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
        crate::domain::oracle::classify(&f),
        crate::domain::oracle::Oracle::Flicker
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
        crate::domain::oracle::classify(hit),
        crate::domain::oracle::Oracle::StuckKeyboard
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
        crate::domain::oracle::classify(hit),
        crate::domain::oracle::Oracle::WakeLock
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
            zero_contrast: Default::default(),
            dead_inputs: Default::default(),
            overflow_checks: Default::default(),
            relations: Default::default(),
            relation_checks: Default::default(),
            accessibility_state_checks: Default::default(),
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
    use crate::adapters::config::CustomInvariant;
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
        vec![crate::domain::map::RelationViolation {
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
fn detached_indicator_recheck_distinguishes_evidence_states() {
    let log = concat!(
        "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[\"Liked You\"]}\n",
        "EXPLORE:RELATIONSTATUS {\"sig\":\"nav\",\"outcome\":\"SATISFIED\",\"checks\":[",
        "{\"kind\":\"indicator-anchor\",\"dependentKey\":\"key:id:dot\",",
        "\"ownerKey\":\"key:id:liked\",\"containerKey\":\"key:id:tabs\",",
        "\"outcome\":\"SATISFIED\"}]}\n",
    );
    let satisfied = crate::domain::map::parse_run(log);
    assert_eq!(
        recheck_detached_indicator(&satisfied, "nav", "key:id:dot"),
        GraphRecheck::Fixed
    );
    assert_eq!(
        recheck_detached_indicator(&satisfied, "nav", "key:id:other"),
        GraphRecheck::NotReached
    );
    let abstain = crate::domain::map::parse_run(
        "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[]}\nEXPLORE:RELATIONSTATUS \
             {\"sig\":\"nav\",\"outcome\":\"ABSTAIN\",\"checks\":[]}",
    );
    assert_eq!(
        recheck_detached_indicator(&abstain, "nav", "key:id:dot"),
        GraphRecheck::NotReached
    );
}

#[test]
fn accessibility_state_finding_and_recheck_require_exact_fingerprint() {
    const FINGERPRINT: &str = "sha256:f264f36f3b511e4ae5993d43";
    let violating_log = concat!(
        "EXPLORE:STATE {\"sig\":\"settings\",\"labels\":[]}\n",
        "EXPLORE:A11YSTATESTATUS {\"sig\":\"settings\",\"outcome\":\"VIOLATION\",\"checks\":[",
        "{\"identity\":\"key:id:notifications\",\"property\":\"checked\",",
        "\"fingerprint\":\"sha256:f264f36f3b511e4ae5993d43\",\"expected\":\"true\",",
        "\"actual\":\"false\",\"outcome\":\"VIOLATION\",",
        "\"reason\":\"semantic-state-mismatch\"}]}\n",
    );
    let mut observations = obs_with(&[("settings", &[])], &[], Some("settings"));
    observations.obs = crate::domain::map::parse_run(violating_log);
    let findings = evaluate(&observations, &InvariantsCfg::default());
    let finding = findings
        .iter()
        .find(|finding| finding["invariant"] == "no-accessibility-state-mismatch")
        .expect("accessibility-state finding");
    assert_eq!(finding["kind"], "A11YSTATE");
    assert_eq!(finding["selector"], "key:id:notifications");
    assert_eq!(finding["fingerprint"], FINGERPRINT);
    assert_eq!(
        crate::domain::oracle::classify(finding),
        crate::domain::oracle::Oracle::AccessibilityState
    );
    assert_eq!(
        recheck_accessibility_state(&observations.obs, "settings", FINGERPRINT),
        GraphRecheck::StillViolating
    );

    let satisfied = crate::domain::map::parse_run(
        &violating_log
            .replace("\"actual\":\"false\"", "\"actual\":\"true\"")
            .replace("\"outcome\":\"VIOLATION\"", "\"outcome\":\"SATISFIED\"")
            .replace(",\"reason\":\"semantic-state-mismatch\"", ""),
    );
    assert_eq!(
        recheck_accessibility_state(&satisfied, "settings", FINGERPRINT),
        GraphRecheck::Fixed
    );
    assert_eq!(
        recheck_accessibility_state(&satisfied, "settings", "sha256:000000000000000000000000"),
        GraphRecheck::NotReached
    );
}
