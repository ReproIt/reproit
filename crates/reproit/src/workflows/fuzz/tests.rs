use super::*;

fn plan(seed: u64) -> SeedPlan {
    SeedPlan {
        seed,
        config: json!({ "seed": seed }),
    }
}

#[test]
fn equivalent_seed_findings_reserve_only_one_shrink() {
    let a = vec![json!({
        "invariant": "no-exception", "kind": "EXCEPTION", "message": "boom",
        "frames": ["render (app.js:10)"]
    })];
    let b = vec![json!({
        "invariant": "no-exception", "kind": "EXCEPTION", "message": "boom",
        "frames": ["render (app.js:10)"]
    })];
    let distinct = vec![json!({
        "invariant": "no-choice-anomaly", "kind": "CHOICEANOMALY", "message": "Go shifts layout"
    })];
    let mut seen = std::collections::BTreeSet::new();
    assert!(reserve_shrink_representative(&mut seen, &a));
    assert!(!reserve_shrink_representative(&mut seen, &b));
    assert!(reserve_shrink_representative(&mut seen, &distinct));
}

#[test]
fn incomplete_batch_never_masquerades_as_complete() {
    let plans = vec![plan(1), plan(2), plan(3)];
    let timed_out = "SEED:BEGIN 1\nFUZZ:ACT tap:A\nSEED:END 1\nSEED:BEGIN 2\nFUZZ:ACT tap:B\n";
    assert!(!batch_completed(timed_out, &plans));

    let partial_with_footer = "SEED:BEGIN 1\nSEED:END 1\nJOURNEY DONE\n";
    assert!(!batch_completed(partial_with_footer, &plans));

    let complete = "SEED:BEGIN 1\nSEED:END 1\nSEED:BEGIN 2\nSEED:END 2\nSEED:BEGIN \
                        3\nSEED:END 3\nJOURNEY DONE\n";
    assert!(batch_completed(complete, &plans));
}

// The shrink reproduction oracle: a shorter candidate counts as
// reproducing only when the exact original finding identity fires.

#[test]
fn shrink_oracle_requires_the_exact_original_finding() {
    let original = json!({
        "kind": "EXCEPTION CAUGHT BY WIDGETS LIBRARY",
        "invariant": "no-exception",
        "message": "boom",
    });
    let want = shrink_target(std::slice::from_ref(&original));
    assert!(want.contains(&finding_signature(&original)));

    // A crash-free shorter candidate that only trips another invariant must
    // NOT count as reproducing the crash.
    let crash_free = vec![json!({
        "invariant": "no-broken-render",
        "kind": "CONTENTBUG",
        "message": "broken binding",
    })];
    assert!(
        !reproduces_original(&crash_free, &want),
        "a trace that only trips another invariant must NOT reproduce a crash finding"
    );

    // The exact original failure does reproduce.
    let still_crashes = vec![original];
    assert!(reproduces_original(&still_crashes, &want));

    // No findings at all: never reproduces.
    assert!(!reproduces_original(&[], &want));
}

#[test]
fn primary_finding_is_stable_among_equal_severity_reals() {
    // Two real bugs: keep the first (preserve the old order).
    let findings = vec![
        json!({ "invariant": "no-choice-anomaly", "kind": "CHOICEANOMALY" }),
        json!({ "invariant": "no-exception", "kind": "EXCEPTION", "message": "boom" }),
    ];
    assert_eq!(
        finding_category(primary_finding(&findings).unwrap()),
        "no-choice-anomaly"
    );
}

#[test]
fn crash_trigger_index_counts_actions_up_to_the_exception() {
    let log = "\
JOURNEY claimed role=a
FUZZ:ACT tap:add
FUZZ:ACT tap:open-cart
FUZZ:ACT tap:remove-last
EXCEPTION CAUGHT BY WEB PAGE
The following error was thrown:
TypeError: ...
FUZZ:ACT back
";
    // The crash fired on the 3rd action; trailing actions don't move it.
    assert_eq!(crash_trigger_index(log), Some(3));
    // No exception -> no crash trigger (graph findings aren't truncated).
    assert_eq!(
        crash_trigger_index("FUZZ:ACT tap:a\nFUZZ:ACT tap:b\n"),
        None
    );
}

#[test]
fn finding_signature_buckets_by_crash_location() {
    // Same message + same top frame (crash location) = same bug bucket,
    // even though the surrounding stack differs.
    let a = json!({
        "kind": "EXCEPTION",
        "message": "Cannot read 'id'",
        "frames": ["updateSummary (app:537)"]
    });
    let b = json!({
        "kind": "EXCEPTION",
        "message": "Cannot read 'id'",
        "frames": ["updateSummary (app:537)", "changeQty (app:469)"]
    });
    assert_eq!(finding_signature(&a), finding_signature(&b));
    // A different crash LOCATION is a different bug, even with the same message.
    let c = json!({
        "kind": "EXCEPTION",
        "message": "Cannot read 'id'",
        "frames": ["renderCart (app:200)"]
    });
    assert_ne!(finding_signature(&a), finding_signature(&c));
}

#[test]
fn finding_signature_separates_invariants_and_root_triggers() {
    let rotation = json!({
        "invariant":"no-rotation-loss", "kind":"STATELOSS",
        "message":"state changed", "sig":"before"
    });
    let background = json!({
        "invariant":"no-background-loss", "kind":"STATELOSS",
        "message":"state changed", "sig":"before"
    });
    let another_rotation = json!({
        "invariant":"no-rotation-loss", "kind":"STATELOSS",
        "message":"state changed", "sig":"other"
    });
    assert_ne!(finding_signature(&rotation), finding_signature(&background));
    assert_ne!(
        finding_signature(&rotation),
        finding_signature(&another_rotation)
    );
}

#[test]
fn persisted_finding_report_uses_durable_id_store() {
    let test_name = std::thread::current()
        .name()
        .unwrap_or("test")
        .replace("::", "-");
    let root = std::env::temp_dir().join(format!(
        "reproit-durable-finding-{}-{}",
        std::process::id(),
        test_name
    ));
    let report = root.join("runs/old");
    std::fs::create_dir_all(&report).unwrap();
    std::fs::write(report.join("fuzz.md"), "report").unwrap();
    persist_finding_report(&root, "abc123", &report).unwrap();
    std::fs::write(report.join("fuzz.md"), "later report").unwrap();
    persist_finding_report(&root, "abc123", &report).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join(".reproit/findings/abc123/fuzz.md")).unwrap(),
        "report"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn is_keyed_action_only_accepts_developer_keys() {
    assert!(is_keyed_action("tap:key:testid:remove-p5"));
    assert!(is_keyed_action("type:key:testid:qty=99"));
    // Positional role-index selectors and navigation are fragile, not keyed.
    assert!(!is_keyed_action("tap:role:button#4"));
    assert!(!is_keyed_action("back"));
}

#[test]
fn shrink_target_keeps_exact_identities_on_equal_severity_ties() {
    // Two equally-severe findings retain their exact identities, not only
    // their broad invariant categories.
    let findings = vec![
        json!({
            "invariant": "no-choice-anomaly",
            "kind": "CHOICEANOMALY",
            "message": "picker shifted",
            "sig": "settings"
        }),
        json!({
            "invariant": "no-exception",
            "kind": "EXCEPTION",
            "message": "boom",
            "frames": ["app.dart:12"]
        }),
    ];
    let target = shrink_target(&findings);
    assert_eq!(target.len(), 2);
    assert!(target.contains(&finding_signature(&findings[0])));
    assert!(target.contains(&finding_signature(&findings[1])));
}

#[test]
fn exact_shrink_identity_rejects_a_different_bug_from_the_same_oracle() {
    let original = json!({
        "invariant": "no-broken-render",
        "kind": "CONTENTBUG",
        "message": "undefined at total",
        "sig": "checkout"
    });
    let same = original.clone();
    let other = json!({
        "invariant": "no-broken-render",
        "kind": "CONTENTBUG",
        "message": "undefined at profile",
        "sig": "settings"
    });
    let want = shrink_target(&[original]);
    assert!(reproduces_original(&[same], &want));
    assert!(!reproduces_original(&[other], &want));
}

#[test]
fn write_report_emits_machine_readable_oracle_block() {
    // The `## oracle` block is what `keep` parses to record the finding's
    // oracle category + violating sig.
    let dir = std::env::temp_dir().join(format!("reproit-wr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let findings = vec![json!({
        "invariant": "no-occluded-control",
        "kind": "OCCLUSION",
        "message": "state advanced has an occluded control",
        "sig": "advanced",
        "frames": [],
    })];
    write_report(
        &dir,
        "abcdef123456",
        9,
        &findings,
        &["tap:Advanced".into()],
        &["tap:Advanced".into()],
        reproit_protocol::ConfirmationStatus::Reproduced,
    )
    .unwrap();
    let md = std::fs::read_to_string(dir.join("fuzz.md")).unwrap();
    assert!(md.contains("## oracle"), "missing oracle block:\n{md}");
    assert!(md.contains("- oracle: `occlusion`"), "{md}");
    assert!(md.contains("- invariant: `no-occluded-control`"), "{md}");
    assert!(md.contains("- sig: `advanced`"), "{md}");
    assert!(md.contains("<!-- finding-id: abcdef123456 -->"), "{md}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn evidence_graph_keeps_unconfirmed_observation_as_candidate() {
    let dir = std::env::temp_dir().join(format!("reproit-proof-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("drive-a.log"), "bounded evidence").unwrap();
    let findings = vec![json!({
        "oracle": "contract",
        "invariant": "response-status",
        "kind": "CONTRACT",
        "message": "unexpected status",
    })];
    let proof = write_run_evidence_graph(
        &dir,
        RunEvidence {
            capture_dir: &dir,
            finding_id: "abcdef123456",
            trace: &["tap:key:submit".into()],
            findings: &findings,
            minimized: &["tap:key:submit".into()],
            confirmation: reproit_protocol::ConfirmationStatus::NotAttempted,
            capsule: None,
        },
    )
    .unwrap();
    assert_eq!(
        proof.promotion,
        reproit_protocol::PromotionStatus::Candidate
    );
    assert!(proof
        .blockers
        .contains(&reproit_protocol::PromotionBlocker::ReplayNotReproduced));
    let graph: reproit_protocol::EvidenceGraph =
        serde_json::from_slice(&std::fs::read(dir.join("run-evidence.json")).unwrap()).unwrap();
    assert_eq!(graph.proof_ledger().unwrap(), Some(proof));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn evidence_graph_abstains_without_authority() {
    let dir = std::env::temp_dir().join(format!("reproit-authority-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let findings = vec![json!({
        "oracle": "experimental-shape",
        "invariant": "unusual-shape",
        "kind": "SPECIALIST",
        "message": "unusual observation",
    })];
    let proof = write_run_evidence_graph(
        &dir,
        RunEvidence {
            capture_dir: &dir,
            finding_id: "abcdef123456",
            trace: &[],
            findings: &findings,
            minimized: &[],
            confirmation: reproit_protocol::ConfirmationStatus::Reproduced,
            capsule: None,
        },
    )
    .unwrap();
    assert_eq!(
        proof.evaluation,
        reproit_protocol::EvaluationStatus::Abstain
    );
    assert!(proof
        .blockers
        .contains(&reproit_protocol::PromotionBlocker::MissingAuthority));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn evidence_graph_carries_validated_causal_and_environment_proofs() {
    let dir = std::env::temp_dir().join(format!("reproit-causal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut capsule = crate::domain::capsule::Capsule::new(
        "app",
        crate::domain::capsule::FindingIdentity {
            oracle: "contract".into(),
            invariant: "response-status".into(),
            kind: "CONTRACT".into(),
            message: "unexpected status".into(),
            frame: String::new(),
            trigger: "tap:key:submit".into(),
            boundary: None,
        },
    );
    capsule.environment.insert("platform".into(), "web".into());
    capsule.actions.push(crate::domain::capsule::Action {
        index: 1,
        actor: "a".into(),
        action: "tap:key:submit".into(),
        from_sig: None,
        to_sig: None,
    });
    capsule.finalize_id().unwrap();
    let findings = vec![json!({
        "oracle": "contract",
        "invariant": "response-status",
        "kind": "CONTRACT",
    })];

    write_run_evidence_graph(
        &dir,
        RunEvidence {
            capture_dir: &dir,
            finding_id: "abcdef123456",
            trace: &["tap:key:submit".into()],
            findings: &findings,
            minimized: &["tap:key:submit".into()],
            confirmation: reproit_protocol::ConfirmationStatus::Reproduced,
            capsule: Some(&capsule),
        },
    )
    .unwrap();

    let graph: reproit_protocol::EvidenceGraph =
        serde_json::from_slice(&std::fs::read(dir.join("run-evidence.json")).unwrap()).unwrap();
    graph.validate().unwrap();
    assert!(graph
        .nodes
        .iter()
        .any(|node| node.kind == reproit_protocol::ArtifactKind::CausalGraph));
    assert!(graph
        .nodes
        .iter()
        .any(|node| { node.kind == reproit_protocol::ArtifactKind::EnvironmentEnvelope }));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn finding_category_falls_back_to_kind_then_default() {
    // invariant present -> use it.
    assert_eq!(
        finding_category(&json!({ "invariant": "no-exception", "kind": "X" })),
        "no-exception"
    );
    // no invariant -> use kind.
    assert_eq!(finding_category(&json!({ "kind": "PERF" })), "PERF");
    // neither -> default "exception".
    assert_eq!(finding_category(&json!({ "message": "x" })), "exception");
}

#[test]
fn single_seed_returns_the_whole_log() {
    let log = "FUZZ:ACT tap:A\nFUZZ:ACT back\nJOURNEY DONE\n";
    let segs = split_seed_segments(log, &[plan(7)]);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].0, 7);
    assert_eq!(trace_in_log(segs[0].1), vec!["tap:A", "back"]);
}

#[test]
fn batch_log_splits_per_seed_by_markers() {
    let log = "\
SEED:BEGIN 1
FUZZ:ACT tap:A
EXPLORE:STATE {\"sig\":\"aa\"}
SEED:END 1
SEED:BEGIN 2
FUZZ:ACT tap:B
FUZZ:ACT back
SEED:END 2
JOURNEY DONE
";
    let segs = split_seed_segments(log, &[plan(1), plan(2)]);
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].0, 1);
    assert_eq!(trace_in_log(segs[0].1), vec!["tap:A"]);
    assert_eq!(segs[1].0, 2);
    assert_eq!(trace_in_log(segs[1].1), vec!["tap:B", "back"]);
}

#[test]
fn split_log_segments_one_per_marker_pair() {
    // check batches N identical replays (all the same seed); split by markers
    // without plans yields one segment per SEED:BEGIN/END pair.
    let log = "\
SEED:BEGIN 7
FUZZ:ACT tap:A
SEED:END 7
SEED:BEGIN 7
FUZZ:ACT tap:A
SEED:END 7
";
    let segs = split_log_segments(log);
    assert_eq!(segs.len(), 2);
    assert_eq!(trace_in_log(segs[0]), vec!["tap:A"]);
    assert_eq!(trace_in_log(segs[1]), vec!["tap:A"]);
}

#[test]
fn split_log_segments_unmarked_is_whole_log() {
    // The single-replay (times == 1) path has no markers: one segment = all.
    let log = "FUZZ:ACT tap:A\nJOURNEY DONE\n";
    let segs = split_log_segments(log);
    assert_eq!(segs.len(), 1);
    assert_eq!(trace_in_log(segs[0]), vec!["tap:A"]);
}

#[test]
fn missing_markers_attributes_whole_log_to_each_planned_seed() {
    // An old vendored explorer with no SEED markers: don't drop anything.
    let log = "FUZZ:ACT tap:A\nJOURNEY DONE\n";
    let segs = split_seed_segments(log, &[plan(1), plan(2)]);
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].0, 1);
    assert_eq!(segs[1].0, 2);
    assert_eq!(trace_in_log(segs[0].1), vec!["tap:A"]);
}

#[test]
fn exceptions_in_a_slice_skip_the_test_framework_block() {
    let app = "\
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: The following assertion was thrown:
flutter: A leaked AnimationController was found.
flutter:
flutter: #0 main (package:bugzoo/main.dart:210:5)
flutter: ════════════════════════\
         ════════════════════════
";
    let found = exceptions_in_log(app);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["kind"], "EXCEPTION CAUGHT BY WIDGETS LIBRARY");
    assert!(found[0]["message"]
        .as_str()
        .unwrap()
        .contains("leaked AnimationController"));
    assert!(found[0]["frames"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f.as_str().unwrap().contains("main.dart:210")));

    let framework = "\
flutter: ══╡ EXCEPTION CAUGHT BY FLUTTER TEST FRAMEWORK ╞══
flutter: The following message was thrown:
flutter: boom
flutter: ════════════════════════\
         ════════════════════════
";
    assert!(exceptions_in_log(framework).is_empty());
}

#[test]
fn url_origin_extracts_scheme_and_authority() {
    // A clip's gotoUrl is origin + route, so origin must stop at the authority.
    assert_eq!(
        url_origin("https://app.com/docs/en/home?q=1"),
        Some("https://app.com".to_string())
    );
    assert_eq!(
        url_origin("http://localhost:3000/x"),
        Some("http://localhost:3000".to_string())
    );
    assert_eq!(url_origin("not-a-url"), None);
}

#[test]
fn promotion_keeps_one_canonical_finding_and_a_lightweight_alias() {
    let root =
        std::env::temp_dir().join(format!("reproit-finding-promotion-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let provisional = "aaaaaaaaaaaa";
    let confirmed = "bbbbbbbbbbbb";
    let provisional_dir = layout::finding_dir(&root, provisional);
    let confirmed_dir = layout::finding_dir(&root, confirmed);
    let report_dir = root.join("report");
    std::fs::create_dir_all(&provisional_dir).unwrap();
    std::fs::create_dir_all(&confirmed_dir).unwrap();
    std::fs::create_dir_all(&report_dir).unwrap();
    for directory in [&provisional_dir, &confirmed_dir] {
        std::fs::write(directory.join("fuzz.md"), "report").unwrap();
        std::fs::write(directory.join("run-evidence.json"), "{}").unwrap();
    }

    promote_finding(&root, Some(provisional), confirmed, &report_dir).unwrap();

    assert_eq!(layout::canonical_finding_id(&root, provisional), confirmed);
    assert!(confirmed_dir.join("fuzz.md").is_file());
    assert!(confirmed_dir.join("run-evidence.json").is_file());
    assert!(confirmed_dir.join("status.json").is_file());
    assert!(!provisional_dir.join("fuzz.md").exists());
    assert!(provisional_dir.join("promoted-to").is_file());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn broken_route_recording_matches_each_exact_destination() {
    let routes = vec![
        ("home".into(), "/gone-a".into(), 404, Some("home".into())),
        ("home".into(), "/gone-b".into(), 410, Some("home".into())),
        (
            "pricing".into(),
            "/gone-c".into(),
            404,
            Some("pricing".into()),
        ),
    ];
    let mut used = std::collections::BTreeSet::new();
    let (b, (_, route, status, _)) = broken_route_for_finding(
        &routes,
        "home",
        "following the link to /gone-b returns HTTP 410",
        &used,
    )
    .unwrap();
    assert_eq!((route.as_str(), *status), ("/gone-b", 410));
    used.insert(b);
    let (a, (_, route, _, _)) = broken_route_for_finding(
        &routes,
        "home",
        "following the link to /gone-a returns HTTP 404",
        &used,
    )
    .unwrap();
    assert_eq!(route, "/gone-a");
    assert_ne!(a, b);
    assert_eq!(
        broken_route_for_finding(&routes, "pricing", "document /gone-c returned", &used)
            .unwrap()
            .1
             .1,
        "/gone-c"
    );
}

#[test]
fn boxed_drew_reads_the_last_marker() {
    let dir = std::env::temp_dir().join(format!("reproit-boxed-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(
        dir.join("drive-a.log"),
        "FINDING:BOXED {\"oracle\":\"overflow\",\"drew\":false}\nFINDING:BOXED \
             {\"oracle\":\"overflow\",\"drew\":true}\n",
    )
    .unwrap();
    assert_eq!(boxed_drew(&dir), Some(true));
    std::fs::write(
        dir.join("drive-a.log"),
        "FINDING:BOXED {\"oracle\":\"overflow\",\"drew\":false}\n",
    )
    .unwrap();
    assert_eq!(boxed_drew(&dir), Some(false));
    // No marker at all (an old runner) is distinct from drew:false.
    std::fs::write(dir.join("drive-a.log"), "no marker here\n").unwrap();
    assert_eq!(boxed_drew(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
}
