use super::*;
use crate::model::repro;

#[test]
fn simplify_preserves_the_property_matched_fixture() {
    // A data-dependent repro carries inputs + locale. Simplifying it minimizes
    // the ACTIONS but must keep the data, or the adopted repro stops
    // reproducing the bug that only fires for that fixture.
    let src = serde_json::json!({
        "seed": 7u64,
        "replay": ["tap:a", "tap:b", "tap:c"],
        "inputs": [{ "field": "name", "value": "a-long-unicode-name" }],
        "locale": "tr",
    });
    let out = build_simplified_replay(7, &["tap:c".to_string()], &src);
    assert_eq!(out["replay"], serde_json::json!(["tap:c"]));
    assert_eq!(out["locale"], "tr");
    assert_eq!(out["inputs"], src["inputs"]);

    // A path-only repro (no fixture) stays the bare {seed, replay} shape.
    let bare = build_simplified_replay(
        7,
        &["tap:c".to_string()],
        &serde_json::json!({ "seed": 7u64, "replay": ["tap:a", "tap:c"] }),
    );
    assert!(bare.get("inputs").is_none());
    assert!(bare.get("locale").is_none());
    assert_eq!(bare["replay"], serde_json::json!(["tap:c"]));
}

#[test]
fn parse_fuzz_report_extracts_seed_and_repro_actions() {
    // The exact shape modes/fuzz.rs::write_report emits.
    let md = "\
# fuzz finding (seed 42)

## invariants violated

- **no-exception** (1)

## findings

- `no-exception` **EXCEPTION CAUGHT BY WIDGETS LIBRARY**: boom

## confirmed repro (2 actions, shrunk from 7)

```
tap:Login
tap:Submit
```

Replay: write {\"replay\": [...]} to .reproit/tmp/fuzz_config.json ...
";
    let (seed, actions) = parse_fuzz_report(md).expect("parse");
    assert_eq!(seed, 42);
    assert_eq!(actions, vec!["tap:Login", "tap:Submit"]);
    // The id is what `keep` would store under.
    assert_eq!(
        repro::repro_id(seed, &actions),
        repro::repro_id(42, &["tap:Login", "tap:Submit"])
    );
}

#[test]
fn pending_meta_lets_a_finding_be_checked_before_keep() {
    // A finding not yet kept: its in-memory Meta carries the same content-hash
    // id keep would store under, is quarantined, has no alias/created stamp,
    // and triggers at the end of its own minimized sequence.
    let f = Finding {
        id: "abcdef123456".into(),
        seed: 42,
        actions: vec!["tap:Login".into(), "tap:Submit".into()],
        run_dir: std::path::PathBuf::from("/tmp/nonexistent-run"),
    };
    let m = f.pending_meta();
    assert_eq!(m.id, "abcdef123456");
    assert_eq!(m.id, f.id());
    assert_eq!(m.status, repro::Status::Quarantined);
    assert_eq!(m.seed, 42);
    assert!(m.alias.is_none());
    assert!(m.created.is_empty());
    assert!(m.last_checked.is_none());
    assert_eq!(m.trigger_index, Some(2));
}

#[test]
fn public_and_internal_finding_ids_resolve_to_pending_artifact() {
    let root = std::env::temp_dir().join(format!("reproit-fnd-{}", std::process::id()));
    let run = root.join(".reproit/runs/run-1");
    std::fs::create_dir_all(&run).unwrap();
    let md = "\
# fuzz finding (seed 42)

## confirmed repro (2 actions)

```
tap:Login
tap:Submit
```
";
    std::fs::write(run.join("fuzz.md"), md).unwrap();
    let loaded = config::parse_str(
        "app:\n  platform: web\n  webRunnerDir: ./runners/web\n  url: http://localhost:3000\n\
             devices:\n  namePrefix: reproit\n\
             journeys:\n  dir: journeys\n  driver: explore\n  doneMarkers: [DONE]\n\
             evidence:\n  outDir: .reproit/runs\n  video: false\n",
        root.clone(),
    )
    .unwrap();
    let raw = repro::repro_id(42, &["tap:Login", "tap:Submit"]);
    assert!(find_finding_by_id(&loaded, &raw).is_some());
    assert!(find_finding_by_id(&loaded, &repro::display_finding_id(&raw)).is_some());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn finding_id_resolves_from_durable_store_after_evidence_moves() {
    let root = std::env::temp_dir().join(format!("reproit-durable-fnd-{}", std::process::id()));
    let raw = repro::repro_id(77, &["tap:key:save"]);
    let durable = root.join(".reproit/findings").join(&raw);
    std::fs::create_dir_all(&durable).unwrap();
    std::fs::write(
        durable.join("fuzz.md"),
        "# fuzz finding (seed 77)\n\n## confirmed repro (1 \
             actions)\n\n```\ntap:key:save\n```\n",
    )
    .unwrap();
    let loaded = config::parse_str(
        "app:\n  platform: web\n  webRunnerDir: ./runners/web\n  url: http://localhost:3000\n\
             devices:\n  namePrefix: reproit\n\
             journeys:\n  dir: journeys\n  driver: explore\n  doneMarkers: [DONE]\n\
             evidence:\n  outDir: moved/evidence\n  video: false\n",
        root.clone(),
    )
    .unwrap();
    let found = find_finding_by_id(&loaded, &repro::display_finding_id(&raw)).unwrap();
    assert_eq!(found.id(), raw);
    assert_eq!(found.run_dir, durable);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn parse_fuzz_oracle_reads_occlusion_block() {
    // The `## oracle` block carries the category and violating state sig.
    let md = "\
# fuzz finding (seed 9)

## invariants violated

- **no-occluded-control** (1)

## oracle

- oracle: `occlusion`
- invariant: `no-occluded-control`
- sig: `advanced`

## findings

- `no-occluded-control` **OCCLUSION**: state advanced has an occluded control

## confirmed repro (1 actions)

```
tap:Advanced
```
";
    let (oracle, sig, selector, fingerprint) = parse_fuzz_oracle(md);
    assert_eq!(oracle.as_deref(), Some("occlusion"));
    assert_eq!(sig.as_deref(), Some("advanced"));
    assert_eq!(selector, None);
    assert_eq!(fingerprint, None);
}

#[test]
fn parse_fuzz_oracle_crash_block_has_no_sig() {
    let md = "\
# fuzz finding (seed 1)

## oracle

- oracle: `crash`
- invariant: `no-exception`
- sig: ``

## findings
";
    let (oracle, sig, selector, fingerprint) = parse_fuzz_oracle(md);
    assert_eq!(oracle.as_deref(), Some("crash"));
    assert_eq!(sig, None);
    assert_eq!(selector, None);
    assert_eq!(fingerprint, None);
}

#[test]
fn parse_fuzz_oracle_preserves_exact_accessibility_fingerprint() {
    let md = "\
## oracle

- oracle: `accessibility-state`
- invariant: `no-accessibility-state-mismatch`
- sig: `settings`
- selector: `key:id:notifications`
- fingerprint: `sha256:f264f36f3b511e4ae5993d43`
";
    let (oracle, sig, selector, fingerprint) = parse_fuzz_oracle(md);
    assert_eq!(oracle.as_deref(), Some("accessibility-state"));
    assert_eq!(sig.as_deref(), Some("settings"));
    assert_eq!(selector.as_deref(), Some("key:id:notifications"));
    assert_eq!(
        fingerprint.as_deref(),
        Some("sha256:f264f36f3b511e4ae5993d43")
    );
}

#[test]
fn parse_fuzz_oracle_absent_block_is_none() {
    // An older report with no `## oracle` block -> fall back to crash path.
    let md = "# fuzz finding (seed 1)\n\n## findings\n";
    assert_eq!(parse_fuzz_oracle(md), (None, None, None, None));
}

#[test]
fn state_present_recording_navigates_directly_without_replay() {
    let log = r#"EXPLORE:STATE {"sig":"docs","route":"/docs/search","labels":[]}"#;
    let (url, action) = web_record_metadata(
        Some("https://example.test/start"),
        Some("zoom-reflow"),
        Some("docs"),
        log,
    );
    assert_eq!(url.as_deref(), Some("https://example.test/docs/search"));
    assert_eq!(action, None);
}

#[test]
fn flicker_recording_keeps_only_the_triggering_action() {
    let log = concat!(
        "EXPLORE:STATE {\"sig\":\"header\",\"route\":\"/pricing\",\"labels\":[]}\n",
        "EXPLORE:RERENDER \
             {\"from\":\"header\",\"action\":\"tap:key:menu\",\"churned\":[\"nav\"]}\n"
    );
    let (url, action) = web_record_metadata(
        Some("https://example.test/"),
        Some("flicker"),
        Some("header"),
        log,
    );
    assert_eq!(url.as_deref(), Some("https://example.test/pricing"));
    assert_eq!(action.as_deref(), Some("tap:key:menu"));
}

#[test]
fn legacy_recording_preserves_full_replay() {
    let meta: repro::Meta = serde_json::from_value(serde_json::json!({
        "id": "abc", "status": "quarantined", "seed": 1,
        "created": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let mut replay = serde_json::json!({"seed": 1, "replay": ["tap:A", "tap:B"]});
    minimize_record_replay(&mut replay, &meta);
    assert_eq!(replay["replay"], serde_json::json!(["tap:A", "tap:B"]));
    assert!(replay.get("gotoUrl").is_none());
}

#[test]
fn direct_recording_replaces_discovery_walk() {
    let mut meta: repro::Meta = serde_json::from_value(serde_json::json!({
        "id": "abc", "status": "quarantined", "seed": 1,
        "created": "2026-01-01T00:00:00Z",
        "record_url": "https://example.test/pricing",
        "record_action": "tap:key:menu"
    }))
    .unwrap();
    let mut replay = serde_json::json!({"seed": 1, "replay": ["tap:A", "tap:B"]});
    minimize_record_replay(&mut replay, &meta);
    assert_eq!(replay["replay"], serde_json::json!(["tap:key:menu"]));
    assert_eq!(replay["gotoUrl"], "https://example.test/pricing");
    meta.record_action = None;
    minimize_record_replay(&mut replay, &meta);
    assert_eq!(replay["replay"], serde_json::json!([]));
}

#[test]
fn parse_fuzz_report_handles_empty_repro_block() {
    let md = "# fuzz finding (seed 5)\n\n## confirmed repro (0 actions)\n\n```\n```\n";
    let (seed, actions) = parse_fuzz_report(md).expect("parse");
    assert_eq!(seed, 5);
    assert!(actions.is_empty());
}

#[test]
fn parse_fuzz_finding_id_accepts_scoped_marker_and_rejects_invalid_ids() {
    assert_eq!(
        parse_fuzz_finding_id("# fuzz finding (seed 0)\n\n<!-- finding-id: abcdef123456 -->"),
        Some("abcdef123456".to_string())
    );
    assert_eq!(
        parse_fuzz_finding_id("<!-- finding-id: not-an-id -->"),
        None
    );
    assert_eq!(parse_fuzz_finding_id("# legacy fuzz report"), None);
}

#[test]
fn parse_fuzz_report_without_seed_is_none() {
    assert!(parse_fuzz_report("# not a finding\n\nblah\n").is_none());
}

#[test]
fn web_engine_targets_route_to_the_cross_engine_path() {
    // A list of only engine names routes to the cross-engine differential.
    assert!(is_web_engines("chromium,firefox,webkit"));
    assert!(is_web_engines("chrome,safari"));
    // A bare `web` (or any platform token) is NOT the engine path: it is a
    // platform run. ios/android likewise route to the platform path.
    assert!(!is_web_engines("web"));
    assert!(!is_web_engines("ios,android"));
    // Mixed engine+platform is NOT all-engine -> platform path.
    assert!(!is_web_engines("chromium,ios"));
    assert!(!is_web_engines(""));
}

#[test]
fn only_flutter_sim_runs_offer_the_device_picker() {
    // Only FlutterDrive provisions a sim reproit picks, and only with --sim
    // (its default is the headless flutter test tier).
    assert!(run_needs_device_pick("flutter", true));
    assert!(!run_needs_device_pick("flutter", false));
    // Every other backend brings its own target (Appium caps, a browser, the
    // host, a PTY), so no reproit picker, even with --sim.
    for p in [
        "web",
        "react-native",
        "swift-ios",
        "android",
        "winui",
        "electron",
        "tauri",
    ] {
        assert!(!run_needs_device_pick(p, false), "{p} should not prompt");
        assert!(
            !run_needs_device_pick(p, true),
            "{p} should not prompt even with --sim"
        );
    }
    // Unknown platform: no prompt.
    assert!(!run_needs_device_pick("cobol-tui", false));
}

#[test]
fn account_login_selects_one_project_and_resolves_names() {
    let projects = vec![
        triage::CloudProject {
            name: "Store".into(),
            app_id: "store-1".into(),
        },
        triage::CloudProject {
            name: "Docs".into(),
            app_id: "docs-2".into(),
        },
    ];
    assert_eq!(
        choose_cloud_project(&projects[..1], None, false)
            .unwrap()
            .as_deref(),
        Some("store-1")
    );
    assert_eq!(
        choose_cloud_project(&projects, Some("Docs"), false)
            .unwrap()
            .as_deref(),
        Some("docs-2")
    );
    assert!(choose_cloud_project(&projects, Some("missing"), false).is_err());
}

#[test]
fn scoped_env_restores_prior_value_and_removes_unset_keys() {
    // ScopedEnv is what guarantees a per-target REPROIT_* never leaks into
    // the next target (Task 1) AND the same Drop pattern underpins the
    // crash-reporter restore (Task 2). Use unique keys to avoid clobbering
    // anything real in the test process.
    let set_key = "REPROIT_TEST_SCOPED_SET";
    let unset_key = "REPROIT_TEST_SCOPED_UNSET";
    std::env::set_var(set_key, "original");
    std::env::remove_var(unset_key);
    {
        let _guard = ScopedEnv::set(vec![
            (set_key.to_string(), "during".to_string()),
            (unset_key.to_string(), "during".to_string()),
        ]);
        assert_eq!(std::env::var(set_key).as_deref(), Ok("during"));
        assert_eq!(std::env::var(unset_key).as_deref(), Ok("during"));
    }
    // After drop: the previously-set key is restored to its old value, and
    // the previously-unset key is removed entirely.
    assert_eq!(std::env::var(set_key).as_deref(), Ok("original"));
    assert!(std::env::var(unset_key).is_err());
    std::env::remove_var(set_key);
}
