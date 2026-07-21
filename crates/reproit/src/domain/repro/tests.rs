use super::*;

#[test]
fn id_is_stable_and_deterministic() {
    let a = repro_id(7, &["tap:Login", "type:user", "tap:Submit"]);
    let b = repro_id(7, &["tap:Login", "type:user", "tap:Submit"]);
    assert_eq!(a, b);
    assert_eq!(a.len(), 12);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn finding_id_scopes_zero_action_findings_and_remains_stable() {
    let a = finding_id(
        "web:https://one.example",
        "crash:no-exception:a",
        0,
        &[] as &[&str],
    );
    let same = finding_id(
        "web:https://one.example",
        "crash:no-exception:a",
        0,
        &[] as &[&str],
    );
    let other_target = finding_id(
        "web:https://two.example",
        "crash:no-exception:a",
        0,
        &[] as &[&str],
    );
    let other_oracle = finding_id(
        "web:https://one.example",
        "occlusion:no-occluded-control:a",
        0,
        &[] as &[&str],
    );

    assert_eq!(a, same);
    assert_ne!(a, other_target);
    assert_ne!(a, other_oracle);
}

#[test]
fn public_prefix_helpers_define_public_id_shapes() {
    let raw = "abcdef123456";
    assert_eq!(display_finding_id(raw), "fnd_abcdef123456");
    assert_eq!(display_repro_id(raw), "rep_abcdef123456");
    assert_eq!(display_finding_id("fnd_abcdef123456"), "fnd_abcdef123456");
    assert_eq!(display_repro_id("rep_abcdef123456"), "rep_abcdef123456");
    assert_eq!(raw_finding_id("fnd_abcdef123456"), Some(raw));
    assert_eq!(raw_repro_id("rep_abcdef123456"), Some(raw));
    assert_eq!(raw_finding_id(raw), None);
    assert_eq!(raw_repro_id(raw), None);
}

#[test]
fn resolve_accepts_public_repro_ids_and_aliases() {
    let root = std::env::temp_dir().join(format!("reproit-rep-{}", std::process::id()));
    let meta = Meta {
        id: "abcdef123456".to_string(),
        alias: Some("checkout".to_string()),
        status: Status::Quarantined,
        seed: 7,
        created: "2026-06-27T00:00:00Z".to_string(),
        last_checked: None,
        last_result: None,
        trigger_index: Some(1),
        trigger_sig: None,
        trigger_selector: None,
        trigger_fingerprint: None,
        oracle: Some("crash".to_string()),
        record_url: None,
        record_action: None,
    };
    save_meta(&root, &meta).unwrap();
    assert!(resolve(&root, "abcdef123456").is_none());
    assert_eq!(resolve(&root, "rep_abcdef123456").unwrap().id, meta.id);
    assert_eq!(resolve(&root, "checkout").unwrap().id, meta.id);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn id_is_whitespace_insensitive_self_deduping() {
    // The same case captured with stray whitespace dedupes to one id.
    let clean = repro_id(1, &["tap:A", "tap:B"]);
    let messy = repro_id(1, &["  tap:A ", "tap:B", "   "]);
    assert_eq!(clean, messy);
}

#[test]
fn id_depends_on_seed_actions_and_order() {
    let base = repro_id(1, &["tap:A", "tap:B"]);
    assert_ne!(base, repro_id(2, &["tap:A", "tap:B"]), "seed matters");
    assert_ne!(base, repro_id(1, &["tap:A", "tap:C"]), "actions matter");
    assert_ne!(base, repro_id(1, &["tap:B", "tap:A"]), "order matters");
}

#[test]
fn verdict_miss_before_first_action_is_could_not_replay_fallback() {
    // No trigger recorded (older repro): a miss on the VERY FIRST action
    // means nothing replayed -> stale by the fallback heuristic.
    let log = "FUZZ:MISS tap:A\nJOURNEY DONE\n";
    assert_eq!(verdict_from_log(log, true), RunVerdict::CouldNotReplay);
}

#[test]
fn unmatched_capsule_request_is_stale_even_if_it_causes_an_error() {
    let log = "CAPSULE:MISS GET /api action=0\nEXCEPTION CAUGHT BY WEB PAGE\nTypeError: \
                   failed fetch\n";
    assert_eq!(
        verdict_from_log_with_trigger(log, false, &Trigger::unknown()),
        RunVerdict::CouldNotReplay
    );
}

#[test]
fn verdict_partial_replay_no_finding_is_green_fallback() {
    // No trigger recorded: at least the first action replayed and no finding
    // fired, so the partial replay is a PASS, not stale.
    let log = "FUZZ:ACT tap:A\nFUZZ:MISS tap:B\nJOURNEY DONE\n";
    assert_eq!(verdict_from_log(log, true), RunVerdict::Green);
}

#[test]
fn verdict_failed_verdict_is_broke_even_with_miss() {
    // A reproduced finding (non-pass verdict) wins over a later miss.
    let log = "FUZZ:ACT tap:A\nFUZZ:MISS tap:B\nJOURNEY DONE\n";
    assert_eq!(verdict_from_log(log, false), RunVerdict::Broke);
}

#[test]
fn verdict_app_exception_is_broke() {
    let log = "\
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: The following assertion was thrown:
flutter: boom
flutter: ════════════════════════
JOURNEY DONE
";
    assert_eq!(verdict_from_log(log, true), RunVerdict::Broke);
}

#[test]
fn verdict_framework_exception_is_not_broke() {
    let log = "\
flutter: ══╡ EXCEPTION CAUGHT BY FLUTTER TEST FRAMEWORK ╞══
flutter: boom
JOURNEY DONE
";
    assert_eq!(verdict_from_log(log, true), RunVerdict::Green);
}

#[test]
fn verdict_failed_verdict_is_broke() {
    assert_eq!(verdict_from_log("JOURNEY DONE\n", false), RunVerdict::Broke);
    assert_eq!(verdict_from_log("JOURNEY DONE\n", true), RunVerdict::Green);
}

// ----- no-verdict guard (the crashed/timed-out runner case) -----
//
// A drive that FAILED but produced neither an app exception NOR any replay
// signal never ran the case (the runner crashed/timed out or hit a setup
// error). It must NOT read as a reproduced finding: that would be a FALSE
// FAIL. The guard classifies it CouldNotReplay -> STALE.

#[test]
fn empty_failed_log_is_could_not_replay_not_false_fail() {
    // The bare case: drive failed, log empty. Old behavior: `!passed` ->
    // Broke -> a FALSE FAIL. Now: no signal, no exception -> CouldNotReplay.
    assert_eq!(verdict_from_log("", false), RunVerdict::CouldNotReplay);
    assert_eq!(verdict_from_log("\n\n", false), RunVerdict::CouldNotReplay);
}

#[test]
fn setup_error_chatter_without_replay_signal_is_could_not_replay() {
    // A drive that failed during setup prints diagnostics but no FUZZ/EXPLORE
    // markers and no JOURNEY DONE: it never replayed the case -> not a verdict.
    let log = "\
flutter: Could not connect to the device.
Error: build failed.
";
    assert_eq!(verdict_from_log(log, false), RunVerdict::CouldNotReplay);
    assert_eq!(classify(&[RunVerdict::CouldNotReplay; 3]), Outcome::Stale);
    assert_eq!(Outcome::Stale.exit_code(), 3);
}

#[test]
fn failed_drive_with_replay_signal_is_still_broke() {
    // The guard must NOT swallow a real reproduction: a failed drive that DID
    // replay (it carries action markers / JOURNEY DONE) is still Broke. This
    // is the line between "the runner died" and "the run failed the case".
    assert_eq!(
        verdict_from_log("FUZZ:ACT tap:A\nJOURNEY DONE\n", false),
        RunVerdict::Broke
    );
    assert_eq!(verdict_from_log("JOURNEY DONE\n", false), RunVerdict::Broke);
}

#[test]
fn failed_drive_with_exception_is_still_broke() {
    // A failed drive carrying an app exception is a reproduction even with no
    // FUZZ markers (the crash fired before/at the first action): the exception
    // is itself the verdict signal, so the guard does not fire.
    let log = "\
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: boom
flutter: ════════════════════════
";
    assert_eq!(verdict_from_log(log, false), RunVerdict::Broke);
}

#[test]
fn classify_all_green_is_pass() {
    let v = vec![RunVerdict::Green; 5];
    assert_eq!(classify(&v), Outcome::Pass);
}

#[test]
fn classify_all_broke_is_fail() {
    let v = vec![RunVerdict::Broke; 3];
    assert_eq!(classify(&v), Outcome::Fail);
}

#[test]
fn classify_mixed_green_broke_is_flaky() {
    let v = vec![
        RunVerdict::Green,
        RunVerdict::Broke,
        RunVerdict::Green,
        RunVerdict::Green,
    ];
    assert_eq!(classify(&v), Outcome::Flaky);
}

#[test]
fn classify_could_not_replay_outranks_fail() {
    // A could-not-reach-trigger outranks a fail mix: re-record beats "failed".
    let v = vec![
        RunVerdict::Green,
        RunVerdict::CouldNotReplay,
        RunVerdict::Broke,
    ];
    assert_eq!(classify(&v), Outcome::Stale);
}

// ----- trigger-context classification (the dogfood fix) -----

/// A trigger context recorded at `keep`: the finding fired after `index`
/// actions. The replay logs below interleave FUZZ:ACT / FUZZ:MISS in order.
fn trig(index: usize) -> Trigger {
    Trigger {
        index: Some(index),
        sig: None,
        selector: None,
        fingerprint: None,
        oracle: None,
    }
}

#[test]
fn crash_repro_that_reproduces_is_fail() {
    // (1) The original exception fires on replay -> FAIL (exit 1).
    let trigger = trig(3);
    let log = "\
FUZZ:ACT tap:A
FUZZ:ACT tap:B
FUZZ:ACT tap:Crash
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: boom
flutter: ════════════════════════
JOURNEY DONE
";
    assert_eq!(
        verdict_from_log_with_trigger(log, true, &trigger),
        RunVerdict::Broke
    );
    assert_eq!(classify(&[RunVerdict::Broke; 3]), Outcome::Fail);
    assert_eq!(Outcome::Fail.exit_code(), 1);
}

#[test]
fn miss_after_trigger_is_pass_the_fixed_bug_case() {
    // (2) The bug is FIXED: no exception, the replay reaches the trigger
    // (performs all 3 trigger actions), and a recorded action AFTER the
    // trigger misses because the fix changed downstream navigation. This
    // must be PASS (exit 0), not stale: the repro stays a green guard.
    let trigger = trig(3);
    let log = "\
FUZZ:ACT tap:A
FUZZ:ACT tap:B
FUZZ:ACT tap:WasCrash
FUZZ:MISS tap:Downstream
JOURNEY DONE
";
    assert_eq!(
        verdict_from_log_with_trigger(log, true, &trigger),
        RunVerdict::Green
    );
    assert_eq!(classify(&[RunVerdict::Green; 3]), Outcome::Pass);
    assert_eq!(Outcome::Pass.exit_code(), 0);
}

#[test]
fn miss_before_trigger_is_stale() {
    // (3) A miss BEFORE reaching the trigger context (only 1 of 3 trigger
    // actions performed): the early path to the bug is gone -> STALE (exit 3).
    let trigger = trig(3);
    let log = "\
FUZZ:ACT tap:A
FUZZ:MISS tap:B
JOURNEY DONE
";
    assert_eq!(
        verdict_from_log_with_trigger(log, true, &trigger),
        RunVerdict::CouldNotReplay
    );
    assert_eq!(classify(&[RunVerdict::CouldNotReplay; 3]), Outcome::Stale);
    assert_eq!(Outcome::Stale.exit_code(), 3);
}

#[test]
fn attempted_action_that_misses_does_not_reach_trigger() {
    let trigger = trig(1);
    let log = "FUZZ:ACT tap:key:load\nFUZZ:MISS tap:key:load\nJOURNEY DONE\n";
    assert_eq!(
        verdict_from_log_with_trigger(log, true, &trigger),
        RunVerdict::CouldNotReplay
    );
}

#[test]
fn clean_full_replay_with_trigger_is_pass() {
    // No miss, no finding, trigger reached: the plainest fixed-bug PASS.
    let trigger = trig(2);
    let log = "FUZZ:ACT tap:A\nFUZZ:ACT tap:B\nJOURNEY DONE\n";
    assert_eq!(
        verdict_from_log_with_trigger(log, true, &trigger),
        RunVerdict::Green
    );
}

#[test]
fn trigger_sig_reached_before_miss_is_pass() {
    // The optional sig path: reaching the recorded trigger sig before any
    // miss counts as reaching the trigger even if the action count fell short.
    let trigger = Trigger {
        index: Some(9),
        sig: Some("SIG:checkout".to_string()),
        selector: None,
        fingerprint: None,
        oracle: None,
    };
    let log = "\
FUZZ:ACT tap:A
EXPLORE:STATE SIG:checkout
FUZZ:MISS tap:Pay
JOURNEY DONE
";
    assert_eq!(
        verdict_from_log_with_trigger(log, true, &trigger),
        RunVerdict::Green
    );
}

#[test]
fn trigger_sig_substring_collision_is_stale_not_pass() {
    // Regression: the recorded trigger sig is a short token that ALSO appears
    // as a substring of an unrelated EARLIER log line (a selector here), but
    // the actual trigger STATE is never reached -- the path moved and the first
    // action missed. The sig must be matched by EQUALITY on EXPLORE:STATE
    // markers only, not by an unanchored `line.contains(sig)`. An unanchored
    // match would falsely set saw_trigger_sig and return Green/Pass, silently
    // turning a stale (should-re-record) repro into a passing one. The correct
    // verdict is CouldNotReplay -> Stale.
    let trigger = Trigger {
        index: Some(9),
        sig: Some("checkout".to_string()),
        selector: None,
        fingerprint: None,
        oracle: None,
    };
    let log = "\
FUZZ:MISS tap:checkout-button
JOURNEY DONE
";
    assert_eq!(
        verdict_from_log_with_trigger(log, true, &trigger),
        RunVerdict::CouldNotReplay
    );

    // And the converse still holds: the sig DOES appear as a proper
    // EXPLORE:STATE marker before the miss -> the trigger was reached -> Green.
    let reached = "\
EXPLORE:STATE {\"sig\":\"checkout\",\"labels\":[\"Pay\"]}
FUZZ:MISS tap:Pay
JOURNEY DONE
";
    assert_eq!(
        verdict_from_log_with_trigger(reached, true, &trigger),
        RunVerdict::Green
    );
}

#[test]
fn trigger_flaky_still_works() {
    // (4) Flakiness across the N runs is unaffected: a deterministic-finding
    // run mixed with clean runs is still FLAKY (exit 2). Each per-run verdict
    // comes from the trigger-aware classifier.
    let trigger = trig(2);
    let clean = "FUZZ:ACT tap:A\nFUZZ:ACT tap:B\nJOURNEY DONE\n";
    let broke = "\
FUZZ:ACT tap:A
FUZZ:ACT tap:B
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: boom
flutter: ════════════════════════
JOURNEY DONE
";
    let verdicts = vec![
        verdict_from_log_with_trigger(clean, true, &trigger),
        verdict_from_log_with_trigger(broke, true, &trigger),
        verdict_from_log_with_trigger(clean, true, &trigger),
    ];
    assert_eq!(verdicts[0], RunVerdict::Green);
    assert_eq!(verdicts[1], RunVerdict::Broke);
    assert_eq!(classify(&verdicts), Outcome::Flaky);
    assert_eq!(Outcome::Flaky.exit_code(), 2);
}

#[test]
fn tester_capture_confirms_only_the_exact_structural_state() {
    let trigger = Trigger {
        index: Some(1),
        sig: Some("broken-checkout".to_string()),
        selector: None,
        fingerprint: None,
        oracle: Some("tester-capture".to_string()),
    };
    let reached = "FUZZ:ACT tap:key:checkout\nEXPLORE:STATE \
                       {\"sig\":\"broken-checkout\"}\nJOURNEY DONE\n";
    let changed =
        "FUZZ:ACT tap:key:checkout\nEXPLORE:STATE {\"sig\":\"fixed-checkout\"}\nJOURNEY DONE\n";
    let premature = "EXPLORE:STATE {\"sig\":\"broken-checkout\"}\nFUZZ:ACT \
                         tap:key:checkout\nEXPLORE:STATE {\"sig\":\"fixed-checkout\"}\n";
    assert_eq!(
        verdict_from_log_with_trigger(reached, true, &trigger),
        RunVerdict::Broke
    );
    assert_eq!(
        verdict_from_log_with_trigger(changed, true, &trigger),
        RunVerdict::CouldNotReplay
    );
    assert_eq!(
        verdict_from_log_with_trigger(premature, true, &trigger),
        RunVerdict::CouldNotReplay
    );
}

#[test]
fn detached_indicator_replay_requires_exact_relationship_and_proof() {
    let trigger = Trigger {
        index: Some(0),
        sig: Some("nav".into()),
        selector: Some("key:id:dot".into()),
        fingerprint: None,
        oracle: Some("detached-indicator".into()),
    };
    let violation = concat!(
        "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[]}\n",
        "EXPLORE:RELATIONSTATUS {\"sig\":\"nav\",\"outcome\":\"VIOLATION\",\"checks\":[",
        "{\"kind\":\"indicator-anchor\",\"dependentKey\":\"key:id:dot\",",
        "\"ownerKey\":\"key:id:tab\",\"containerKey\":\"key:id:tabs\",",
        "\"outcome\":\"VIOLATION\",\"violation\":\"detached\"}]}\n",
        "EXPLORE:RELATION {\"sig\":\"nav\",\"items\":[",
        "{\"kind\":\"indicator-anchor\",\"dependentKey\":\"key:id:dot\",",
        "\"ownerKey\":\"key:id:tab\",\"containerKey\":\"key:id:tabs\",",
        "\"violation\":\"detached\",\"maxGap\":8,\"gap\":90}]}\n",
    );
    assert_eq!(
        verdict_from_log_with_trigger(violation, true, &trigger),
        RunVerdict::Broke
    );
    let satisfied = violation
        .replace("\"outcome\":\"VIOLATION\"", "\"outcome\":\"SATISFIED\"")
        .lines()
        .filter(|line| !line.starts_with("EXPLORE:RELATION "))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        verdict_from_log_with_trigger(&satisfied, true, &trigger),
        RunVerdict::Green
    );
    let abstain = "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[]}\nEXPLORE:RELATIONSTATUS \
                       {\"sig\":\"nav\",\"outcome\":\"ABSTAIN\",\"checks\":[]}";
    assert_eq!(
        verdict_from_log_with_trigger(abstain, true, &trigger),
        RunVerdict::CouldNotReplay
    );
}

#[test]
fn accessibility_state_replay_requires_exact_fingerprint_and_evidence() {
    let trigger = Trigger {
        index: Some(0),
        sig: Some("settings".into()),
        selector: Some("key:id:notifications".into()),
        fingerprint: Some("sha256:f264f36f3b511e4ae5993d43".into()),
        oracle: Some("accessibility-state".into()),
    };
    let violation = concat!(
        "EXPLORE:STATE {\"sig\":\"settings\",\"labels\":[]}\n",
        "EXPLORE:A11YSTATESTATUS {\"sig\":\"settings\",\"outcome\":\"VIOLATION\",\"checks\":[",
        "{\"identity\":\"key:id:notifications\",\"property\":\"checked\",",
        "\"fingerprint\":\"sha256:f264f36f3b511e4ae5993d43\",\"expected\":\"true\",",
        "\"actual\":\"false\",\"outcome\":\"VIOLATION\",",
        "\"reason\":\"semantic-state-mismatch\"}]}\n",
    );
    assert_eq!(
        verdict_from_log_with_trigger(violation, true, &trigger),
        RunVerdict::Broke
    );
    let satisfied = violation
        .replace("\"actual\":\"false\"", "\"actual\":\"true\"")
        .replace("\"outcome\":\"VIOLATION\"", "\"outcome\":\"SATISFIED\"")
        .replace(",\"reason\":\"semantic-state-mismatch\"", "");
    assert_eq!(
        verdict_from_log_with_trigger(&satisfied, true, &trigger),
        RunVerdict::Green
    );
    let abstain = concat!(
        "EXPLORE:STATE {\"sig\":\"settings\",\"labels\":[]}\n",
        "EXPLORE:A11YSTATESTATUS {\"sig\":\"settings\",",
        "\"outcome\":\"ABSTAIN\",\"checks\":[]}",
    );
    assert_eq!(
        verdict_from_log_with_trigger(abstain, true, &trigger),
        RunVerdict::CouldNotReplay
    );
    let mut incomplete = trigger.clone();
    incomplete.fingerprint = None;
    assert_eq!(
        verdict_from_log_with_trigger(violation, true, &incomplete),
        RunVerdict::CouldNotReplay
    );
}

#[test]
fn overflow_replay_requires_same_subject_and_authoritative_clean_evidence() {
    let marker = |right: i64, policy: &str| {
        format!(
            concat!(
                "EXPLORE:STATE {{\"sig\":\"card\",\"labels\":[]}}\n",
                "EXPLORE:OVERFLOW {{\"sig\":\"card\",\"version\":1,",
                "\"complete\":true,\"checks\":[{{",
                "\"subjectKey\":\"key:id:message\",",
                "\"containerKey\":\"key:id:card\",",
                "\"authority\":\"exact-layout\",\"ownership\":\"app\",",
                "\"stableSamples\":2,\"transformed\":false,\"policy\":\"{}\",",
                "\"subjectRect\":{{\"left\":4,\"top\":4,\"right\":{},",
                "\"bottom\":36}},\"containerRect\":{{\"left\":0,\"top\":0,",
                "\"right\":100,\"bottom\":40}}}}]}}\n"
            ),
            policy, right
        )
    };
    let violation = marker(108, "contain");
    let parsed = crate::domain::map::parse_run(&violation);
    let fingerprint = parsed.overflow_checks["card"][0].fingerprint.clone();
    let trigger = Trigger {
        index: Some(0),
        sig: Some("card".into()),
        selector: Some("key:id:message".into()),
        fingerprint: Some(fingerprint),
        oracle: Some("overflow".into()),
    };
    assert_eq!(
        verdict_from_log_with_trigger(&violation, true, &trigger),
        RunVerdict::Broke
    );
    assert_eq!(
        verdict_from_log_with_trigger(&marker(96, "contain"), true, &trigger),
        RunVerdict::Green
    );
    assert_eq!(
        verdict_from_log_with_trigger(&marker(108, "scroll"), true, &trigger),
        RunVerdict::CouldNotReplay
    );
}

#[test]
fn crash_repro_unaffected_by_graph_path() {
    // A crash-oracle repro (or one with no oracle) is untouched: it still
    // uses the exception path, never the graph re-evaluation.
    let crash = Trigger {
        index: Some(2),
        sig: None,
        selector: None,
        fingerprint: None,
        oracle: Some("crash".to_string()),
    };
    let clean = "FUZZ:ACT tap:A\nFUZZ:ACT tap:B\nJOURNEY DONE\n";
    assert_eq!(
        verdict_from_log_with_trigger(clean, true, &crash),
        RunVerdict::Green
    );
    let exc = "\
FUZZ:ACT tap:A
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: boom
flutter: ════════════════════════
JOURNEY DONE
";
    assert_eq!(
        verdict_from_log_with_trigger(exc, true, &crash),
        RunVerdict::Broke
    );
}

#[test]
fn outcome_severity_orders_for_suite_worst() {
    assert!(Outcome::Fail > Outcome::Flaky);
    assert!(Outcome::Flaky > Outcome::Stale);
    assert!(Outcome::Stale > Outcome::Pass);
    // The suite's worst is the max.
    let outcomes = [Outcome::Pass, Outcome::Stale, Outcome::Pass];
    assert_eq!(*outcomes.iter().max().unwrap(), Outcome::Stale);
}

#[test]
fn exit_codes_match_the_contract() {
    assert_eq!(Outcome::Pass.exit_code(), 0);
    assert_eq!(Outcome::Fail.exit_code(), 1);
    assert_eq!(Outcome::Flaky.exit_code(), 2);
    assert_eq!(Outcome::Stale.exit_code(), 3);
}

#[test]
fn check_result_reports_rate() {
    let v = vec![RunVerdict::Green, RunVerdict::Broke, RunVerdict::Green];
    let r = CheckResult::from_verdicts(&v);
    assert_eq!(r.outcome, Outcome::Flaky);
    assert_eq!(r.rate(), "2/3");
}
