use super::*;

mod markers;
mod persistence_tests;

/// The VERBATIM `EXPLORE:GROUNDTRUTH` JSON each in-process operability
/// agent emits, kept in ONE shared place:
/// `tests/golden/operability/<platform>.json` (byte-for-byte the marker
/// the real agent prints). The engine contract tests below read these
/// goldens instead of inlining the literal, and a per-platform
/// capture-diff CI job (.github/workflows/ci.yml) re-runs the real agent,
/// drops the volatile `sig`, and DIFFs the live marker against the same
/// golden. So the golden is the single source of truth: if the real
/// marker drifts, the test keeps asserting the old contract here while
/// the CI diff catches the drift against production, instead of an
/// inline literal silently going stale.
fn golden_groundtruth(platform: &str) -> &'static str {
    match platform {
        "web" => include_str!("../../../tests/golden/operability/web.json"),
        "appkit" => include_str!("../../../tests/golden/operability/appkit.json"),
        "wpf" => include_str!("../../../tests/golden/operability/wpf.json"),
        "qt" => include_str!("../../../tests/golden/operability/qt.json"),
        "gtk" => include_str!("../../../tests/golden/operability/gtk.json"),
        "flutter" => include_str!("../../../tests/golden/operability/flutter.json"),
        other => panic!("no operability golden for platform {other:?}"),
    }
    .trim()
}

/// Parse a platform's golden marker through the real engine, returning the
/// state's operability gaps. The golden carries the marker's own `sig`, so
/// we read it back out of the JSON rather than hard-coding it at each
/// call site.
fn gaps_from_golden(platform: &str) -> OperabilityGaps {
    let payload = golden_groundtruth(platform);
    let sig = serde_json::from_str::<Value>(payload)
        .expect("golden is valid JSON")
        .get("sig")
        .and_then(Value::as_str)
        .expect("golden carries a sig")
        .to_string();
    let log = format!("EXPLORE:GROUNDTRUTH {payload}");
    parse_run(&log)
        .gaps
        .get(&sig)
        .unwrap_or_else(|| panic!("gaps for the {platform} agent state ({sig})"))
        .clone()
}

fn st(desc: &str) -> State {
    State {
        name: None,
        description: desc.to_string(),
        signature: StateSignature {
            screenshot_phash: None,
            semantics_hash: None,
            route: None,
        },
        elements: vec![],
        texts: vec![],
        parameters: vec![],
        operability_gaps: Default::default(),
    }
}
fn tap(from: &str, label: &str, to: &str) -> Transition {
    Transition {
        from: from.to_string(),
        to: to.to_string(),
        action: Action::Tap {
            finder: label.to_string(),
        },
        guards: vec![],
        reversibility: Reversibility::ProposedReversible,
        expected: None,
    }
}
fn sample() -> AppMap {
    let mut states = BTreeMap::new();
    states.insert("Home".to_string(), st("home screen"));
    states.insert("Settings".to_string(), st("settings screen"));
    states.insert("About".to_string(), st("about / version info"));
    AppMap {
        app: "demo".to_string(),
        schema_version: APP_MAP_SCHEMA_VERSION,
        revision: 1,
        states,
        transitions: vec![
            tap("Home", "Settings", "Settings"),
            tap("Settings", "About", "About"),
        ],
        invariants: vec![],
        interrupts: vec![],
    }
}

#[test]
fn entry_is_the_state_without_incoming_edges() {
    assert_eq!(entry_state(&sample()).as_deref(), Some("Home"));
}

#[test]
fn graph_index_exposes_action_lookup_and_state_summaries() {
    let map = sample();
    let graph = GraphIndex::new(&map);
    let action = Action::Tap {
        finder: "Settings".to_string(),
    };
    let matches = graph.transitions_for_action("Home", &action);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].to, "Settings");
    assert_eq!(graph.summary("Home").outgoing, 1);
    assert_eq!(graph.summary("Home").distinct_actions, 1);
}

#[test]
fn graph_guidance_finds_components_and_dominator_reach() {
    let mut map = sample();
    map.states.insert("Loop".to_string(), st("loop"));
    map.transitions.push(tap("About", "Loop", "Loop"));
    map.transitions.push(tap("Loop", "About", "About"));
    let graph = GraphIndex::new(&map);
    let guidance = GraphGuidance::analyze(&graph, "Home");

    assert_eq!(guidance.component_members("About"), &["About", "Loop"]);
    assert_eq!(guidance.dominated_count("Settings"), 2);
    assert_eq!(guidance.dominated_count("About"), 1);
}

#[test]
fn graph_guidance_handles_the_maximum_dominator_graph() {
    let count = 1_024;
    let states = (0..count)
        .map(|index| (format!("state-{index:04}"), st("chain state")))
        .collect();
    let transitions = (0..count - 1)
        .map(|index| {
            tap(
                &format!("state-{index:04}"),
                "next",
                &format!("state-{:04}", index + 1),
            )
        })
        .collect();
    let map = AppMap {
        app: "bounded-chain".to_string(),
        schema_version: APP_MAP_SCHEMA_VERSION,
        revision: 1,
        states,
        transitions,
        invariants: vec![],
        interrupts: vec![],
    };
    let graph = GraphIndex::new(&map);
    let guidance = GraphGuidance::analyze(&graph, "state-0000");

    assert_eq!(guidance.dominated_count("state-0000"), count - 1);
    assert_eq!(guidance.dominated_count("state-0512"), count - 513);
    assert_eq!(guidance.dominated_count("state-1023"), 0);
}

#[test]
fn frontier_prefers_a_state_that_unlocks_more_reachable_graph() {
    let sig_state = |sig: &str| {
        let mut state = st("state");
        state.signature.semantics_hash = Some(sig.to_string());
        state
    };
    let states = ["Home", "Gate", "DeepA", "DeepB", "Leaf"]
        .into_iter()
        .map(|state| (state.to_string(), sig_state(&format!("sig-{state}"))))
        .collect();
    let map = AppMap {
        app: "demo".to_string(),
        schema_version: APP_MAP_SCHEMA_VERSION,
        revision: 1,
        states,
        transitions: vec![
            tap("Home", "gate", "Gate"),
            tap("Gate", "a", "DeepA"),
            tap("Gate", "b", "DeepB"),
            tap("Home", "leaf", "Leaf"),
        ],
        invariants: vec![],
        interrupts: vec![],
    };
    let visits = Visits {
        map_revision: 1,
        start: Some("sig-Home".to_string()),
        ..Visits::default()
    };

    let (target, path) = frontier_path(&map, &visits).unwrap();
    assert_eq!(target, "Gate");
    assert_eq!(path, vec!["tap:gate"]);
}

#[test]
fn path_to_label_finds_shortest_action_sequence() {
    let m = sample();
    let (target, path) = path_to_label(&m, "about").expect("About is reachable");
    assert_eq!(target, "About");
    assert_eq!(
        path,
        vec!["tap:Settings".to_string(), "tap:About".to_string()]
    );
    // the entry state itself matching yields an empty path.
    let (t0, p0) = path_to_label(&m, "home").unwrap();
    assert_eq!(t0, "Home");
    assert!(p0.is_empty());
    // an unreachable/unknown label yields None.
    assert!(path_to_label(&m, "nonexistent-screen").is_none());
}

#[test]
fn human_name_is_searchable_without_changing_structural_identity() {
    let mut map = sample();
    map.states.get_mut("Home").unwrap().name = Some("launch_pad".to_string());

    let (target, path) = path_to_label(&map, "launch").unwrap();
    assert_eq!(target, "Home");
    assert!(path.is_empty());
    assert!(map.states.contains_key("Home"));
}

#[test]
fn frontier_path_is_deterministic_on_ties() {
    // Two unvisited frontier states, each one tap from Home: equal visit count
    // AND equal path length, so the pick comes down to the tie-break. Before
    // the fix it resolved on `HashMap` iteration order (a fresh random seed per
    // call), so `fuzz --frontier` could target a different state run-to-run.
    let sig_state = |sig: &str| {
        let mut s = st("x");
        s.signature.semantics_hash = Some(sig.to_string());
        s
    };
    let mut states = BTreeMap::new();
    states.insert("Home".to_string(), sig_state("sig-home"));
    states.insert("Alpha".to_string(), sig_state("sig-alpha"));
    states.insert("Bravo".to_string(), sig_state("sig-bravo"));
    let map = AppMap {
        app: "demo".to_string(),
        schema_version: APP_MAP_SCHEMA_VERSION,
        revision: 1,
        states,
        transitions: vec![tap("Home", "a", "Alpha"), tap("Home", "b", "Bravo")],
        invariants: vec![],
        interrupts: vec![],
    };
    let visits = Visits {
        map_revision: map.revision,
        start: Some("sig-home".to_string()),
        counts: BTreeMap::new(),
        edge_counts: BTreeMap::new(),
    };
    // Stable across many calls (each rebuilds the internal HashMaps with a new
    // seed, so a non-deterministic tie-break would diverge over the loop)...
    let first = frontier_path(&map, &visits).expect("a frontier exists");
    for _ in 0..64 {
        assert_eq!(frontier_path(&map, &visits), Some(first.clone()));
    }
    // ...and it is the smallest-signature tied state (sig-alpha < sig-bravo),
    // not whichever happened to hash first.
    assert_eq!(first.0, "Alpha");
}

#[test]
fn frontier_path_handles_a_ten_thousand_state_chain() {
    const STATE_COUNT: usize = 10_000;
    let mut map = AppMap::empty("scaling".to_string());
    for index in 0..STATE_COUNT {
        let id = format!("s_{index:05}");
        let mut state = st("chain");
        state.signature.semantics_hash = Some(format!("sig-{index:05}"));
        map.states.insert(id, state);
    }
    for index in 0..STATE_COUNT - 1 {
        map.transitions.push(tap(
            &format!("s_{index:05}"),
            "next",
            &format!("s_{:05}", index + 1),
        ));
    }
    let visits = Visits {
        map_revision: map.revision,
        start: Some("sig-00000".to_string()),
        ..Visits::default()
    };

    let (target, path) = frontier_path(&map, &visits).unwrap();
    assert_eq!(target, "s_09999");
    assert_eq!(path.len(), STATE_COUNT - 1);
}

#[test]
fn parse_action_recovers_typed_scroll_key_system_edges() {
    // type:/scroll:/key:/system: must round-trip into their real variants, not
    // collapse to Back (which lost the finder/value of form-driven edges).
    assert!(matches!(parse_action("tap:Go"), Some(Action::Tap { .. })));
    match parse_action("type:role:textfield#0=hello") {
        Some(Action::Type { finder, text }) => {
            assert_eq!(finder, "role:textfield#0");
            assert!(text.is_empty(), "raw typed values must not enter the map");
        }
        a => panic!("expected Type, got {a:?}"),
    }
    match parse_action("scroll:key:list=-300") {
        Some(Action::Scroll { finder, dy }) => {
            assert_eq!(finder, "key:list");
            assert_eq!(dy, -300);
        }
        a => panic!("expected Scroll, got {a:?}"),
    }
    match parse_action("system:back") {
        Some(Action::System { event }) => assert_eq!(event, "back"),
        a => panic!("expected System, got {a:?}"),
    }
    let key = parse_action("key:Down").expect("key action parses");
    assert_eq!(
        key,
        Action::Key {
            key: "Down".to_string()
        }
    );
    assert_eq!(action_str(&key), "key:Down");
    assert!(matches!(parse_action("back"), Some(Action::Back)));
    // A typed edge with no `=value` still parses as Type (empty text), not Back.
    assert!(matches!(
        parse_action("type:key:x"),
        Some(Action::Type { .. })
    ));
    assert!(parse_action("unknown").is_none());
    assert!(parse_action("key:").is_none());
    assert!(parse_action("scroll:key:list=wat").is_none());
}

#[test]
fn merge_persists_tui_key_transition() {
    let mut map = AppMap::empty("tui-demo".to_string());
    let mut visits = Visits::default();
    let log = concat!(
        "EXPLORE:STATE {\"sig\":\"list\",\"labels\":[\"List\"]}\n",
        "EXPLORE:STATE {\"sig\":\"selected\",\"labels\":[\"Selected\"]}\n",
        "EXPLORE:EDGE {\"from\":\"list\",\"action\":\"key:Down\",",
        "\"to\":\"selected\"}\n",
    );

    absorb_run_inmem(&mut map, &mut visits, log);

    assert_eq!(map.transitions.len(), 1);
    let transition = &map.transitions[0];
    assert_eq!(transition.from, "s_list");
    assert_eq!(transition.to, "s_selected");
    assert_eq!(
        transition.action,
        Action::Key {
            key: "Down".to_string()
        }
    );
    let json = serde_json::to_string(&transition.action).unwrap();
    assert_eq!(json, r#"{"kind":"key","key":"Down"}"#);
    assert_eq!(
        serde_json::from_str::<Action>(&json).unwrap(),
        transition.action
    );
}

#[test]
fn unsupported_edge_summary_is_bounded_and_omits_action_payloads() {
    let obs = parse_run(concat!(
        "EXPLORE:STATE {\"sig\":\"a\",\"labels\":[\"A\"]}\n",
        "EXPLORE:STATE {\"sig\":\"b\",\"labels\":[\"B\"]}\n",
        "EXPLORE:EDGE {\"from\":\"a\",\"action\":\"key:Down\",\"to\":\"b\"}\n",
        "EXPLORE:EDGE {\"from\":\"a\",\"action\":\"key:\",\"to\":\"b\"}\n",
        "EXPLORE:EDGE {\"from\":\"a\",\"action\":\"future:secret\",\"to\":\"b\"}\n",
    ));

    let (count, kinds) = unsupported_edge_summary(&obs);

    assert_eq!(count, 2);
    assert_eq!(
        kinds,
        BTreeSet::from(["future".to_string(), "key".to_string()])
    );
    assert!(!kinds.iter().any(|kind| kind.contains("secret")));
}

#[test]
fn merge_deduplicates_one_run_and_abstains_on_unknown_actions() {
    let mut map = AppMap::empty("demo".to_string());
    let mut visits = Visits::default();
    let log = concat!(
        "EXPLORE:STATE {\"sig\":\"a\",\"labels\":[\"A\"]}\n",
        "EXPLORE:STATE {\"sig\":\"b\",\"labels\":[\"B\"]}\n",
        "EXPLORE:EDGE {\"from\":\"a\",\"action\":\"tap:key:go\",\"to\":\"b\"}\n",
        "EXPLORE:EDGE {\"from\":\"a\",\"action\":\"tap:key:go\",\"to\":\"b\"}\n",
        "EXPLORE:EDGE {\"from\":\"a\",\"action\":\"mystery\",\"to\":\"b\"}\n",
    );
    absorb_run_inmem(&mut map, &mut visits, log);
    assert_eq!(map.transitions.len(), 1);
    assert_eq!(map.revision, 2);
    assert!(map.states.contains_key("s_a"));
    assert!(map.states.contains_key("s_b"));

    absorb_run_inmem(&mut map, &mut visits, log);
    assert_eq!(map.transitions.len(), 1);
    assert_eq!(map.revision, 2, "an identical merge is not a graph change");
}

#[test]
fn malformed_structural_evidence_abstains_from_graph_invariants() {
    let obs = parse_run(concat!(
        "EXPLORE:STATE {\"sig\":\"a\",\"labels\":[]}\n",
        "EXPLORE:PERMISSIONWALK {\"sig\":\"a\",\"permission\":\"camera\"}\n",
        "EXPLORE:EDGE {malformed}\n",
    ));
    assert!(obs.states.is_empty());
    assert!(obs.permission_screens.is_empty());
}

#[test]
fn legacy_version_deserializes_as_a_graph_revision() {
    let map: AppMap = serde_json::from_str(
        r#"{"app":"demo","version":7,"states":{},"transitions":[],"invariants":[]}"#,
    )
    .unwrap();
    assert_eq!(map.schema_version, 1);
    assert_eq!(map.revision, 7);

    let serialized = serde_json::to_value(AppMap::empty("demo".to_string())).unwrap();
    assert_eq!(serialized["version"], 1);
    assert!(serialized.get("revision").is_none());
}

#[test]
fn edges_summary_lists_real_transitions() {
    assert!(edges_summary(&sample())
        .iter()
        .any(|e| e == "Home --tap:Settings--> Settings"));
}

#[test]
fn edge_weights_caps_the_visit_count_so_hub_actions_keep_a_floor() {
    // A hub destination visited far more than the cap must not decay the
    // edge weight toward zero: the count feeding 1/(1+count) is clamped to
    // VISIT_WEIGHT_CAP, so the walk can still reach it.
    let sig_state = |sig: &str| {
        let mut s = st("x");
        s.signature.semantics_hash = Some(sig.to_string());
        s
    };
    let mut states = BTreeMap::new();
    states.insert("A".to_string(), sig_state("sigA"));
    states.insert("B".to_string(), sig_state("sigB"));
    let map = AppMap {
        app: "demo".to_string(),
        schema_version: APP_MAP_SCHEMA_VERSION,
        revision: 1,
        states,
        transitions: vec![tap("A", "go", "B")],
        invariants: vec![],
        interrupts: vec![],
    };
    let mut visits = Visits::default();
    visits.counts.insert("sigB".to_string(), 1000); // wildly over-visited hub
    let ew = visits.edge_weights(&map);
    let count = *ew
        .get("sigA")
        .and_then(|m| m.values().next())
        .expect("an edge from sigA");
    assert_eq!(
        count, VISIT_WEIGHT_CAP,
        "the weighting count must be capped, not the raw 1000"
    );
}
