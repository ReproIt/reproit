use super::*;

mod recording;

pub(super) use recording::state_present_footer;
#[cfg(test)]
pub(super) use recording::{boxed_drew, broken_route_for_finding, url_origin};
use recording::{record_scan_clips, scan_detail, ScanClipInput};

pub struct ScanArgs {
    pub journey: String,
    pub seed: u64,
    pub budget: u32,
    pub sim: bool,
    pub json: bool,
    /// `--record-video`: after the crawl, record every distinct reported
    /// finding; exact visual targets are boxed and the rest are diagnostic clips.
    pub record_video: bool,
    /// `--out <dir>`: where the clips land (default
    /// `.reproit/recordings/scan/<scan-run>/`).
    pub out: Option<std::path::PathBuf>,
}

/// SCAN: the coverage finder. Where `fuzz` permutes action sequences to provoke
/// SEQUENCE-dependent bugs (crash/jank/hang), `scan` does ONE crawl that visits
/// every reachable screen once and reports the STATE-PRESENT bugs simply
/// visible on each (overflow / content / choice-anomaly) - one finding per
/// (screen x issue), no per-seed collapse. Results retain their authoritative
/// or specialist policy classification; both are findings whose own oracle
/// predicate held. The runner already emits these markers on any walk; scan is
/// about COLLECTING and reporting them, not new detection.
/// Returns coverage completeness and the reported issue count. The caller
/// exits non-zero for either partial coverage or findings so CI cannot read
/// either as a clean pass.
pub struct ScanSummary {
    pub complete: bool,
    pub issues: usize,
    /// Evidence directory produced by the coverage walk. Callers may commit
    /// this exact run into the app map instead of launching a duplicate crawl.
    pub run_dir: std::path::PathBuf,
}

pub async fn scan(cfg: &Config, root: &Path, args: &ScanArgs) -> Result<ScanSummary> {
    let json = args.json;
    let cfg_path = crate::runtime::project_layout::fuzz_config_path(root);
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    let defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    // One coverage walk: a generous budget lets the explorer reach the reachable
    // screens once. We do not permute seeds - state-present bugs are
    // path-independent.
    let config = json!({ "seed": args.seed, "budget": args.budget });
    std::fs::write(&cfg_path, config.to_string())?;
    say(
        json,
        "scan: one coverage walk (every reachable screen, checked once)...".to_string(),
    );
    let outcome = run_explorer(
        cfg,
        root,
        &args.journey,
        false,
        &defines,
        false,
        args.sim,
        false,
    )
    .await?;
    let log = std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
    let coverage_gaps = scan_coverage_gaps(outcome.passed, &log);
    let completed = coverage_gaps.is_empty();
    let mut evidence = crate::domain::evidence::EvidenceCounts::from_log(&log);

    // ALL per-state observations (every state x oracle), NOT collapsed to one per
    // seed. Objective/authored findings and specialist findings are both valid
    // oracle observations and both remain visible. Their separate classification
    // records the policy boundary without erasing or downgrading either finding.
    // The sequence-dependent oracles (crash, jank, hang, leak, flicker) are
    // `fuzz`'s job: a single coverage crawl can trip them flakily, so surfacing
    // them here contradicted the documented scan contract and was the main source
    // of scan non-determinism. They still land in the run log for `fuzz`.
    let findings: Vec<Value> = findings_for_tier(cfg, &outcome.run_dir, args.sim)
        .into_iter()
        .filter_map(scan_finding)
        .collect();
    // BOT-WALL / UNSCANNABLE: the runner hit a WAF challenge interstitial and never
    // reached the app, so any oracle output would be about the interstitial, not
    // the app. Surface the remediation prominently and emit ZERO findings.
    if let Some(line) = log.lines().find(|l| l.contains("EXPLORE:UNSCANNABLE")) {
        let diag = line
            .split_once("EXPLORE:UNSCANNABLE ")
            .and_then(|(_, j)| serde_json::from_str::<Value>(j).ok())
            .and_then(|v| {
                v.get("diagnostic")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "target is unscannable (bot-challenge)".to_string());
        let mut coverage_gaps = coverage_gaps;
        coverage_gaps.push(format!("unscannable: {diag}"));
        // Any oracle markers belong to the challenge interstitial, not the app.
        // Discard them instead of attributing third-party evidence to the target.
        let unscannable_evidence = crate::domain::evidence::EvidenceCounts::default();
        if json {
            println!(
                "{}",
                json!({
                    "command": "scan",
                    "complete": false,
                    "unscannable": true,
                    "diagnostic": diag,
                    "screens_scanned": 0,
                    "screens_with_findings": 0,
                    "issues": 0,
                    "evidenceStatus": unscannable_evidence.status(false),
                    "evidence": unscannable_evidence,
                    "coverage_gaps": coverage_gaps,
                    "results": [],
                    "clips": []
                })
            );
        } else {
            say(json, format!("\nscan: UNSCANNABLE -- {diag}"));
        }
        return Ok(ScanSummary {
            complete: false,
            issues: 0,
            run_dir: outcome.run_dir.clone(),
        });
    }
    let obs = crate::domain::map::parse_run(&log);
    // Distinct screens actually crawled (routes when the runner emits them, else
    // state sigs) -- the coverage denominator, NOT "screens with findings".
    let swept = {
        let routes: std::collections::BTreeSet<&String> = obs.routes.values().collect();
        if routes.is_empty() {
            obs.states.len()
        } else {
            routes.len()
        }
    };

    // Group per SCREEN (route), deduped by (oracle, detail). A route is the
    // user's mental "screen": the same overflow/anomaly visited via several state
    // sigs is one issue, not N, so we key on the route and strip the per-sig
    // prefix from each detail. Genuinely different details on one route (6 spills
    // vs 2) stay distinct because their normalized text differs.
    let mut by_screen: std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<(String, String, String)>,
    > = std::collections::BTreeMap::new();
    let route_of = |sig: &str| {
        obs.routes
            .get(sig)
            .cloned()
            .unwrap_or_else(|| sig.to_string())
    };
    for f in &findings {
        let raw_oracle = f.get("oracle").and_then(Value::as_str);
        let oracle = if raw_oracle == Some("backend-contract") {
            "backend-contract".to_string()
        } else {
            crate::domain::oracle::classify(f).as_str().to_string()
        };
        let sig = f.get("sig").and_then(Value::as_str).unwrap_or("-");
        let route = f
            .get("operation")
            .and_then(Value::as_str)
            .filter(|_| raw_oracle == Some("backend-contract"))
            .map(|operation| format!("backend:{operation}"))
            .unwrap_or_else(|| route_of(sig));
        let detail = scan_detail(f.get("message").and_then(Value::as_str).unwrap_or(""));
        let classification = f
            .get("classification")
            .and_then(Value::as_str)
            .unwrap_or("authoritative")
            .to_string();
        by_screen
            .entry(route)
            .or_default()
            .insert((oracle, classification, detail));
    }
    for items in by_screen.values_mut() {
        collapse_related_findings(items);
    }

    let issues: usize = by_screen.values().map(|s| s.len()).sum();
    let unreported_violations = by_screen
        .values()
        .flat_map(|items| items.iter())
        .filter(|(oracle, _, _)| !crate::domain::evidence::has_explicit_status_marker(oracle))
        .count();
    evidence.observe_unreported_violations(unreported_violations);

    // `--record-video`: save one clip for every distinct reported finding. Done
    // after grouping so clip identity exactly matches the visible issue list.
    let clips = if args.record_video {
        let clip_input = ScanClipInput {
            findings: &findings,
            reported: &by_screen,
            obs: &obs,
            scan_run_dir: &outcome.run_dir,
            cfg_path: &cfg_path,
            defines: &defines,
        };
        record_scan_clips(cfg, root, args, clip_input).await
    } else {
        Vec::new()
    };

    if json {
        let results: Vec<Value> = by_screen
            .iter()
            .map(|(route, items)| {
                json!({
                    "screen": route,
                    "findings": items
                        .iter()
                        .map(|(o, c, d)| {
                            json!({"oracle": o, "classification": c, "detail": d})
                        })
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            json!({
                "command": "scan",
                "complete": completed,
                "screens_scanned": swept,
                "screens_with_findings": by_screen.len(),
                "issues": issues,
                "evidenceStatus": evidence.status(completed),
                "evidence": evidence,
                "coverage_gaps": coverage_gaps,
                "results": results,
                "clips": clips
            })
        );
        return Ok(ScanSummary {
            complete: completed,
            issues,
            run_dir: outcome.run_dir.clone(),
        });
    }

    let summary = if issues == 0 {
        format!("\nscan: {swept} screen(s) scanned; no issues found")
    } else {
        format!(
            "\nscan: {swept} screen(s) scanned; {} with {issues} distinct issue(s)",
            by_screen.len(),
        )
    };
    say(json, summary);
    for (route, items) in &by_screen {
        say(json, format!("\n  {route}"));
        for (oracle, classification, detail) in items {
            say(json, format!("    {oracle:16} [{classification}] {detail}"));
        }
    }
    let (boxed_clips, diagnostic_clips) = clip_visualization_counts(&clips);
    if !clips.is_empty() {
        say(json, format!("\n{} clip(s) recorded.", clips.len()));
    }
    if boxed_clips > 0 {
        say(json, format!("\n{boxed_clips} finding(s) visually boxed."));
    }
    if diagnostic_clips > 0 {
        say(
            json,
            format!("\n{diagnostic_clips} diagnostic clip(s) saved without a visual box."),
        );
    }
    // Honest about partial coverage: a cut-short crawl did NOT check every screen,
    // so don't let it read as a clean pass (the caller also exits non-zero).
    if !completed {
        let gaps = coverage_gaps.join("; ");
        say(
            json,
            format!(
                "\nscan: coverage INCOMPLETE -- {gaps}. Some screens or links were not checked. \
                 Raise --budget or journeys.timeoutSec to go deeper."
            ),
        );
    }
    Ok(ScanSummary {
        complete: completed,
        issues,
        run_dir: outcome.run_dir.clone(),
    })
}

/// Prepare a finding for scan output. Scan has a different selection boundary
/// from fuzz: a specialist state observation is a valid finding on the screen
/// where its oracle predicate held. Keep it visible and record the policy class
/// separately from the finding itself.
fn scan_finding(mut finding: Value) -> Option<Value> {
    use crate::domain::oracle::OracleFilter;

    let raw_oracle = finding.get("oracle").and_then(Value::as_str);
    let (oracle, classification) = if raw_oracle == Some("backend-contract") {
        ("backend-contract", "authoritative")
    } else if raw_oracle == Some("contract") {
        if finding.get("scope").and_then(Value::as_str) != Some("state") {
            return None;
        }
        ("contract", "authoritative")
    } else {
        let classified = crate::domain::oracle::classify(&finding);
        if !is_state_present(&classified) {
            return None;
        }
        let classification = if OracleFilter::stable().allows(classified) {
            "authoritative"
        } else {
            "specialist"
        };
        (classified.as_str(), classification)
    };

    let object = finding.as_object_mut()?;
    object.insert("oracle".into(), Value::String(oracle.into()));
    object.insert(
        "classification".into(),
        Value::String(classification.into()),
    );
    Some(finding)
}

/// Coverage is independent from findings. A clean process can still have a
/// deterministic work cap or leave links unchecked, so it must not be
/// reported as a complete clean scan.
fn scan_coverage_gaps(process_passed: bool, log: &str) -> Vec<String> {
    let mut gaps = std::collections::BTreeSet::new();
    if !process_passed {
        gaps.insert("runner did not pass".to_string());
    }
    for line in log.lines() {
        if let Some((_, detail)) = line.split_once("EXPLORE:COVERAGE ") {
            if let Some(gap) = coverage_marker_gap(detail) {
                gaps.insert(gap);
            }
        }
        if let Some((_, detail)) = line.split_once("EXPLORE:TRUNCATED ") {
            gaps.insert(format!("exploration truncated: {}", detail.trim()));
        }
        let Some((prefix, _)) = line.split_once(" candidate link(s) not verified (capped)") else {
            continue;
        };
        let count = prefix.split_whitespace().last().unwrap_or("unknown");
        gaps.insert(format!(
            "broken-route verification capped: {count} link(s) not verified"
        ));
    }
    gaps.into_iter().collect()
}

fn coverage_marker_gap(detail: &str) -> Option<String> {
    let Ok(value) = serde_json::from_str::<Value>(detail) else {
        return Some("coverage marker malformed".to_string());
    };
    match value.get("complete").and_then(Value::as_bool) {
        Some(true) => return None,
        Some(false) => {}
        None => return Some("coverage marker missing completion status".to_string()),
    }
    let reason = match value.get("stopReason").and_then(Value::as_str) {
        Some("launch-failed") => "launch failed",
        Some("no-effective-actions-after-nonzero-exit") => {
            "no effective actions after nonzero process exits"
        }
        Some("crash") => "runner crashed",
        _ => "runner reported incomplete coverage",
    };
    let states = value.get("states").and_then(Value::as_u64).unwrap_or(0);
    let attempted = value
        .get("actionsAttempted")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let effective = value
        .get("actionsEffective")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(format!(
        "coverage incomplete: {reason} ({states} state(s), {effective}/{attempted} effective \
         actions)"
    ))
}

/// Keep one user-impact finding when a failed helper resource and its broken
/// generated link are two observations of the same feature failure. The raw
/// evidence remains in the run log; only the scan summary is causally deduped.
fn collapse_related_findings(items: &mut std::collections::BTreeSet<(String, String, String)>) {
    let broken_email_link = items.iter().any(|(oracle, _, detail)| {
        oracle == "broken-route" && detail.contains("/cdn-cgi/l/email-protection")
    });
    if broken_email_link {
        items.retain(|(oracle, _, detail)| {
            oracle != "broken-asset" || !detail.contains("cloudflare-static/email-decode.min.js")
        });
    }
}

fn clip_visualization_counts(clips: &[Value]) -> (usize, usize) {
    let boxed = clips
        .iter()
        .filter(|clip| clip.get("visualization").and_then(Value::as_str) == Some("boxed"))
        .count();
    let diagnostic = clips
        .iter()
        .filter(|clip| clip.get("visualization").and_then(Value::as_str) == Some("diagnostic"))
        .count();
    (boxed, diagnostic)
}

/// The STATE-PRESENT oracles: bugs visible on a single screen, which is what
/// `scan` reports. Everything else (crash/jank/hang/leak/flicker and the
/// cross-cutting visual/divergence classes) is sequence-dependent or a
/// different mode's job and belongs to `fuzz`/`soak`/`baseline`, not a one-pass
/// A listener LEAK is repeat-dependent (it needs the revisit loop), so the Leak
/// class stays out.
fn is_state_present(oracle: &crate::domain::oracle::Oracle) -> bool {
    use crate::domain::oracle::Oracle;
    matches!(
        oracle,
        Oracle::ContentBug
            | Oracle::DetachedIndicator
            | Oracle::ChoiceAnomaly
            | Oracle::BrokenRoute
            | Oracle::Occlusion
            | Oracle::Security
            | Oracle::StuckKeyboard
            | Oracle::BlankScreen
            | Oracle::BrokenAsset
            | Oracle::ZoomReflow
            | Oracle::Invariant
            // Safe-area is a single-screen geometry check (a control in a device
            // inset is visible on the one screen), so the scan crawl reports it.
            | Oracle::SafeArea
            // Zero-contrast is a single-screen attribute equality check (the
            // invisible run is present on the one settled screen).
            | Oracle::ZeroContrast
            // Dead-input is a single-screen interaction probe (the swallowed
            // input is demonstrable on the one settled screen).
            | Oracle::DeadInput
    )
    // NB: PermissionWalk is deliberately NOT here. It only exists under a
    // permission-denial ENVIRONMENT sweep and is sequence-dependent (the trap
    // appears after a denial), so it belongs to that sweep, not the one-pass
    // scan crawl.
}

#[cfg(test)]
mod tests;
