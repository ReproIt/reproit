use super::*;

#[test]
fn scan_reports_state_present_specialist_findings() {
    for (invariant, kind, expected) in [
        ("no-choice-anomaly", "CHOICE", "choice-anomaly"),
        ("no-broken-route", "BROKENROUTE", "broken-route"),
    ] {
        let finding = scan_finding(json!({
            "invariant": invariant,
            "kind": kind,
            "message": "state s has valid specialist evidence",
            "sig": "s",
        }))
        .expect("state-present specialist findings belong in scan output");
        assert_eq!(finding["oracle"], expected);
        assert_eq!(finding["classification"], "specialist");
        assert!(finding.get("advisory").is_none());
    }
}

#[test]
fn scan_excludes_sequence_dependent_fuzz_signals() {
    for (invariant, kind) in [
        ("no-exception", "CRASH"),
        ("no-jank", "PERF"),
        ("no-hang", "HANG"),
        ("no-leak", "LEAK"),
        ("paint-flicker", "FLICKER"),
    ] {
        let finding = json!({
            "invariant": invariant,
            "kind": kind,
            "message": "sequence-dependent",
            "sig": "s",
        });
        assert!(
            scan_finding(finding).is_none(),
            "{invariant} must remain fuzz-only"
        );
    }
}

#[test]
fn scan_keeps_only_state_scoped_contracts_as_authoritative() {
    let state = scan_finding(json!({
        "oracle": "contract",
        "scope": "state",
        "message": "authored state contract failed",
    }))
    .expect("state contract belongs in scan");
    assert_eq!(state["classification"], "authoritative");

    let trace = json!({
        "oracle": "contract",
        "scope": "trace",
        "message": "temporal contract failed",
    });
    assert!(scan_finding(trace).is_none());
}

#[test]
fn scan_marks_truncated_and_capped_coverage_incomplete() {
    let gaps = scan_coverage_gaps(
        true,
        concat!(
            "EXPLORE:TRUNCATED {\"reason\":\"action-budget\",\"budget\":20}\n",
            "JOURNEY[a] step: broken-route: 7 candidate link(s) not verified (capped)\n",
        ),
    );
    assert_eq!(gaps.len(), 2);
    assert!(gaps.iter().any(|gap| gap.contains("exploration truncated")));
    assert!(gaps
        .iter()
        .any(|gap| gap.contains("7 link(s) not verified")));
    assert!(scan_coverage_gaps(true, "JOURNEY DONE\n").is_empty());
    assert!(!scan_coverage_gaps(false, "JOURNEY DONE\n").is_empty());
}

#[test]
fn scan_marks_nnn_style_nonzero_exit_without_effective_actions_incomplete() {
    let log = concat!(
        "EXPLORE:STATE {\"sig\":\"cf9df150\",\"labels\":[\"launch output\"]}\n",
        "FUZZ:ACT key:Down\n",
        "FUZZ:OBS {\"sig\":\"cf9df150\"}\n",
        "EXPLORE:COVERAGE {\"platform\":\"tui\",\"complete\":false,",
        "\"states\":1,\"transitions\":0,\"actionsAttempted\":9,",
        "\"actionsEffective\":0,\"sessions\":10,\"nonzeroExits\":9,",
        "\"launchFailures\":0,",
        "\"stopReason\":\"no-effective-actions-after-nonzero-exit\"}\n",
        "JOURNEY DONE\nAll tests passed\n",
    );

    let gaps = scan_coverage_gaps(true, log);

    assert_eq!(gaps.len(), 1);
    assert!(gaps[0].contains("no effective actions after nonzero process exits"));
    assert!(gaps[0].contains("0/9 effective actions"));
}

#[test]
fn scan_accepts_healthy_dynamic_and_static_tui_coverage() {
    let healthy_dynamic = concat!(
        "EXPLORE:STATE {\"sig\":\"home\",\"labels\":[]}\n",
        "EXPLORE:EDGE {\"from\":\"home\",\"action\":\"key:Down\",",
        "\"to\":\"selected\"}\n",
        "EXPLORE:COVERAGE {\"platform\":\"tui\",\"complete\":true,",
        "\"states\":21,\"transitions\":40,\"actionsAttempted\":80,",
        "\"actionsEffective\":45,\"sessions\":2,\"nonzeroExits\":0,",
        "\"launchFailures\":0,\"stopReason\":\"frontier-exhausted\"}\n",
    );
    let healthy_static = concat!(
        "EXPLORE:STATE {\"sig\":\"help\",\"labels\":[]}\n",
        "EXPLORE:COVERAGE {\"platform\":\"tui\",\"complete\":true,",
        "\"states\":1,\"transitions\":0,\"actionsAttempted\":9,",
        "\"actionsEffective\":0,\"sessions\":1,\"nonzeroExits\":0,",
        "\"launchFailures\":0,\"stopReason\":\"frontier-exhausted\"}\n",
    );

    assert!(scan_coverage_gaps(true, healthy_dynamic).is_empty());
    assert!(scan_coverage_gaps(true, healthy_static).is_empty());
}

#[test]
fn scan_fails_closed_on_malformed_coverage_marker() {
    let gaps = scan_coverage_gaps(true, "EXPLORE:COVERAGE {bad}\n");
    assert_eq!(gaps, vec!["coverage marker malformed"]);
}

#[test]
fn scan_collapses_email_decoder_and_generated_dead_link() {
    let mut findings = std::collections::BTreeSet::from([
        (
            "broken-asset".to_string(),
            "specialist".to_string(),
            "script cloudflare-static/email-decode.min.js failure=csp".to_string(),
        ),
        (
            "broken-route".to_string(),
            "specialist".to_string(),
            "dead link /cdn-cgi/l/email-protection returns HTTP 404".to_string(),
        ),
        (
            "choice-anomaly".to_string(),
            "specialist".to_string(),
            "choice differs".to_string(),
        ),
    ]);
    collapse_related_findings(&mut findings);
    assert_eq!(findings.len(), 2);
    assert!(!findings
        .iter()
        .any(|(oracle, _, _)| oracle == "broken-asset"));
}

#[test]
fn scan_counts_only_visualization_outcomes() {
    let clips = vec![
        json!({ "visualization": "boxed" }),
        json!({ "visualization": "diagnostic" }),
    ];
    assert_eq!(clip_visualization_counts(&clips), (1, 1));
    assert!(clips.iter().all(|clip| clip.get("reproduced").is_none()));
}
