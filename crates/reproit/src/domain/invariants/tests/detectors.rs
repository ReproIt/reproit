use super::*;

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
    assert!(
        !kinds(&evaluate(&clean, &InvariantsCfg::default())).contains(&"app-invariant".to_string())
    );
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
        crate::domain::oracle::classify(v),
        crate::domain::oracle::Oracle::DuplicateSubmit
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
        crate::domain::oracle::classify(v),
        crate::domain::oracle::Oracle::FocusLoss
    );
    // Disabling the invariant suppresses it.
    let cfg = InvariantsCfg {
        no_focus_loss: false,
        ..Default::default()
    };
    assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-focus-loss".to_string()));
    // A run with no FOCUSLOSS records stays silent.
    let clean = obs_with(&[("s1", &["Todo"])], &[], Some("s1"));
    assert!(
        !kinds(&evaluate(&clean, &InvariantsCfg::default())).contains(&"no-focus-loss".to_string())
    );
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
fn layout_overflow_finding_keeps_exact_replay_identity() {
    let log = concat!(
        "EXPLORE:STATE {\"sig\":\"card\",\"labels\":[\"Message\"]}\n",
        "EXPLORE:OVERFLOW {\"sig\":\"card\",\"version\":1,\"complete\":true,",
        "\"checks\":[{\"subjectKey\":\"key:id:message\",",
        "\"containerKey\":\"key:id:card\",\"authority\":\"exact-layout\",",
        "\"ownership\":\"app\",\"stableSamples\":2,\"transformed\":false,",
        "\"policy\":\"contain\",",
        "\"subjectRect\":{\"left\":4,\"top\":4,\"right\":108,\"bottom\":36},",
        "\"containerRect\":{\"left\":0,\"top\":0,\"right\":100,\"bottom\":40}}]}\n",
    );
    let mut observations = obs_with(&[("card", &["Message"])], &[], Some("card"));
    observations.obs = crate::domain::map::parse_run(log);
    let findings = evaluate(&observations, &InvariantsCfg::default());
    let finding = findings
        .iter()
        .find(|finding| finding["invariant"] == "no-layout-overflow")
        .expect("overflow finding");
    assert_eq!(finding["kind"], "OVERFLOW");
    assert_eq!(finding["selector"], "key:id:message");
    assert!(finding["fingerprint"]
        .as_str()
        .is_some_and(|value| value.starts_with("sha256:")));
    assert_eq!(
        crate::domain::oracle::classify(finding),
        crate::domain::oracle::Oracle::Overflow
    );
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
        crate::domain::oracle::classify(rot),
        crate::domain::oracle::Oracle::Rotation
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
        crate::domain::oracle::classify(bg),
        crate::domain::oracle::Oracle::BackgroundRestore
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
        crate::domain::oracle::classify(v),
        crate::domain::oracle::Oracle::BlankScreen
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
        crate::domain::oracle::classify(v),
        crate::domain::oracle::Oracle::SafeArea
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
        crate::domain::oracle::classify(v),
        crate::domain::oracle::Oracle::PermissionWalk
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
        crate::domain::oracle::classify(leak[0]),
        crate::domain::oracle::Oracle::Leak
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
        crate::domain::oracle::classify(v),
        crate::domain::oracle::Oracle::ZoomReflow
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
        crate::domain::oracle::classify(v),
        crate::domain::oracle::Oracle::ScrollRoundTrip
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
        crate::domain::oracle::classify(v),
        crate::domain::oracle::Oracle::BrokenAsset
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
fn occlusion_findings_dedup_by_control_set_across_signatures() {
    // A stateful single-DOM app re-presents the SAME buried controls under
    // many state signatures. The oracle collapses identical control-sets to
    // one finding (the mytwenda.app demo case), while a genuinely different
    // occlusion set on another state still reports.
    let mut o = obs_with(&[("s1", &["App"])], &[], Some("s1"));
    let buried = vec![
        ("Cancel".to_string(), "input".to_string()),
        ("Pay".to_string(), "button.cta".to_string()),
    ];
    o.obs.occlusions.insert("s1".to_string(), buried.clone());
    o.obs.occlusions.insert("s2".to_string(), buried.clone());
    o.obs.occlusions.insert("s3".to_string(), buried);
    // A distinct set on another state is its own finding.
    o.obs.occlusions.insert(
        "s4".to_string(),
        vec![("Menu".to_string(), "div.sheet".to_string())],
    );
    let occ: Vec<_> = evaluate(&o, &InvariantsCfg::default())
        .into_iter()
        .filter(|x| x["invariant"] == "no-occluded-control")
        .collect();
    assert_eq!(
        occ.len(),
        2,
        "3 identical sets collapse to 1, plus the distinct set"
    );
    assert!(occ.iter().any(|x| x["message"]
        .as_str()
        .unwrap()
        .contains("Cancel under input")));
    assert!(occ.iter().any(|x| x["message"]
        .as_str()
        .unwrap()
        .contains("Menu under div.sheet")));

    let cfg = InvariantsCfg {
        no_occluded_control: false,
        ..Default::default()
    };
    assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-occluded-control".to_string()));
}
