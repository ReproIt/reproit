use super::*;

#[test]
fn merge_backfills_route_on_a_known_state() {
    // First run had no route; a later run that reports one backfills it.
    let mut map = AppMap {
        app: "t".into(),
        schema_version: APP_MAP_SCHEMA_VERSION,
        revision: 1,
        states: BTreeMap::new(),
        transitions: vec![],
        invariants: vec![],
        interrupts: vec![],
    };
    merge(
        &mut map,
        &parse_run(r#"EXPLORE:STATE {"sig":"abc","labels":[]}"#),
    );
    assert!(map
        .states
        .values()
        .next()
        .unwrap()
        .signature
        .route
        .is_none());
    merge(
        &mut map,
        &parse_run(r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":[]}"#),
    );
    assert_eq!(
        map.states
            .values()
            .next()
            .unwrap()
            .signature
            .route
            .as_deref(),
        Some("/home")
    );
}

#[test]
fn read_all_device_logs_unions_every_actor() {
    let dir = std::env::temp_dir().join(format!("reproit-maplogs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("drive-a.log"), "EXPLORE:STATE a-line").unwrap();
    std::fs::write(dir.join("drive-b.log"), "EXPLORE:STATE b-line").unwrap();
    // A non-device file must be ignored.
    std::fs::write(dir.join("other.log"), "ignore me").unwrap();
    let joined = read_all_device_logs(&dir).unwrap();
    assert!(joined.contains("a-line"), "device a's log is included");
    assert!(joined.contains("b-line"), "device b's log is included");
    assert!(
        !joined.contains("ignore me"),
        "non-device logs are excluded"
    );
    // Sorted by name: a before b.
    assert!(joined.find("a-line").unwrap() < joined.find("b-line").unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn absorb_run_writes_map_files_to_documented_layout() {
    let root = std::env::temp_dir().join(format!(
        "reproit-map-layout-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let loaded = crate::adapters::config::parse_str(
        "app:\n  platform: web\n  bundleId: test.app\n  webRunnerDir: /tmp/web\n  \
             url: http://localhost:3000\n\
             devices:\n  namePrefix: test\n\
             journeys:\n  driver: web\n  doneMarkers:\n    - done\n",
        root.clone(),
    )
    .unwrap();

    absorb_run(
        &root,
        &loaded.config,
        concat!(
            r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":["Home"],"#,
            r#""elements":[{"sel":"key:testid:sign-in","role":"button","#,
            r#""label":"Sign in","bounds":[10,20,100,32]}],"#,
            r#""texts":[{"text":"Sign in","bounds":[22,28,44,14]}]}"#
        ),
    )
    .unwrap();

    assert!(
        crate::runtime::project_layout::appmap_path(&root).exists(),
        "app map should be under .reproit/map/"
    );
    assert!(
        crate::runtime::project_layout::visits_path(&root).exists(),
        "visits should be under .reproit/map/"
    );
    assert!(
        !root.join(".reproit/appmap.json").exists(),
        "old root app map should not be written"
    );
    assert!(
        !root.join(".reproit/visits.json").exists(),
        "old root visits should not be written"
    );
    let map = load_map(&root, &loaded.config).unwrap();
    let state = map.states.values().next().unwrap();
    assert_eq!(state.elements.len(), 1);
    assert_eq!(state.elements[0].label, "Sign in");
    assert_eq!(state.elements[0].sel, "key:testid:sign-in");
    assert_eq!(state.elements[0].bounds, Some([10, 20, 100, 32]));
    assert_eq!(state.texts.len(), 1);
    assert_eq!(state.texts[0].text, "Sign in");
    assert_eq!(state.texts[0].bounds, Some([22, 28, 44, 14]));

    let visits = load_visits(&root, map.revision).unwrap();
    assert_eq!(visits.map_revision, map.revision);
    let good_map = std::fs::read(appmap_path(&root)).unwrap();
    std::fs::write(appmap_path(&root), b"{").unwrap();
    let error = load_map(&root, &loaded.config).unwrap_err().to_string();
    assert!(error.contains("refusing to replace a corrupt map"));
    assert_eq!(std::fs::read(appmap_path(&root)).unwrap(), b"{");
    std::fs::write(appmap_path(&root), good_map).unwrap();

    let mut mismatched = visits;
    mismatched.map_revision += 1;
    persistence::save_visits(&root, &mismatched).unwrap();
    let error = load_visits(&root, map.revision).unwrap_err().to_string();
    assert!(error.contains("refusing a partial snapshot"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn provenance_detects_real_inputs_and_ignores_build_output() {
    let root = std::env::temp_dir().join(format!(
        "reproit-map-provenance-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::create_dir_all(root.join(".reproit/map")).unwrap();
    std::fs::write(root.join("src/app.ts"), "export const screen = 'home';").unwrap();
    std::fs::write(root.join("reproit.yaml"), "app: {}\n").unwrap();
    let map = AppMap::empty("test-app".to_string());
    let mut visits = Visits::default();
    with_map_lock(&root, || save_snapshot(&root, &map, &mut visits)).unwrap();

    assert_eq!(map_freshness(&root).unwrap(), MapFreshness::Current);

    std::fs::write(root.join("target/generated.js"), "ignored").unwrap();
    assert_eq!(map_freshness(&root).unwrap(), MapFreshness::Current);

    std::fs::write(root.join("src/app.ts"), "export const screen = 'settings';").unwrap();
    assert_eq!(
        map_freshness(&root).unwrap(),
        MapFreshness::Stale(vec!["application source changed"])
    );

    with_map_lock(&root, || save_snapshot(&root, &map, &mut visits)).unwrap();
    std::fs::write(root.join("reproit.yaml"), "app: { platform: web }\n").unwrap();
    assert_eq!(
        map_freshness(&root).unwrap(),
        MapFreshness::Stale(vec!["reproit configuration changed"])
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn source_free_url_map_reuses_target_config_and_runner_identity() {
    let root = std::env::temp_dir().join(format!(
        "reproit-url-map-provenance-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join(".reproit/map")).unwrap();
    let config_path = root.join(".reproit/reproit.yaml");
    std::fs::write(
        &config_path,
        "app: { platform: web, url: https://one.test, webRunnerDir: /runner/v1 }\n",
    )
    .unwrap();
    let map = AppMap::empty("https://one.test".to_string());
    let mut visits = Visits::default();
    with_map_lock(&root, || save_snapshot(&root, &map, &mut visits)).unwrap();

    assert_eq!(map_freshness(&root).unwrap(), MapFreshness::Current);

    let provenance_path = persistence::provenance_path(&root);
    let mut provenance: MapProvenance =
        serde_json::from_slice(&std::fs::read(&provenance_path).unwrap()).unwrap();
    provenance.generated_at = (chrono::Utc::now() - chrono::Duration::minutes(16)).to_rfc3339();
    persistence::atomic_write_json(&provenance_path, &provenance).unwrap();
    assert_eq!(
        map_freshness(&root).unwrap(),
        MapFreshness::Stale(vec!["remote runtime revalidation due"])
    );

    with_map_lock(&root, || save_snapshot(&root, &map, &mut visits)).unwrap();

    std::fs::write(
        &config_path,
        "app: { platform: web, url: https://two.test, webRunnerDir: /runner/v1 }\n",
    )
    .unwrap();
    assert_eq!(
        map_freshness(&root).unwrap(),
        MapFreshness::Stale(vec!["reproit configuration changed"])
    );

    with_map_lock(&root, || save_snapshot(&root, &map, &mut visits)).unwrap();
    std::fs::write(
        &config_path,
        "app: { platform: web, url: https://two.test, webRunnerDir: /runner/v2 }\n",
    )
    .unwrap();
    assert_eq!(
        map_freshness(&root).unwrap(),
        MapFreshness::Stale(vec!["reproit configuration changed"])
    );

    with_map_lock(&root, || save_snapshot(&root, &map, &mut visits)).unwrap();
    std::fs::write(root.join("app.ts"), "export const screen = 'home';").unwrap();
    assert_eq!(
        map_freshness(&root).unwrap(),
        MapFreshness::Stale(vec!["application source changed"])
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn completed_scan_run_can_commit_the_map_without_another_drive() {
    let root = std::env::temp_dir().join(format!(
        "reproit-scan-map-commit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let run_dir = root.join(".reproit/runs/scan");
    std::fs::create_dir_all(&run_dir).unwrap();
    let loaded = crate::adapters::config::synthesize_web(
        "https://scan.test",
        Path::new("/runner/v1"),
        root.clone(),
    )
    .unwrap();
    std::fs::write(
        run_dir.join("drive-a.log"),
        concat!(
            "EXPLORE:STATE {\"sig\":\"home\",\"labels\":[\"Home\"]}\n",
            "JOURNEY DONE\n",
        ),
    )
    .unwrap();
    assert!(commit_run(&root, &loaded.config, &run_dir, false, true).unwrap());
    let map = load_map(&root, &loaded.config).unwrap();
    assert_eq!(map.states.len(), 1);
    assert_eq!(map_freshness(&root).unwrap(), MapFreshness::Current);

    std::fs::write(
        run_dir.join("drive-a.log"),
        "EXPLORE:STATE {\"sig\":\"partial\",\"labels\":[\"Partial\"]}\n",
    )
    .unwrap();
    assert!(!commit_run(&root, &loaded.config, &run_dir, true, false).unwrap());
    let preserved = load_map(&root, &loaded.config).unwrap();
    assert!(preserved
        .states
        .values()
        .any(|state| { state.signature.semantics_hash.as_deref() == Some("home") }));

    std::fs::write(run_dir.join("drive-a.log"), "EXPLORE:UNSCANNABLE {}\n").unwrap();
    assert!(!commit_run(&root, &loaded.config, &run_dir, true, true).unwrap());
    let preserved = load_map(&root, &loaded.config).unwrap();
    assert!(preserved
        .states
        .values()
        .any(|state| { state.signature.semantics_hash.as_deref() == Some("home") }));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn parses_only_supported_relationship_violations() {
    let obs = parse_run(concat!(
        "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[\"Liked You\"]}\n",
        "EXPLORE:RELATION {\"sig\":\"nav\",\"items\":[",
        "{\"kind\":\"indicator-anchor\",\"dependentKey\":\"key:id:dot\",",
        "\"ownerKey\":\"key:id:liked\",\"containerKey\":\"key:id:tabs\",",
        "\"violation\":\"detached\",\"maxGap\":8,\"gap\":123.45},",
        "{\"kind\":\"guessed-red-dot\",\"dependentKey\":\"x\",",
        "\"ownerKey\":\"y\",\"containerKey\":\"z\",\"violation\":\"detached\"}]}",
    ));
    let items = obs.relations.get("nav").expect("relationship violation");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "indicator-anchor");
    assert_eq!(items[0].dependent_key, "key:id:dot");
    assert_eq!(items[0].owner_key, "key:id:liked");
    assert_eq!(items[0].container_key, "key:id:tabs");
    assert_eq!(items[0].violation, "detached");
    assert_eq!(items[0].max_gap, 8);
    assert_eq!(items[0].gap_centipx, 12_345);
}

#[test]
fn parses_accessibility_state_checks_with_exact_subject_fingerprint() {
    let obs = parse_run(concat!(
        "EXPLORE:STATE {\"sig\":\"settings\",\"labels\":[\"Settings\"]}\n",
        "EXPLORE:A11YSTATESTATUS {\"sig\":\"settings\",\"outcome\":\"VIOLATION\",\"checks\":[",
        "{\"identity\":\"key:id:notifications\",\"property\":\"checked\",",
        "\"fingerprint\":\"sha256:f264f36f3b511e4ae5993d43\",\"expected\":\"true\",",
        "\"actual\":\"false\",\"outcome\":\"VIOLATION\",",
        "\"reason\":\"semantic-state-mismatch\"},",
        "{\"identity\":\"text:Notifications\",\"property\":\"checked\",",
        "\"fingerprint\":\"sha256:000000000000000000000000\",\"expected\":\"true\",",
        "\"actual\":\"false\",\"outcome\":\"VIOLATION\",",
        "\"reason\":\"semantic-state-mismatch\"}]}\n",
    ));
    let checks = obs
        .accessibility_state_checks
        .get("settings")
        .expect("accessibility checks");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].identity, "key:id:notifications");
    assert_eq!(checks[0].property, "checked");
    assert_eq!(checks[0].fingerprint, "sha256:f264f36f3b511e4ae5993d43");
    assert_eq!(checks[0].outcome, "VIOLATION");
}

#[test]
fn parses_overflow_through_the_shared_evaluator() {
    let obs = parse_run(concat!(
        "EXPLORE:STATE {\"sig\":\"card\",\"labels\":[\"Message\"]}\n",
        "EXPLORE:OVERFLOW {\"sig\":\"card\",\"version\":1,\"complete\":true,",
        "\"checks\":[{\"subjectKey\":\"key:id:message\",",
        "\"containerKey\":\"key:id:card\",\"authority\":\"exact-layout\",",
        "\"ownership\":\"app\",\"stableSamples\":2,\"transformed\":false,",
        "\"policy\":\"contain\",",
        "\"subjectRect\":{\"left\":4,\"top\":4,\"right\":108,\"bottom\":36},",
        "\"containerRect\":{\"left\":0,\"top\":0,\"right\":100,\"bottom\":40}}]}\n",
    ));
    let checks = obs.overflow_checks.get("card").expect("overflow checks");
    assert_eq!(checks.len(), 1);
    assert_eq!(
        checks[0].outcome,
        crate::domain::overflow::OverflowOutcome::Violation
    );
    assert_eq!(checks[0].spill_x_centipx, 800);
}
