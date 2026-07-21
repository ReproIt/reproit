use super::*;

#[test]
fn merge_captures_route_from_explore_state() {
    // A runner that reports a route (Flutter anchor, web URL path, ...) lands
    // it on the verified state, so the candidate map can reconcile by route.
    let log = r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":["Home"]}"#;
    let obs = parse_run(log);
    assert_eq!(obs.routes.get("abc").map(String::as_str), Some("/home"));
    let mut map = AppMap {
        app: "t".into(),
        schema_version: APP_MAP_SCHEMA_VERSION,
        revision: 1,
        states: BTreeMap::new(),
        transitions: vec![],
        invariants: vec![],
        interrupts: vec![],
    };
    merge(&mut map, &obs);
    let state = map.states.values().next().expect("a merged state");
    assert_eq!(state.signature.route.as_deref(), Some("/home"));
}

#[test]
fn groundtruth_marker_yields_operability_gaps() {
    // The motivating case: a control operable by pointer but not keyboard-
    // reachable and exposing no role (the finding-div in the dashboard). This
    // is the web in-process agent's marker, kept in
    // tests/golden/operability/web.json (sig "abc"); CI re-captures + diffs it.
    let log = format!(
        "{}\nEXPLORE:GROUNDTRUTH {}",
        r#"EXPLORE:STATE {"sig":"abc","labels":[]}"#,
        golden_groundtruth("web"),
    );
    let obs = parse_run(&log);
    let g = obs.gaps.get("abc").expect("gaps for abc");
    assert_eq!(
        g.pointer_only, 1,
        "one operable element not keyboard-activatable"
    );
    assert_eq!(
        g.keyboard_unreachable, 1,
        "one operable element not in tab order"
    );
    assert_eq!(g.no_role, 1, "one operable element with no role");
    assert!(!g.focus_trap);
    // The grounded per-element detail: exactly the one failing element, by
    // selector, tagged with every dimension it fails. This is what the
    // accessibility view/MCP tool serves, so it must be present, not a count.
    assert_eq!(g.items.len(), 1, "only the one failing element is recorded");
    assert_eq!(g.items[0].selector, "role:option#0");
    assert_eq!(
        g.items[0].kinds,
        vec!["pointer_only", "keyboard_unreachable", "no_role"],
        "the failing element is tagged with all three dimensions it fails"
    );
    // The non-operable decoration is never a gap; the healthy nav is not either.
    let mut map = AppMap {
        app: "t".into(),
        schema_version: APP_MAP_SCHEMA_VERSION,
        revision: 1,
        states: BTreeMap::new(),
        transitions: vec![],
        invariants: vec![],
        interrupts: vec![],
    };
    merge(&mut map, &obs);
    let state = map.states.values().next().expect("a merged state");
    assert_eq!(state.operability_gaps.pointer_only, 1);
    assert_eq!(state.operability_gaps.keyboard_unreachable, 1);
}

#[test]
fn rerender_marker_yields_keyed_churn() {
    // A transition that rebuilt persistent chrome which did not change: the
    // runner emits EXPLORE:RERENDER with the from sig, the action, and the
    // churned anchor selectors. parse_run keys it by (from, action). A marker
    // with an empty churned list (no flicker) is dropped, not recorded.
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:RERENDER {"from":"s1","action":"tap:key:id:bad","#,
        r#""churned":["id:hdr","id:nav"]}"#,
        "\n",
        r#"EXPLORE:RERENDER {"from":"s1","action":"tap:key:id:good","churned":[]}"#,
    );
    let obs = parse_run(log);
    assert_eq!(
        obs.rerenders.len(),
        1,
        "only the non-empty churn is recorded"
    );
    let churned = obs
        .rerenders
        .get(&("s1".to_string(), "tap:key:id:bad".to_string()))
        .expect("churn for the bad transition");
    assert_eq!(churned, &vec!["id:hdr".to_string(), "id:nav".to_string()]);
    assert!(
        !obs.rerenders
            .contains_key(&("s1".to_string(), "tap:key:id:good".to_string())),
        "the reconciled (empty-churn) transition is not a flicker"
    );
}

#[test]
fn dupsubmit_marker_yields_keyed_method_url_count() {
    // The opt-in double-dispatch probe: EXPLORE:DUPSUBMIT carries the
    // duplicated (method, url) and how many times it fired, keyed by
    // (from, action). A record missing any field (here: no url) is dropped,
    // never half-recorded.
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:DUPSUBMIT {"from":"s1","action":"tap:key:id:pay","#,
        r#""method":"POST","url":"https://app.example/api/orders","count":2}"#,
        "\n",
        r#"EXPLORE:DUPSUBMIT {"from":"s1","action":"tap:key:id:bad","#,
        r#""method":"POST","count":2}"#,
    );
    let obs = parse_run(log);
    assert_eq!(obs.duplicate_submits.len(), 1, "only the valid payload");
    let rec = obs
        .duplicate_submits
        .get(&("s1".to_string(), "tap:key:id:pay".to_string()))
        .expect("duplicate submit for the pay button");
    assert_eq!(
        rec,
        &(
            "POST".to_string(),
            "https://app.example/api/orders".to_string(),
            2
        )
    );
}

#[test]
fn focusloss_marker_yields_keyed_pairs() {
    // The focus-loss oracle: EXPLORE:FOCUSLOSS is keyed by (from, action);
    // a repeat of the same pair dedupes (set semantics) and a record
    // missing the action is dropped.
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:FOCUSLOSS {"from":"s1","action":"tap:key:id:add"}"#,
        "\n",
        r#"EXPLORE:FOCUSLOSS {"from":"s1","action":"tap:key:id:add"}"#,
        "\n",
        r#"EXPLORE:FOCUSLOSS {"from":"s1"}"#,
    );
    let obs = parse_run(log);
    assert_eq!(obs.focus_losses.len(), 1, "deduped, invalid dropped");
    assert!(obs
        .focus_losses
        .contains(&("s1".to_string(), "tap:key:id:add".to_string())));
}

#[test]
fn flicker_marker_records_peak_divergence() {
    // The gated Tier-2 pixel oracle: EXPLORE:FLICKER carries the peak
    // transient-divergence magnitude, keyed by (from, action).
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:FLICKER {"from":"s1","action":"tap:key:id:bad","peak":0.82,"frames":7}"#,
    );
    let obs = parse_run(log);
    let peak = obs
        .paint_flickers
        .get(&("s1".to_string(), "tap:key:id:bad".to_string()))
        .expect("paint flicker for the bad transition");
    assert!((peak - 0.82).abs() < 1e-9);
}

#[test]
fn stuck_keyboard_marker_records_sig() {
    // The stuck-keyboard oracle: EXPLORE:STUCKKEYBOARD is emitted only on a
    // violation (IME visible, no editable focused), so presence of the sig
    // is the whole record.
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:STUCKKEYBOARD {"sig":"s1","route":"/detail"}"#,
    );
    let obs = parse_run(log);
    assert!(obs.stuck_keyboards.contains("s1"));
    // A marker without a sig is dropped, never recorded as an empty key.
    let obs2 = parse_run(r#"EXPLORE:STUCKKEYBOARD {"route":"/detail"}"#);
    assert!(obs2.stuck_keyboards.is_empty());
}

#[test]
fn rotation_and_bgrestore_markers_key_by_sig() {
    // The lifecycle-metamorphic oracles: EXPLORE:ROTATION / EXPLORE:BGRESTORE
    // carry the pre-transform structural sig (`expected`) and what survived
    // the transform (`got`), keyed by the state signature. A marker missing
    // any of sig/expected/got is dropped.
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:ROTATION {"sig":"s1","route":"/detail","expected":"abc","got":"def"}"#,
        "\n",
        r#"EXPLORE:BGRESTORE {"sig":"s1","route":"/detail","expected":"abc","got":"xyz"}"#,
        "\n",
        r#"EXPLORE:ROTATION {"sig":"s2","expected":"only"}"#,
    );
    let obs = parse_run(log);
    assert_eq!(
        obs.rotation_losses.get("s1"),
        Some(&("abc".to_string(), "def".to_string()))
    );
    assert_eq!(
        obs.background_losses.get("s1"),
        Some(&("abc".to_string(), "xyz".to_string()))
    );
    // A marker missing `got` is dropped (never a half-recorded entry).
    assert!(!obs.rotation_losses.contains_key("s2"));
}

#[test]
fn listenerleak_marker_keys_by_route() {
    // The listener-leak oracle: EXPLORE:LISTENERLEAK carries the per-metric
    // climb (kind, first, last) plus the revisit count, keyed by route. A
    // marker with an empty items list is dropped (silent when the route is
    // stable), and a marker without a route is ignored.
    let log = concat!(
        r#"EXPLORE:LISTENERLEAK {"route":"/detail","visits":5,"items":["#,
        r#"{"kind":"listeners","first":8,"last":40},"#,
        r#"{"kind":"nodes","first":120,"last":180}]}"#,
        "\n",
        r#"EXPLORE:LISTENERLEAK {"route":"/home","visits":5,"items":[]}"#,
        "\n",
        r#"EXPLORE:LISTENERLEAK {"visits":5,"items":["#,
        r#"{"kind":"listeners","first":1,"last":9}]}"#,
    );
    let obs = parse_run(log);
    let (visits, items) = obs.listener_leaks.get("/detail").expect("leak for /detail");
    assert_eq!(*visits, 5);
    assert_eq!(
        items,
        &vec![
            ("listeners".to_string(), 8, 40),
            ("nodes".to_string(), 120, 180),
        ]
    );
    assert!(
        !obs.listener_leaks.contains_key("/home"),
        "an empty listener-leak list is not recorded"
    );
    assert_eq!(
        obs.listener_leaks.len(),
        1,
        "a marker without a route is dropped"
    );
}

#[test]
fn blankscreen_marker_keys_by_sig() {
    // BLANKSCREEN is reportable only with enumerated independent authority.
    // Structural-only and unknown-authority markers abstain, while an
    // authoritative marker carries the root + viewport keyed by signature.
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:BLANKSCREEN {"sig":"candidate","items":[{"key":"tag:body","w":1280,"h":720}]}"#,
        "\n",
        r#"EXPLORE:BLANKSCREEN {"sig":"unknown","authority":"looks-empty","items":[{"key":"tag:body","w":1280,"h":720}]}"#,
        "\n",
        r#"EXPLORE:BLANKSCREEN {"sig":"s1","authority":"first-party-exception","items":[{"key":"tag:body","w":1280,"h":720}]}"#,
        "\n",
        r#"EXPLORE:BLANKSCREEN {"sig":"s2","authority":"renderer-crash","items":[]}"#,
    );
    let obs = parse_run(log);
    let items = obs.blank_screens.get("s1").expect("blank screen for s1");
    assert_eq!(items, &vec![("tag:body".to_string(), 1280, 720)]);
    assert!(!obs.blank_screens.contains_key("candidate"));
    assert!(!obs.blank_screens.contains_key("unknown"));
    assert!(
        !obs.blank_screens.contains_key("s2"),
        "an empty blank-screen list is not recorded"
    );
}

#[test]
fn invariant_marker_keys_app_predicates_by_sig() {
    // The app-invariant oracle: EXPLORE:INVARIANT carries the app's own
    // predicate violations (id, message), keyed by state signature. A
    // marker with an empty items list is dropped (silent when all held),
    // and a missing message defaults to empty.
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:INVARIANT {"sig":"s1","items":["#,
        r#"{"id":"cart total never negative","message":"total was -5"},"#,
        r#"{"id":"tab highlighted"}]}"#,
        "\n",
        r#"EXPLORE:INVARIANT {"sig":"s2","items":[]}"#,
    );
    let obs = parse_run(log);
    let items = obs.app_invariants.get("s1").expect("invariants for s1");
    assert_eq!(
        items,
        &vec![
            (
                "cart total never negative".to_string(),
                "total was -5".to_string()
            ),
            ("tab highlighted".to_string(), String::new()),
        ]
    );
    assert!(
        !obs.app_invariants.contains_key("s2"),
        "an empty invariant list is not recorded"
    );
}

#[test]
fn safearea_marker_keys_collisions_by_sig() {
    // The safe-area oracle: EXPLORE:SAFEAREA carries the controls whose hit
    // rect intersects a device inset (key, edge, overlap px), keyed by state
    // signature. A marker with an empty items list is dropped (silent when no
    // control sits in an inset).
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:SAFEAREA {"sig":"s1","items":["#,
        r#"{"key":"key:done","edge":"top","by":18},"#,
        r#"{"key":"key:next","edge":"bottom","by":6}]}"#,
        "\n",
        r#"EXPLORE:SAFEAREA {"sig":"s2","items":[]}"#,
    );
    let obs = parse_run(log);
    let items = obs.safe_areas.get("s1").expect("safe-area for s1");
    assert_eq!(
        items,
        &vec![
            ("key:done".to_string(), "top".to_string(), 18),
            ("key:next".to_string(), "bottom".to_string(), 6),
        ]
    );
    assert!(
        !obs.safe_areas.contains_key("s2"),
        "an empty safe-area list is not recorded"
    );
}

#[test]
fn wakelock_marker_keys_leaks_by_sig() {
    // The wakelock-leak oracle: EXPLORE:WAKELOCK carries the wakelocks still
    // held after leaving a screen (tag, kind), keyed by the origin state
    // signature. A marker with an empty items list is dropped (silent when a
    // screen releases its locks on leaving).
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"video","labels":[]}"#,
        "\n",
        r#"EXPLORE:WAKELOCK {"sig":"video","items":["#,
        r#"{"tag":"com.app:VideoPlayback","kind":"wakelock"},"#,
        r#"{"tag":"KEEP_SCREEN_ON","kind":"keep-screen-on"}]}"#,
        "\n",
        r#"EXPLORE:WAKELOCK {"sig":"home","items":[]}"#,
    );
    let obs = parse_run(log);
    let items = obs.wakelock_leaks.get("video").expect("leak for video");
    assert_eq!(
        items,
        &vec![
            ("com.app:VideoPlayback".to_string(), "wakelock".to_string()),
            ("KEEP_SCREEN_ON".to_string(), "keep-screen-on".to_string()),
        ]
    );
    assert!(
        !obs.wakelock_leaks.contains_key("home"),
        "an empty wakelock list is not recorded"
    );
}

#[test]
fn permissionwalk_marker_records_permission_by_sig() {
    // The permission-walk oracle: EXPLORE:PERMISSIONWALK marks a screen
    // reached after a permission denial, keyed by state signature; the value
    // is the denied permission. A marker without both a sig and a permission
    // is dropped.
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:PERMISSIONWALK {"sig":"s1","permission":"camera","route":"/scan"}"#,
    );
    let obs = parse_run(log);
    assert_eq!(
        obs.permission_screens.get("s1").map(String::as_str),
        Some("camera")
    );
    let obs2 = parse_run(r#"EXPLORE:PERMISSIONWALK {"sig":"s1"}"#);
    assert!(obs2.permission_screens.is_empty());
}

#[test]
fn brokenasset_marker_keys_dead_assets_by_sig() {
    // The broken-asset oracle: EXPLORE:BROKENASSET carries the dead
    // subresources (key, reason, detail), keyed by state signature. A marker
    // with an empty items list is dropped (silent when every asset loads).
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:BROKENASSET {"sig":"s1","items":["#,
        r#"{"key":"key:id:hero","reason":"img","detail":"missing.png"},"#,
        r#"{"key":"font:BrokeFont","reason":"font","detail":"BrokeFont"},"#,
        r#"{"key":"key:id:desc","reason":"tofu","detail":"glitch � here"}]}"#,
        "\n",
        r#"EXPLORE:BROKENASSET {"sig":"s2","items":[]}"#,
    );
    let obs = parse_run(log);
    let items = obs.broken_assets.get("s1").expect("broken assets for s1");
    assert_eq!(
        items,
        &vec![
            (
                "key:id:hero".to_string(),
                "img".to_string(),
                "missing.png".to_string()
            ),
            (
                "font:BrokeFont".to_string(),
                "font".to_string(),
                "BrokeFont".to_string()
            ),
            (
                "key:id:desc".to_string(),
                "tofu".to_string(),
                "glitch \u{FFFD} here".to_string()
            ),
        ]
    );
    assert!(
        !obs.broken_assets.contains_key("s2"),
        "an empty broken-asset list is not recorded"
    );
}

#[test]
fn auth_input_purpose_marker_contract_is_locale_and_backend_independent() {
    let log = concat!(
        "EXPLORE:STATE {\"sig\":\"web\",\"labels\":[\"Correo \
             electrónico\"],\"elements\":[{\"sel\":\"key:email\",\"role\":\"textfield\",\"label\":\
             \"Correo electrónico\",\"inputPurpose\":\"email-address\"}]}\n",
        "EXPLORE:STATE {\"sig\":\"native\",\"labels\":[\"Код \
             подтверждения\"],\"elements\":[{\"sel\":\"key:otp\",\
             \"role\":\"textfield\",\"label\":\
             \"Код подтверждения\",\"inputPurpose\":\"one-time-code\"}]}\n",
        "EXPLORE:STATE \
             {\"sig\":\"instrumented\",\"labels\":[],\"elements\":[{\"sel\":\"key:\
             reproit-purpose-phone--login\",\"role\":\"textfield\",\"label\":\"\"}]}\n"
    );
    let obs = parse_run(log);
    assert_eq!(
        obs.elements["web"][0].input_purpose.as_deref(),
        Some("email")
    );
    assert_eq!(
        obs.elements["native"][0].input_purpose.as_deref(),
        Some("otp")
    );
    assert_eq!(
        obs.elements["instrumented"][0].input_purpose.as_deref(),
        Some("phone")
    );
}

#[test]
fn zoomreflow_marker_keys_breaks_by_sig() {
    // The zoom-reflow (WCAG 1.4.10) oracle: EXPLORE:ZOOMREFLOW carries the
    // reflow breaks (key, kind, by) measured at the zoomed viewport, keyed
    // by state signature. A marker with an empty items list is dropped
    // (silent when the route reflows cleanly).
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:ZOOMREFLOW {"sig":"s1","items":["#,
        r#"{"key":"tag:html","kind":"hscroll","by":560},"#,
        r#"{"key":"key:id:save","kind":"collapsed","by":0}]}"#,
        "\n",
        r#"EXPLORE:ZOOMREFLOW {"sig":"s2","items":[]}"#,
    );
    let obs = parse_run(log);
    let items = obs.zoom_reflows.get("s1").expect("zoom reflow for s1");
    assert_eq!(
        items,
        &vec![
            ("tag:html".to_string(), "hscroll".to_string(), 560),
            ("key:id:save".to_string(), "collapsed".to_string(), 0),
        ]
    );
    assert!(
        !obs.zoom_reflows.contains_key("s2"),
        "an empty zoom-reflow list is not recorded"
    );
}

#[test]
fn scrollroundtrip_marker_keys_diffs_by_sig() {
    // The scroll-round-trip oracle: EXPLORE:SCROLLROUNDTRIP carries the
    // per-offset (pos, before, after) content mismatches observed after
    // scrolling a list away and back, keyed by state signature. A marker
    // with an empty items list is dropped (silent when the list is stable).
    let log = concat!(
        r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
        "\n",
        r#"EXPLORE:SCROLLROUNDTRIP {"sig":"s1","items":["#,
        r#"{"pos":"y=0","before":"Alpha|Bravo","after":"Charlie|Delta"}]}"#,
        "\n",
        r#"EXPLORE:SCROLLROUNDTRIP {"sig":"s2","items":[]}"#,
    );
    let obs = parse_run(log);
    let items = obs
        .scroll_round_trips
        .get("s1")
        .expect("scroll round trip for s1");
    assert_eq!(
        items,
        &vec![(
            "y=0".to_string(),
            "Alpha|Bravo".to_string(),
            "Charlie|Delta".to_string()
        )]
    );
    assert!(
        !obs.scroll_round_trips.contains_key("s2"),
        "an empty scroll-round-trip list is not recorded"
    );
}

#[test]
fn appkit_in_process_agent_groundtruth_detects_fake_button_gap() {
    // End-to-end contract proof for the in-process AppKit operability agent
    // (runners/native/appkit-agent/main.swift). This is the VERBATIM
    // EXPLORE:GROUNDTRUTH line that the built+run Swift agent emits for a
    // window holding a real NSButton, a "fake button" (custom NSView with a
    // click gesture + handler and no a11y role), and a correctly-built
    // accessible custom control. The engine must score exactly one gap row
    // (the fake button), failing all three a11y dimensions. The marker lives
    // in tests/golden/operability/appkit.json; CI re-captures + diffs it.
    let g = gaps_from_golden("appkit");
    // The fake button alone is an operable-but-inaccessible element.
    assert_eq!(g.no_role, 1, "fake button has no a11y role");
    assert_eq!(
        g.keyboard_unreachable, 1,
        "fake button is not in the key-view loop"
    );
    assert_eq!(g.pointer_only, 1, "fake button is pointer-only (gesture)");
    assert!(!g.focus_trap);
}

#[test]
fn wpf_in_process_agent_groundtruth_detects_fake_button_gap() {
    // End-to-end contract proof for the in-process WPF operability agent
    // (runners/native/wpf-agent/Program.cs). This is the VERBATIM
    // EXPLORE:GROUNDTRUTH line that the built+run agent emits on the Windows
    // VM for a window holding a real <Button> and a "fake button" (a
    // clickable <Border>/<TextBlock> with a MouseLeftButtonUp handler and no
    // Button role / no AutomationProperties). Graph 1 (visual tree + handler
    // reflection) and graph 2 (UIElementAutomationPeer) are joined by object
    // identity. The engine must score exactly one gap row (the fake button),
    // failing all three a11y dimensions; the real Button is clean. The marker
    // lives in tests/golden/operability/wpf.json; CI re-captures + diffs it.
    let g = gaps_from_golden("wpf");
    assert_eq!(g.no_role, 1, "fake button has no Button role");
    assert_eq!(
        g.keyboard_unreachable, 1,
        "fake button is not in the tab order"
    );
    assert_eq!(
        g.pointer_only, 1,
        "fake button is pointer-only (mouse handler)"
    );
    assert!(!g.focus_trap);
}

#[test]
fn qt_in_process_agent_groundtruth_detects_fake_button_gap() {
    // End-to-end contract proof for the in-process Qt operability agent
    // (runners/native/qt-agent/qt_agent.cpp). This is the VERBATIM
    // EXPLORE:GROUNDTRUTH line the built+run agent emits on Linux
    // (Qt 6.8.2, `QT_QPA_PLATFORM=offscreen`) for a window
    // holding a real QPushButton, a "fake button" (custom QWidget with a
    // mousePressEvent handler and no QAccessible role), and a correctly-built
    // accessible control. Graph 1 (QObject tree + wired signals / custom
    // subclass) joins graph 2 (QAccessibleInterface) by object identity. The
    // engine must score exactly one gap row (the fake button), failing all
    // three a11y dimensions; the real button is clean. The signature matches
    // the AppKit agent's (3854aea0): same three-control structural descriptor.
    // The marker lives in tests/golden/operability/qt.json; CI re-captures it.
    let g = gaps_from_golden("qt");
    assert_eq!(g.no_role, 1, "fake button has no QAccessible role");
    assert_eq!(
        g.keyboard_unreachable, 1,
        "fake button is not in the tab order"
    );
    assert_eq!(
        g.pointer_only, 1,
        "fake button is pointer-only (mousePressEvent)"
    );
    assert!(!g.focus_trap);
}

#[test]
fn gtk_in_process_agent_groundtruth_detects_fake_button_gap() {
    // End-to-end contract proof for the in-process GTK operability agent
    // (runners/native/gtk-agent/gtk_agent.c). This is the VERBATIM
    // EXPLORE:GROUNDTRUTH line the built+run agent emits on Linux
    // (GTK 4.18.6, under `xvfb-run`) for a window holding a real
    // GtkButton, a "fake button" (a GtkBox carrying a GtkGestureClick +
    // handler with no button role / not focusable), and a correctly-built
    // accessible GtkButton. Graph 1 (GtkWidget tree + wired signals / click
    // gestures) joins graph 2 (GtkAccessible role/state) by object identity.
    // The fake button is the motivating finding: operable yet rolePresent
    // false and keyboard-unreachable. GTK4 also surfaces the window's
    // built-in click gesture (role:group#0, a focusless operable element) and
    // the buttons' inner GtkLabel children (operable:false, never gaps); the
    // engine counts every operable-but-inaccessible element, so no_role==1
    // (the fake button alone has no role) while the two focusless operable
    // elements (window + fake button) drive keyboard_unreachable/pointer_only.
    // The marker lives in tests/golden/operability/gtk.json; CI re-captures it.
    let g = gaps_from_golden("gtk");
    // The fake button is the only operable element with no accessible role.
    assert_eq!(g.no_role, 1, "fake button alone has no GtkAccessible role");
    // Two operable elements lack focus/keyboard reachability: the fake button
    // and GTK4's window-level click gesture; the real + good buttons are clean.
    assert_eq!(g.keyboard_unreachable, 2);
    assert_eq!(g.pointer_only, 2);
    assert!(!g.focus_trap);
}

#[test]
fn flutter_in_process_agent_groundtruth_detects_fake_button_gap() {
    // End-to-end contract proof for the in-process Flutter operability agent
    // (sdk/reproit_flutter/.../operability_fixture_test.dart's groundTruth()).
    // This is the VERBATIM EXPLORE:GROUNDTRUTH line `flutter test` emits for
    // the operability fixture: a real ElevatedButton (clean) and a "fake
    // button" (a bare GestureDetector(onTap:) wrapping Text). Flutter's
    // semantics DO give the gesture a synthetic button role (rolePresent:true,
    // gestureKind "tap"), so the gap is NOT no_role; the fake button is the
    // motivating finding because it is operable by pointer yet has no Focus, so
    // it is keyboard-unreachable AND not keyboard-activatable. The marker lives
    // in tests/golden/operability/flutter.json and is RE-CAPTURED by the CI
    // capture-diff job (`flutter test`); see .github/workflows/ci.yml.
    let g = gaps_from_golden("flutter");
    // Flutter exposes the gesture's button role, so there is no no_role gap.
    assert_eq!(g.no_role, 0, "flutter gives the gesture a button role");
    assert_eq!(
        g.keyboard_unreachable, 1,
        "fake button has no Focus -> not in the tab order"
    );
    assert_eq!(
        g.pointer_only, 1,
        "fake button is pointer-only (onTap, not keyboard-activatable)"
    );
    assert!(!g.focus_trap);
}
