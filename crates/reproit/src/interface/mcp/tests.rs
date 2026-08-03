use super::*;

/// The set of tool names `tool_defs()` advertises.
fn tool_names() -> Vec<String> {
    tool_defs()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

/// The argv `build_argv` produces for a tool call (panicking the test on the
/// error path so a dispatch test reads cleanly).
fn argv(name: &str, args: Value) -> Vec<String> {
    build_argv(None, name, &args).expect("build_argv should not error for a valid call")
}

#[test]
fn the_full_cloud_loop_tools_are_present() {
    // Every tool the manage+monitor loop needs is advertised: pull (already
    // wired) plus the new triage (set fixed), resolution-events (monitor
    // regressions), and timeline.
    let names = tool_names();
    for want in [
        "reproit_cloud_buckets",
        "reproit_cloud_pull",
        "reproit_cloud_triage",
        "reproit_cloud_resolution_events",
        "reproit_cloud_timeline",
    ] {
        assert!(names.contains(&want.to_string()), "missing tool {want}");
    }
}

#[test]
fn reproduce_dispatches_hermetic_check_or_direct_reference() {
    // With an exec override: the capture routes through check --exec --auto.
    let with_exec = argv(
        "reproit_reproduce",
        json!({"reference": "capture.json", "exec": "node server.js"}),
    );
    assert!(with_exec.contains(&"check".to_string()));
    assert!(with_exec.contains(&"capture.json".to_string()));
    assert!(with_exec.contains(&"--exec".to_string()));
    assert!(with_exec.contains(&"node server.js".to_string()));
    assert!(with_exec.contains(&"--auto".to_string()));
    assert!(with_exec.contains(&"--json".to_string()));
    // Without: the reference resolves its own recipe (occ_ routing, kept
    // guard hermetic.json), exactly like a human typing `reproit <ref>`.
    let direct = argv(
        "reproit_reproduce",
        json!({"reference": "occ_0123456789abcdef"}),
    );
    assert!(direct.contains(&"occ_0123456789abcdef".to_string()));
    assert!(!direct.contains(&"check".to_string()));
}

#[test]
fn verify_dispatches_all_findings_and_named_ids() {
    // No ids => verify the whole persisted suite; the bridge globals are present.
    let all = argv("reproit_verify", json!({}));
    assert!(all.contains(&"--json".to_string()));
    assert!(all.contains(&"verify".to_string()));
    assert!(!all.iter().any(|a| a.starts_with("fnd_")));

    // Named ids are forwarded as positionals to the verify command.
    let named = argv(
        "reproit_verify",
        json!({ "ids": ["fnd_deadbeef0001", "fnd_deadbeef0002"] }),
    );
    assert!(named
        .windows(2)
        .any(|w| w == ["verify", "fnd_deadbeef0001"]));
    assert!(named.contains(&"fnd_deadbeef0002".to_string()));
}

#[test]
fn cloud_triage_read_dispatches_without_status() {
    // No status => READ through the private machine-only Cloud route with no
    // --status, and the bridge's --json / --yes globals are present.
    let argv = argv(
        "reproit_cloud_triage",
        json!({ "app": "demo", "bucket": "b00b" }),
    );
    assert!(argv.contains(&"--json".to_string()));
    assert!(argv.contains(&"--yes".to_string()));
    assert!(argv.windows(2).any(|w| w == ["__cloud-internal", "triage"]));
    assert!(argv.windows(2).any(|w| w == ["--app", "demo"]));
    assert!(argv.windows(2).any(|w| w == ["--bucket", "b00b"]));
    assert!(!argv.iter().any(|a| a == "--status"));
}

#[test]
fn cloud_triage_set_fixed_forwards_status_and_anchor() {
    // status=fixed + fixed_in_build => SET: forwards --status and
    // --fixed-in-build (the prod-resolution anchor) to the CLI.
    let argv = argv(
        "reproit_cloud_triage",
        json!({
            "app": "demo",
            "bucket": "b00b",
            "status": "fixed",
            "fixed_in_build": "1.4.2"
        }),
    );
    assert!(argv.windows(2).any(|w| w == ["--status", "fixed"]));
    assert!(argv.windows(2).any(|w| w == ["--fixed-in-build", "1.4.2"]));
}

#[test]
fn cloud_triage_assigned_forwards_assignee() {
    // An integer assignee is forwarded as a string arg (clap parses it back).
    let argv = argv(
        "reproit_cloud_triage",
        json!({ "app": "demo", "bucket": "b00b", "status": "assigned", "assignee": 42 }),
    );
    assert!(argv.windows(2).any(|w| w == ["--status", "assigned"]));
    assert!(argv.windows(2).any(|w| w == ["--assignee", "42"]));
}

#[test]
fn cloud_triage_requires_app_and_bucket() {
    // Missing the bucket is a tool error (app supplied so we isolate bucket).
    let err = build_argv(None, "reproit_cloud_triage", &json!({ "app": "demo" }))
        .expect_err("missing bucket should error");
    assert!(err.1);
    assert!(err.0.contains("bucket"));
}

#[test]
fn cloud_buckets_dispatches_to_cloud_buckets_not_findings() {
    // The loop-breaker fix: reproit_cloud_buckets must hit the impact-ranked
    // the bucket list endpoint that surfaces bucketId, not the cohort lens.
    let argv = argv("reproit_cloud_buckets", json!({ "app": "demo" }));
    assert!(
        argv.windows(2)
            .any(|w| w == ["__cloud-internal", "buckets"]),
        "expected the internal buckets route, got {argv:?}"
    );
    assert!(
        !argv
            .windows(2)
            .any(|w| w == ["__cloud-internal", "findings"]),
        "must not dispatch to findings"
    );
    assert!(argv.windows(2).any(|w| w == ["--app", "demo"]));
}

#[test]
fn cloud_buckets_forwards_query_filter() {
    let argv = argv(
        "reproit_cloud_buckets",
        json!({ "app": "demo", "query": "checkout" }),
    );
    assert!(argv
        .windows(2)
        .any(|w| w == ["__cloud-internal", "buckets"]));
    assert!(argv.windows(2).any(|w| w == ["--query", "checkout"]));
}

#[test]
fn cloud_resolution_events_dispatches() {
    let argv = argv("reproit_cloud_resolution_events", json!({ "app": "demo" }));
    assert!(argv
        .windows(2)
        .any(|w| w == ["__cloud-internal", "resolution-events"]));
    assert!(argv.windows(2).any(|w| w == ["--app", "demo"]));
}

#[test]
fn cloud_timeline_dispatches_with_bucket() {
    let argv = argv(
        "reproit_cloud_timeline",
        json!({ "app": "demo", "bucket": "b00b" }),
    );
    assert!(argv
        .windows(2)
        .any(|w| w == ["__cloud-internal", "timeline"]));
    assert!(argv.windows(2).any(|w| w == ["--app", "demo"]));
    assert!(argv.windows(2).any(|w| w == ["--bucket", "b00b"]));
}

#[test]
fn unknown_tool_is_an_error() {
    let err = build_argv(None, "reproit_nonexistent", &json!({}))
        .expect_err("an unknown tool should error");
    assert!(err.1);
    assert!(err.0.contains("unknown tool"));
}

#[test]
fn json_has_field_distinguishes_a_verdict_from_a_failure() {
    // A check that produced a verdict carries `outcome` -> NOT a tool error,
    // even when the CLI exited non-zero (fail/flaky/stale are verdicts).
    assert!(json_has_field(
        br#"{"command":"check","outcome":"fail"}"#,
        "outcome"
    ));
    assert!(json_has_field(br#"{"outcome":"stale"}"#, "outcome"));
    // No outcome (a real failure: bad config, repro not found, or non-JSON
    // error text) -> a tool error.
    assert!(!json_has_field(br#"{"command":"check"}"#, "outcome"));
    assert!(!json_has_field(b"Error: no repro `x`", "outcome"));
    assert!(!json_has_field(b"", "outcome"));
}

#[test]
fn check_gloss_distinguishes_all_four_outcomes() {
    // Each of the four verdicts gets a distinct, actionable leading line so an
    // agent reads the outcome without parsing the enum.
    let pass = check_gloss(br#"{"command":"check","outcome":"pass"}"#).unwrap();
    let fail = check_gloss(br#"{"outcome":"fail"}"#).unwrap();
    let flaky = check_gloss(br#"{"outcome":"flaky"}"#).unwrap();
    let stale = check_gloss(br#"{"outcome":"stale"}"#).unwrap();
    assert!(pass.starts_with("PASS"));
    assert!(fail.starts_with("FAIL"));
    assert!(flaky.starts_with("FLAKY"));
    assert!(stale.starts_with("STALE"));
    // All four are different text (no two outcomes collapse to one signal).
    let all = [&pass, &fail, &flaky, &stale];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            assert_ne!(all[i], all[j]);
        }
    }
}

#[test]
fn check_gloss_stale_is_actionable_and_not_a_pass() {
    // The stale gloss must read as "could not run, re-record", never a soft
    // pass. Model maintenance is automatic, never delegated to the agent.
    let stale = check_gloss(br#"{"outcome":"stale"}"#).unwrap();
    assert!(stale.contains("NOT a pass"));
    assert!(stale.contains("refreshes its internal model"));
    assert!(!stale.contains("reproit_map"));
    // It must not contain "PASS"/"FAIL" as a leading verdict that could be
    // misread as a clean/confirmed result.
    assert!(!stale.starts_with("PASS"));
    assert!(!stale.starts_with("FAIL"));
}

#[test]
fn check_gloss_absent_for_non_verdict_output() {
    // No outcome (a real error: bad config / unresolvable repro) -> no gloss,
    // so the error path (stderr surfaced as a tool error) is untouched.
    assert!(check_gloss(br#"{"command":"check"}"#).is_none());
    assert!(check_gloss(b"Error: no repro `x`").is_none());
    assert!(check_gloss(b"").is_none());
    // An unknown outcome string also yields no gloss (we never invent a label).
    assert!(check_gloss(br#"{"outcome":"weird"}"#).is_none());
}

#[test]
fn check_gloss_reads_the_hermetic_verdict_before_the_folded_outcome() {
    // A hermetic capture run folds diverged and inconclusive into `outcome:
    // "stale"`. The gloss must read the precise `capture.verdict` first, so
    // drift is named DIVERGED (re-capture) rather than glossed as a moved UI
    // path, and each of the four hermetic verdicts is distinct.
    let hermetic = |verdict: &str, outcome: &str| {
        let raw = format!(r#"{{"capture":{{"verdict":"{verdict}"}},"outcome":"{outcome}"}}"#);
        check_gloss(raw.as_bytes()).unwrap()
    };
    let reproduced = hermetic("reproduced", "fail");
    let fixed = hermetic("fixed", "pass");
    let diverged = hermetic("diverged", "stale");
    let inconclusive = hermetic("inconclusive", "stale");
    assert!(reproduced.starts_with("FAIL"));
    assert!(reproduced.contains("re-executed code"));
    assert!(fixed.starts_with("PASS"));
    assert!(fixed.contains("hermetic re-execution"));
    assert!(diverged.starts_with("DIVERGED"));
    assert!(diverged.contains("re-capture"));
    assert!(inconclusive.starts_with("INCONCLUSIVE"));
    assert!(inconclusive.contains("NOT a verdict"));
    assert_ne!(diverged, inconclusive);
    // The offline log re-evaluation output carries `capture` without a
    // `verdict`; it must keep the plain outcome gloss.
    let offline = check_gloss(
        br#"{"capture":{"mode":"offline-evaluation","reproduced":true},"outcome":"fail"}"#,
    )
    .unwrap();
    assert!(offline.starts_with("FAIL"));
}

#[test]
fn occurrence_live_resend_output_counts_as_a_verdict() {
    // `reproit occ_x` on the live-resend plan path emits `verdict`, not
    // `outcome`. The bridge must read that as a produced verdict, or a
    // reproduced occurrence (exit 1) would surface as a tool error.
    assert!(json_has_field(
        br#"{"command":"occurrence","mode":"live-resend","verdict":"Reproduced"}"#,
        "verdict"
    ));
    assert!(!json_has_field(b"Error: no occurrence `occ_x`", "verdict"));
}

#[test]
fn scan_check_video_and_baseline_tools_are_present() {
    // The redesigned find/evidence surface is advertised.
    let names = tool_names();
    for want in ["reproit_scan", "reproit_check", "reproit_baseline"] {
        assert!(names.contains(&want.to_string()), "missing tool {want}");
    }
    assert!(!names.contains(&"reproit_record".to_string()));
}

#[test]
fn map_is_internal_and_context_maintains_it() {
    let names = tool_names();
    assert!(!names.contains(&"reproit_map".to_string()));
    let args = argv("reproit_context", json!({}));
    assert!(args.windows(3).any(|w| w == ["debug", "map", "show"]));
}

#[test]
fn scan_dispatches_with_optional_target() {
    // Bare scan -> just the verb (+ the --json global).
    let bare = argv("reproit_scan", json!({}));
    assert_eq!(bare.last().unwrap(), "scan");
    assert!(bare.contains(&"--json".to_string()));
    // A target (URL or alias) is forwarded positionally.
    let scoped = argv("reproit_scan", json!({ "target": "https://app.com" }));
    assert!(scoped.windows(2).any(|w| w == ["scan", "https://app.com"]));
    let route_access = argv("reproit_scan", json!({ "only": "route-access" }));
    assert!(route_access
        .windows(2)
        .any(|w| w == ["--only", "route-access"]));
}

#[test]
fn check_uses_direct_repro_syntax_and_explicit_video_flag() {
    let a = argv("reproit_check", json!({ "repro": "cart-1" }));
    assert!(a.contains(&"@cart-1".to_string()));
    assert!(!a.iter().any(|x| x == "--record-video"));

    let a = argv(
        "reproit_check",
        json!({ "repro": "cart-1", "record_video": true, "flicker": true }),
    );
    assert!(a.contains(&"@cart-1".to_string()));
    assert!(a.contains(&"--record-video".to_string()));
    assert!(a.contains(&"--flicker".to_string()));

    let a = argv("reproit_check", json!({ "changed": "origin/main" }));
    assert!(a.windows(2).any(|w| w == ["--changed", "origin/main"]));

    let error = dispatch::build_argv(
        None,
        "reproit_check",
        &json!({ "repro": "cart-1", "changed": "origin/main" }),
    )
    .unwrap_err();
    assert!(error.0.contains("cannot be combined"));
}

#[test]
fn baseline_dispatches_with_update() {
    let a = argv("reproit_baseline", json!({ "update": true }));
    assert!(a.contains(&"baseline".to_string()));
    assert!(a.contains(&"--update".to_string()));
}

#[test]
fn simplify_and_why_use_the_repro_group() {
    // The advanced repro ops live under the `repro` subcommand now.
    let s = argv(
        "reproit_simplify",
        json!({ "repro": "cart-1", "actions": ["tap:key:testid:add"] }),
    );
    assert!(s.windows(2).any(|w| w == ["repro", "simplify"]));
    let w = argv("reproit_why", json!({ "repro": "cart-1" }));
    assert!(w.windows(2).any(|x| x == ["repro", "why"]));
}

/// The bridge reaches the engine by spawning this same binary with an argv it
/// builds from strings, so a renamed or removed subcommand cannot fail to
/// compile: it fails at run time, inside an agent's session, as "unrecognized
/// subcommand". This test closes that gap by parsing every advertised tool's
/// argv with the real parser. It is the reason the argv path is safe to keep.
#[test]
fn every_advertised_tool_builds_an_argv_the_parser_accepts() {
    use crate::interface::cli::args::Cli;
    use clap::Parser as _;

    // A superset of every argument key `build_argv` reads, so one object
    // satisfies each tool's required set without a per-tool table to drift.
    let args = json!({
        "app": "demo",
        "bucket": "bkt_deadbeef0001",
        "reference": "occ_deadbeef0001",
        "repro": "cart-1",
        "id": "fnd_deadbeef0001",
        "ids": ["fnd_deadbeef0001"],
        "reason": "accepted for the pilot",
        "actions": ["tap:key:testid:add"],
        "name": "checkout",
        "journey": { "steps": [] },
        "as": "checkout-crash",
        "state": "guards",
        "status": "fixed",
        "target": "web",
        "platform": "web",
        "only": "route-access",
        "query": "checkout",
        "kind": "operability",
        // `changed` is deliberately absent: it is mutually exclusive with
        // `repro`, and the point of the superset is one object every tool
        // accepts, not every flag combination.
        "fixed_in_build": "1.2.3",
        "assignee": 1,
        "exec": "node server.js",
        "update": true,
        "record_video": true,
        "flicker": true,
        "remove": false,
    });

    let mut checked = 0usize;
    for name in tool_names() {
        let argv = build_argv(None, &name, &args)
            .unwrap_or_else(|(message, _)| panic!("{name}: build_argv failed: {message}"));
        // The spawned process applies the direct-reference rewrite before clap
        // sees argv (`reproit @alias` is a real dispatch form), so the test
        // parses through the same entry point rather than a shorter one.
        let full = std::iter::once("reproit".to_string())
            .chain(argv.iter().cloned())
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>();
        let full = crate::interface::cli::rewrite::expand_direct_reference_arg(full);
        Cli::try_parse_from(full).unwrap_or_else(|error| {
            panic!("{name} dispatches an argv the CLI cannot parse: {argv:?}\n{error}")
        });
        checked += 1;
    }
    assert!(
        checked >= 20,
        "expected the full tool surface, saw {checked}"
    );
}
