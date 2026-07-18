//! Cross-cutting contracts shared by CLI commands and execution modes.
//!
//! This facade keeps the historical crate::crosscut API stable while the
//! implementations live in responsibility-focused modules.

mod cloud_profile;
mod device;
mod locale;
mod oracle;
mod target;

pub use cloud_profile::{load_cloud_app, load_token, save_cloud_profile, save_token, token_path};
pub use device::{parse_adb_devices, parse_flutter_devices, parse_simctl_devices, Device};
pub use locale::{locale_specific_findings, parse_locales, tag_finding_locale, LOCALE_ENV};
pub use oracle::{classify, Oracle, OracleFilter};
// Compatibility façade: retained even when only downstream callers use it.
#[allow(unused_imports)]
pub use target::{
    canonical_engine, cross_target_divergence, is_web_engine_token, parse_run_targets,
    platform_targets, RunTarget, Target,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;

    #[test]
    fn parse_locales_dedupes_trims_and_drops_blanks() {
        assert_eq!(parse_locales("de, ar ,ja"), vec!["de", "ar", "ja"]);
        assert_eq!(parse_locales("de,de,ar"), vec!["de", "ar"]);
        assert!(parse_locales("").is_empty());
        assert!(parse_locales("  , ,").is_empty());
    }

    // CROSS-REPO DRIFT GUARD. `crates/reproit/oracle-registry.json` is the machine-
    // readable contract of every oracle category the CLI can stamp onto a finding
    // (the `oracle` field, set by OracleFilter::apply). reproit-cloud pins this
    // file to know which ids it must handle. This test keeps the JSON in lockstep
    // with `Oracle::ALL` (the code source of truth): if someone adds or removes an
    // oracle without updating the JSON, CLI CI fails HERE, so the contract can
    // never silently drift from the code.
    //
    // The enforcement that the CLOUD keeps up is INTENTIONALLY not here (the CLI is
    // never blocked by the cloud lagging). It lives cloud-side as a P0 CI test that
    // consumes this same file, e.g.:
    //   const cli =
    // JSON.parse(fs.readFileSync("<vendored>/oracle-registry.json")).oracles;
    //   const handled = Object.keys(ORACLE_DISPLAY);           // cloud's own
    // registry   const missing = cli.filter(id => !handled.includes(id));
    //   assert(missing.length === 0, `P0: cloud does not handle oracle(s):
    // ${missing}`); plus a RUNTIME rule that an unrecognized `oracle` id
    // renders generically and is never dropped -- so a newer CLI in a
    // customer's CI never breaks ingestion, it only trips the cloud's own drift
    // alarm to add first-class handling.
    #[test]
    fn oracle_registry_matches_all() {
        const REGISTRY: &str = include_str!("../../oracle-registry.json");
        let doc: Value =
            serde_json::from_str(REGISTRY).expect("oracle-registry.json must be valid JSON");
        let listed: BTreeSet<String> = doc["oracles"]
            .as_array()
            .expect("oracle-registry.json must have an `oracles` array")
            .iter()
            .map(|v| v.as_str().expect("each oracle id is a string").to_string())
            .collect();
        let actual: BTreeSet<String> = Oracle::ALL.iter().map(|o| o.as_str().to_string()).collect();
        assert_eq!(
            listed, actual,
            "oracle-registry.json is out of sync with Oracle::ALL. Update the JSON to match, then \
             add cloud-side handling for any new id (a P0 on reproit-cloud)."
        );
    }

    #[test]
    fn stable_registry_and_code_only_contain_authoritative_exact_replay_oracles() {
        const REGISTRY: &str = include_str!("../../oracle-registry.json");
        let doc: Value = serde_json::from_str(REGISTRY).unwrap();
        let registered: BTreeSet<&str> = doc["stable_defaults"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        let code = OracleFilter::stable_set();
        assert_eq!(
            registered, code,
            "stable defaults drifted from the registry"
        );

        // This is deliberately an allowlist, not an optimistic property on the
        // enum. Adding a default therefore requires a code review that adds its
        // authoritative predicate and exact replay branch first.
        let authoritative: BTreeSet<&str> = doc["authoritative_exact_replay"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert_eq!(code, authoritative);
    }

    #[test]
    fn confidence_audit_classifies_every_oracle_exactly_once() {
        const REGISTRY: &str = include_str!("../../oracle-registry.json");
        let doc: Value = serde_json::from_str(REGISTRY).unwrap();
        let confidence = doc["confidence"].as_object().unwrap();
        let expected_tiers: BTreeSet<&str> = [
            "confirmed_proof",
            "contract_dependent",
            "environment_dependent_or_advisory",
            "heuristic_or_specialist",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            confidence
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            expected_tiers
        );

        let mut audited = BTreeSet::new();
        for values in confidence.values() {
            for value in values.as_array().unwrap() {
                let id = value.as_str().unwrap();
                assert!(audited.insert(id), "oracle {id} appears in multiple tiers");
            }
        }
        let all = Oracle::ALL.iter().map(|oracle| oracle.as_str()).collect();
        assert_eq!(audited, all, "every oracle must have one confidence tier");
    }

    #[test]
    fn tag_finding_locale_sets_the_field() {
        let mut f = json!({ "kind": "PERF" });
        tag_finding_locale(&mut f, "ar");
        assert_eq!(f["locale"], "ar");
    }

    #[test]
    fn locale_specific_findings_flags_partial_presence() {
        let de: BTreeSet<String> = ["overflow", "shared"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ar: BTreeSet<String> = ["shared"].iter().map(|s| s.to_string()).collect();
        let out = locale_specific_findings(&[("de".into(), de), ("ar".into(), ar)]);
        // "overflow" appears in de only -> locale-specific; "shared" is in both.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "overflow");
        assert_eq!(out[0].1, vec!["de".to_string()]);
    }

    #[test]
    fn locale_specific_findings_needs_two_locales() {
        let de: BTreeSet<String> = ["a"].iter().map(|s| s.to_string()).collect();
        assert!(locale_specific_findings(&[("de".into(), de)]).is_empty());
    }

    #[test]
    fn classify_maps_invariants_and_kinds_to_oracles() {
        assert_eq!(
            classify(&json!({ "invariant": "no-exception" })),
            Oracle::Crash
        );
        assert_eq!(classify(&json!({ "invariant": "no-jank" })), Oracle::Jank);
        assert_eq!(classify(&json!({ "invariant": "no-leak" })), Oracle::Leak);
        assert_eq!(
            classify(&json!({ "oracle": "backend-contract", "kind": "response-shape" })),
            Oracle::Contract
        );
        // Listener leak folds into the Leak oracle (invariant id AND kind).
        assert_eq!(
            classify(&json!({ "invariant": "no-listener-leak" })),
            Oracle::Leak
        );
        assert_eq!(classify(&json!({ "kind": "LISTENERLEAK" })), Oracle::Leak);
        assert_eq!(
            classify(&json!({ "invariant": "no-occluded-control" })),
            Oracle::Occlusion
        );
        assert_eq!(
            classify(&json!({ "invariant": "no-broken-render" })),
            Oracle::ContentBug
        );
        assert_eq!(
            classify(&json!({ "kind": "CONTENTBUG" })),
            Oracle::ContentBug
        );
        assert_eq!(classify(&json!({ "invariant": "no-hang" })), Oracle::Hang);
        assert_eq!(classify(&json!({ "kind": "HANG" })), Oracle::Hang);
        // The web jank path reuses the no-jank invariant -> jank category.
        assert_eq!(classify(&json!({ "invariant": "no-jank" })), Oracle::Jank);
        // The new categories parse from their --only/--no names + aliases.
        assert_eq!(Oracle::parse("content-bug"), Some(Oracle::ContentBug));
        assert_eq!(Oracle::parse("content"), Some(Oracle::ContentBug));
        assert_eq!(Oracle::parse("hang"), Some(Oracle::Hang));
        assert_eq!(Oracle::parse("freeze"), Some(Oracle::Hang));
        assert_eq!(classify(&json!({ "kind": "PERF" })), Oracle::Jank);
        // Choice-anomaly must NOT fall through to crash (it has its own category).
        assert_eq!(
            classify(&json!({ "invariant": "no-choice-anomaly" })),
            Oracle::ChoiceAnomaly
        );
        assert_eq!(Oracle::parse("choice-anomaly"), Some(Oracle::ChoiceAnomaly));
        // Broken-route is its own category, parsed from its names/aliases.
        assert_eq!(
            classify(&json!({ "invariant": "no-broken-route" })),
            Oracle::BrokenRoute
        );
        assert_eq!(Oracle::parse("broken-route"), Some(Oracle::BrokenRoute));
        assert_eq!(Oracle::parse("404"), Some(Oracle::BrokenRoute));
        // Duplicate-submit maps from its invariant AND its kind, and parses from
        // its --only/--no names + aliases.
        assert_eq!(
            classify(&json!({ "invariant": "no-duplicate-submit" })),
            Oracle::DuplicateSubmit
        );
        assert_eq!(
            classify(&json!({ "kind": "DUPSUBMIT" })),
            Oracle::DuplicateSubmit
        );
        assert_eq!(
            Oracle::parse("duplicate-submit"),
            Some(Oracle::DuplicateSubmit)
        );
        assert_eq!(Oracle::parse("dupsubmit"), Some(Oracle::DuplicateSubmit));
        assert_eq!(
            Oracle::parse("double-submit"),
            Some(Oracle::DuplicateSubmit)
        );
        // Focus-loss maps from its invariant AND its kind, and parses from its
        // --only/--no names + aliases.
        assert_eq!(
            classify(&json!({ "invariant": "no-focus-loss" })),
            Oracle::FocusLoss
        );
        assert_eq!(classify(&json!({ "kind": "FOCUSLOSS" })), Oracle::FocusLoss);
        assert_eq!(Oracle::parse("focus-loss"), Some(Oracle::FocusLoss));
        assert_eq!(Oracle::parse("focusloss"), Some(Oracle::FocusLoss));
        // Zoom-reflow maps from its invariant AND its kind, and parses from its
        // --only/--no names + aliases.
        assert_eq!(
            classify(&json!({ "invariant": "no-reflow-break" })),
            Oracle::ZoomReflow
        );
        assert_eq!(
            classify(&json!({ "kind": "ZOOMREFLOW" })),
            Oracle::ZoomReflow
        );
        assert_eq!(Oracle::parse("zoom-reflow"), Some(Oracle::ZoomReflow));
        assert_eq!(Oracle::parse("reflow"), Some(Oracle::ZoomReflow));
        assert_eq!(Oracle::parse("zoom"), Some(Oracle::ZoomReflow));
        // Rotation + background-restore (the lifecycle-metamorphic oracles) map
        // from their invariant AND kind, and parse from their names + aliases.
        assert_eq!(
            classify(&json!({ "invariant": "no-rotation-loss" })),
            Oracle::Rotation
        );
        assert_eq!(classify(&json!({ "kind": "ROTATION" })), Oracle::Rotation);
        assert_eq!(Oracle::parse("rotation"), Some(Oracle::Rotation));
        assert_eq!(Oracle::parse("orientation"), Some(Oracle::Rotation));
        assert_eq!(
            classify(&json!({ "invariant": "no-background-loss" })),
            Oracle::BackgroundRestore
        );
        assert_eq!(
            classify(&json!({ "kind": "BGRESTORE" })),
            Oracle::BackgroundRestore
        );
        assert_eq!(
            Oracle::parse("background-restore"),
            Some(Oracle::BackgroundRestore)
        );
        assert_eq!(Oracle::parse("lifecycle"), Some(Oracle::BackgroundRestore));
        // Scroll-round-trip maps from its invariant AND its kind, and parses
        // from its --only/--no names + aliases.
        assert_eq!(
            classify(&json!({ "invariant": "no-scroll-recycle" })),
            Oracle::ScrollRoundTrip
        );
        assert_eq!(
            classify(&json!({ "kind": "SCROLLROUNDTRIP" })),
            Oracle::ScrollRoundTrip
        );
        assert_eq!(
            Oracle::parse("scroll-round-trip"),
            Some(Oracle::ScrollRoundTrip)
        );
        assert_eq!(Oracle::parse("list-recycle"), Some(Oracle::ScrollRoundTrip));
        // Wakelock maps from its invariant AND its kind, and parses from its
        // --only/--no names + aliases (Android battery-drain oracle).
        assert_eq!(
            classify(&json!({ "invariant": "no-wakelock-leak" })),
            Oracle::WakeLock
        );
        assert_eq!(classify(&json!({ "kind": "WAKELOCK" })), Oracle::WakeLock);
        assert_eq!(Oracle::parse("wakelock"), Some(Oracle::WakeLock));
        assert_eq!(Oracle::parse("keep-screen-on"), Some(Oracle::WakeLock));
        // Safe-area maps from its invariant AND its kind, and parses from its
        // --only/--no names + aliases (never falling through to crash).
        assert_eq!(
            classify(&json!({ "invariant": "no-safe-area-collision" })),
            Oracle::SafeArea
        );
        assert_eq!(classify(&json!({ "kind": "SAFEAREA" })), Oracle::SafeArea);
        assert_eq!(Oracle::parse("safe-area"), Some(Oracle::SafeArea));
        assert_eq!(Oracle::parse("notch"), Some(Oracle::SafeArea));
        // Permission-walk maps from its invariant AND its kind, and parses from
        // its --only/--no names + aliases.
        assert_eq!(
            classify(&json!({ "invariant": "no-permission-dead-end" })),
            Oracle::PermissionWalk
        );
        assert_eq!(
            classify(&json!({ "kind": "PERMISSIONWALK" })),
            Oracle::PermissionWalk
        );
        assert_eq!(
            Oracle::parse("permission-walk"),
            Some(Oracle::PermissionWalk)
        );
        assert_eq!(Oracle::parse("permission"), Some(Oracle::PermissionWalk));
        // Raw exception block: falls back to crash.
        assert_eq!(
            classify(&json!({ "kind": "EXCEPTION CAUGHT BY WIDGETS LIBRARY" })),
            Oracle::Crash
        );
    }

    #[test]
    fn oracle_filter_default_allows_everything() {
        let f = OracleFilter::all();
        for &o in Oracle::ALL {
            assert_eq!(f.allows(o), o != Oracle::Unclassified);
        }
    }

    #[test]
    fn unknown_finding_never_falls_through_to_crash_or_confirmation() {
        let finding = json!({"kind": "FUTURE_OR_MISSPELLED_ORACLE"});
        assert_eq!(classify(&finding), Oracle::Unclassified);
        let (kept, dropped) = OracleFilter::all().apply(vec![finding]);
        assert!(kept.is_empty());
        assert_eq!(dropped.len(), 1);
    }

    #[test]
    fn explicit_specialist_oracle_stays_candidate_and_cannot_become_a_repro() {
        let finding = json!({"kind": "CHOICEANOMALY", "invariant": "no-choice-anomaly"});
        let (kept, dropped) = OracleFilter::all().apply(vec![finding]);
        assert!(dropped.is_empty());
        assert_eq!(kept[0]["oracle"], "choice-anomaly");
        assert_eq!(kept[0]["advisory"], true);
        assert_eq!(kept[0]["confidence"], "candidate");
    }

    #[test]
    fn oracle_filter_build_defaults_to_stable_confirmable_detectors() {
        let (f, unknown) = OracleFilter::build(None, None);
        assert!(unknown.is_empty());
        for oracle in [Oracle::Crash, Oracle::DetachedIndicator, Oracle::Contract] {
            assert!(f.allows(oracle), "stable default missing {oracle:?}");
        }
        for oracle in [
            Oracle::FocusLoss,
            Oracle::Security,
            Oracle::Jank,
            Oracle::Leak,
            Oracle::ContentBug,
            Oracle::Hang,
            Oracle::ChoiceAnomaly,
            Oracle::BrokenRoute,
            Oracle::BlankScreen,
            Oracle::BrokenAsset,
            Oracle::Unclassified,
        ] {
            assert!(
                !f.allows(oracle),
                "specialist oracle leaked into defaults: {oracle:?}"
            );
        }
    }

    #[test]
    fn oracle_filter_only_restricts() {
        let (f, unknown) = OracleFilter::build(Some("crash,jank"), None);
        assert!(unknown.is_empty());
        assert!(f.allows(Oracle::Crash));
        assert!(f.allows(Oracle::Jank));
        assert!(!f.allows(Oracle::Leak));
        assert!(!f.allows(Oracle::Visual));
    }

    #[test]
    fn oracle_filter_no_excludes() {
        let (f, _) = OracleFilter::build(None, Some("jank,leak"));
        assert!(f.allows(Oracle::Crash));
        assert!(!f.allows(Oracle::Jank));
        assert!(!f.allows(Oracle::Leak));
    }

    #[test]
    fn oracle_filter_only_then_no_subtracts() {
        let (f, _) = OracleFilter::build(Some("crash,jank"), Some("jank"));
        assert!(f.allows(Oracle::Crash));
        assert!(!f.allows(Oracle::Jank));
    }

    #[test]
    fn oracle_filter_reports_unknown_categories() {
        let (_f, unknown) = OracleFilter::build(Some("crash,bogus"), None);
        assert_eq!(unknown, vec!["bogus".to_string()]);
    }

    #[test]
    fn oracle_filter_apply_tags_kept_and_splits_dropped() {
        let (f, _) = OracleFilter::build(Some("crash"), None);
        let findings = vec![
            json!({ "invariant": "no-exception", "message": "boom" }),
            json!({ "invariant": "no-jank", "message": "janky" }),
        ];
        let (kept, dropped) = f.apply(findings);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["oracle"], "crash");
        assert_eq!(dropped.len(), 1);
        // Dropped findings are NOT tagged.
        assert!(dropped[0].get("oracle").is_none());
    }

    #[test]
    fn parse_run_targets_routes_engines_vs_platforms() {
        // All-engine list -> the cross-engine differential, canonicalized.
        let (rts, unknown) = parse_run_targets("chromium,firefox,webkit");
        assert!(unknown.is_empty());
        assert_eq!(
            rts,
            vec![
                RunTarget::Engine("chromium".into()),
                RunTarget::Engine("firefox".into()),
                RunTarget::Engine("webkit".into()),
            ]
        );
        // Engine aliases canonicalize (chrome->chromium, safari->webkit).
        let (rts2, _) = parse_run_targets("chrome,safari");
        assert_eq!(
            rts2,
            vec![
                RunTarget::Engine("chromium".into()),
                RunTarget::Engine("webkit".into()),
            ]
        );
    }

    #[test]
    fn parse_run_targets_treats_bare_web_as_platform() {
        // A bare `web` is the WEB PLATFORM (one platform run), not the
        // cross-engine differential. Mixed platform list expands `all`.
        let (rts, _) = parse_run_targets("web");
        assert_eq!(rts, vec![RunTarget::Platform(Target::Web)]);
        let (rts2, _) = parse_run_targets("ios,all,web");
        assert_eq!(
            rts2,
            vec![
                RunTarget::Platform(Target::Ios),
                RunTarget::Platform(Target::Android),
                RunTarget::Platform(Target::Web),
            ]
        );
    }

    #[test]
    fn parse_run_targets_reports_unknown_tokens() {
        // A platform list with a bogus token surfaces it; the rest still parse.
        let (rts, unknown) = parse_run_targets("ios,bogus");
        assert_eq!(rts, vec![RunTarget::Platform(Target::Ios)]);
        assert_eq!(unknown, vec!["bogus".to_string()]);
    }

    #[test]
    fn cross_target_divergence_flags_subset_findings() {
        // crash:X reproduces on chromium+firefox but NOT webkit -> divergence.
        // crash:Y reproduces on all three -> consistent, NOT divergence.
        let chromium: BTreeSet<String> = ["crash:X", "crash:Y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let firefox: BTreeSet<String> = ["crash:X", "crash:Y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let webkit: BTreeSet<String> = ["crash:Y"].iter().map(|s| s.to_string()).collect();
        let div = cross_target_divergence(&[
            ("chromium".into(), chromium),
            ("firefox".into(), firefox),
            ("webkit".into(), webkit),
        ]);
        assert_eq!(div.len(), 1);
        assert_eq!(div[0].0, "crash:X");
        assert_eq!(
            div[0].1,
            vec!["chromium".to_string(), "firefox".to_string()]
        );
    }

    #[test]
    fn cross_target_divergence_same_on_all_is_not_divergence() {
        let a: BTreeSet<String> = ["crash:X"].iter().map(|s| s.to_string()).collect();
        let b: BTreeSet<String> = ["crash:X"].iter().map(|s| s.to_string()).collect();
        assert!(cross_target_divergence(&[("ios".into(), a), ("android".into(), b)]).is_empty());
    }

    #[test]
    fn cross_target_divergence_needs_two_targets() {
        let a: BTreeSet<String> = ["crash:X"].iter().map(|s| s.to_string()).collect();
        assert!(cross_target_divergence(&[("ios".into(), a)]).is_empty());
    }

    #[test]
    fn parse_flutter_devices_reads_machine_json() {
        let json = r#"[
            {"name":"iPhone 16","id":"ABC-123","targetPlatform":"ios"},
            {"name":"Pixel 7","id":"emulator-5554","targetPlatform":"android-arm64"},
            {"name":"Chrome","id":"chrome","targetPlatform":"web-javascript"},
            {"name":"macOS","id":"macos","targetPlatform":"darwin"}
        ]"#;
        let devs = parse_flutter_devices(json);
        // macOS (darwin) is not one of our three targets -> dropped.
        assert_eq!(devs.len(), 3);
        assert_eq!(devs[0].target, Target::Ios);
        assert_eq!(devs[1].target, Target::Android);
        assert_eq!(devs[1].id, "emulator-5554");
        assert_eq!(devs[2].target, Target::Web);
    }

    #[test]
    fn parse_simctl_devices_picks_booted_and_skips_unavailable() {
        let text = "\
== Devices ==
-- iOS 17.0 --
    iPhone 16 (11111111-2222-3333-4444-555555555555) (Booted)
    iPhone SE (66666666-7777-8888-9999-000000000000) (Shutdown)
    Old Phone (AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE) (Shutdown) (unavailable, runtime profile not \
                    found)
";
        let devs = parse_simctl_devices(text);
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[0].name, "iPhone 16");
        assert!(devs[0].booted);
        assert_eq!(devs[1].name, "iPhone SE");
        assert!(!devs[1].booted);
        assert!(devs.iter().all(|d| d.target == Target::Ios));
    }

    #[test]
    fn parse_simctl_devices_skips_tv_watch_vision_runtimes() {
        let text = "\
== Devices ==
-- iOS 18.0 --
    iPhone 16 Pro (11111111-2222-3333-4444-555555555555) (Booted)
    iPad Air 11-inch (22222222-3333-4444-5555-666666666666) (Shutdown)
-- tvOS 18.0 --
    Apple TV 4K (33333333-4444-5555-6666-777777777777) (Shutdown)
-- watchOS 11.0 --
    Apple Watch Series 10 (44444444-5555-6666-7777-888888888888) (Shutdown)
-- visionOS 2.0 --
    Apple Vision Pro (55555555-6666-7777-8888-999999999999) (Shutdown)
";
        let devs = parse_simctl_devices(text);
        // Only the iOS-runtime iPhone + iPad survive; TV / Watch / Vision are out.
        assert_eq!(devs.len(), 2);
        assert!(devs.iter().any(|d| d.name == "iPhone 16 Pro"));
        assert!(devs.iter().any(|d| d.name == "iPad Air 11-inch"));
        assert!(devs.iter().all(|d| !d.name.contains("Apple")));
    }

    #[test]
    fn platform_targets_cover_every_framework() {
        assert_eq!(platform_targets("flutter"), vec![Target::Ios]);
        assert_eq!(platform_targets("web"), vec![Target::Web]);
        assert_eq!(platform_targets("swift-ios"), vec![Target::Ios]);
        assert_eq!(platform_targets("android"), vec![Target::Android]);
        assert_eq!(
            platform_targets("react-native"),
            vec![Target::Ios, Target::Android]
        );
        // Desktop / host frameworks have no device target (they run on the host).
        for p in ["winui", "electron", "tauri", "swift-macos"] {
            assert!(platform_targets(p).is_empty(), "{p} has no device target");
        }
    }

    #[test]
    fn parse_adb_devices_keeps_only_ready_devices() {
        let text = "\
List of devices attached
emulator-5554\tdevice
ZX1G\toffline
RF8N\tunauthorized
ZY2H\tdevice
";
        let devs = parse_adb_devices(text);
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[0].id, "emulator-5554");
        assert_eq!(devs[1].id, "ZY2H");
        assert!(devs.iter().all(|d| d.target == Target::Android));
    }

    #[test]
    fn token_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("reproit-tok-{}", std::process::id()));
        let path = dir.join("token");
        save_token(&path, "sk-test-123", "http://cloud.example").unwrap();
        let (tok, url) = load_token(&path).expect("loads");
        assert_eq!(tok, "sk-test-123");
        assert_eq!(url.as_deref(), Some("http://cloud.example"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_token_missing_file_is_none() {
        let path = std::env::temp_dir().join("reproit-nonexistent-token-xyz");
        let _ = std::fs::remove_file(&path);
        assert!(load_token(&path).is_none());
    }

    #[test]
    fn cloud_profile_roundtrips_selected_app() {
        let dir = std::env::temp_dir().join(format!("reproit-profile-{}", std::process::id()));
        let path = dir.join("token");
        save_cloud_profile(&path, "sk-test", "https://cloud.example", Some("checkout")).unwrap();
        assert_eq!(load_cloud_app(&path).as_deref(), Some("checkout"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
