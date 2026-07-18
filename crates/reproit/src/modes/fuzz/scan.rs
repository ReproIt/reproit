use super::*;

pub struct ScanArgs {
    pub journey: String,
    pub seed: u64,
    pub budget: u32,
    pub sim: bool,
    pub json: bool,
    /// `--record`: after the crawl, record every distinct reported finding;
    /// exact visual targets are boxed and the rest are diagnostic clips.
    pub record: bool,
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
    let cfg_path = crate::layout::fuzz_config_path(root);
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
    let obs = crate::model::map::parse_run(&log);
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
            crate::crosscut::classify(f).as_str().to_string()
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

    // `--record`: save one clip for every distinct reported finding. Done after
    // report grouping so clip identity exactly matches the visible issue list.
    let clips = if args.record {
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
    let (reproduced_clips, failed_replays, diagnostic_clips) = clip_outcome_counts(&clips);
    if !clips.is_empty() {
        say(json, format!("\n{} clip(s) recorded.", clips.len()));
    }
    if reproduced_clips > 0 {
        say(
            json,
            format!("\n{reproduced_clips} exact visual reproduction(s) confirmed."),
        );
    }
    if failed_replays > 0 {
        say(
            json,
            format!("\n{failed_replays} exact replay(s) did not reproduce."),
        );
    }
    if diagnostic_clips > 0 {
        say(
            json,
            format!(
                "\n{diagnostic_clips} diagnostic clip(s) have no visual target; they make no reproduction claim."
            ),
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
    use crate::crosscut::OracleFilter;

    let raw_oracle = finding.get("oracle").and_then(Value::as_str);
    let (oracle, classification) = if raw_oracle == Some("backend-contract") {
        ("backend-contract", "authoritative")
    } else if raw_oracle == Some("contract") {
        if finding.get("scope").and_then(Value::as_str) != Some("state") {
            return None;
        }
        ("contract", "authoritative")
    } else {
        let classified = crate::crosscut::classify(&finding);
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

fn clip_outcome_counts(clips: &[Value]) -> (usize, usize, usize) {
    let reproduced = clips
        .iter()
        .filter(|clip| clip.get("reproduced").and_then(Value::as_bool) == Some(true))
        .count();
    let failed = clips
        .iter()
        .filter(|clip| clip.get("reproduced").and_then(Value::as_bool) == Some(false))
        .count();
    let diagnostic = clips
        .iter()
        .filter(|clip| clip.get("visualization").and_then(Value::as_str) == Some("diagnostic"))
        .count();
    (reproduced, failed, diagnostic)
}

/// The STATE-PRESENT oracles: bugs visible on a single screen, which is what
/// `scan` reports. Everything else (crash/jank/hang/leak/flicker and the
/// cross-cutting visual/divergence classes) is sequence-dependent or a
/// different mode's job and belongs to `fuzz`/`soak`/`baseline`, not a one-pass
/// A listener LEAK is repeat-dependent (it needs the revisit loop), so the Leak
/// class stays out.
fn is_state_present(oracle: &crate::crosscut::Oracle) -> bool {
    use crate::crosscut::Oracle;
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
    )
    // NB: PermissionWalk is deliberately NOT here. It only exists under a
    // permission-denial ENVIRONMENT sweep and is sequence-dependent (the trap
    // appears after a denial), so it belongs to that sweep, not the one-pass
    // scan crawl.
}

/// Record one clip per DISTINCT REPORTED scan finding. Findings with a precise
/// visual reproduction strategy are re-detected and boxed; every other finding
/// still receives a diagnostic recording of the observed screen. A diagnostic
/// recording makes no reproduction verdict; the finding keeps its own evidence.
///
/// Content bugs are
/// re-detected by drawFindingBoxes on the loaded screen, so a clip = replay
/// the crawl's own action path to that screen, then the runner draws the red
/// box at the end and saves the video. choice-anomaly re-runs its live
/// differential on the loaded screen.
/// Deduplication uses the exact same (route, oracle, normalized detail) identity
/// as the user-facing scan summary, so the finding count and recording plan
/// cannot silently diverge.
struct ScanClipInput<'a> {
    findings: &'a [Value],
    reported: &'a std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<(String, String, String)>,
    >,
    obs: &'a crate::model::map::RunObs,
    scan_run_dir: &'a Path,
    cfg_path: &'a Path,
    defines: &'a [(String, String)],
}

type BrokenRoute = (String, String, i64, Option<String>);

pub(super) fn broken_route_for_finding<'a>(
    routes: &'a [BrokenRoute],
    sig: &str,
    message: &str,
    used: &std::collections::BTreeSet<usize>,
) -> Option<(usize, &'a BrokenRoute)> {
    routes
        .iter()
        .enumerate()
        .find(|(i, (s, route, _status, _from))| {
            s == sig && !used.contains(i) && message.contains(route)
        })
        .or_else(|| {
            routes
                .iter()
                .enumerate()
                .find(|(i, (s, _route, _status, _from))| s == sig && !used.contains(i))
        })
}

async fn record_scan_clips(
    cfg: &Config,
    root: &Path,
    args: &ScanArgs,
    input: ScanClipInput<'_>,
) -> Vec<Value> {
    let json = args.json;
    let out = args.out.clone().unwrap_or_else(|| {
        let run = input
            .scan_run_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("latest");
        crate::layout::scan_recordings_dir(root, run)
    });

    // Web clips navigate by REAL URL (a faithful, hand-followable "open this
    // URL"). Native (desktop/mobile) targets have no URL, so their clips REPLAY
    // the map's action path to the finding, film the app's own window, and box
    // it post-capture -- a different reproduction, handled by record_native_clips.
    let Some(origin) = cfg.app.url.as_deref().and_then(url_origin) else {
        return record_native_clips(cfg, root, args, input, &out).await;
    };
    let route_of = |sig: &str| {
        input
            .obs
            .routes
            .get(sig)
            .cloned()
            .unwrap_or_else(|| sig.to_string())
    };

    // One clip per distinct reported issue, each with the reproduction its bug needs:
    //  - content: land on the screen by URL, re-detect + box.
    //  - broken-route: land on the SOURCE page, box the dead <a> by its href.
    //  - choice-anomaly: land on the screen, tap the outlier option so the page
    //    shifts, box the choice that did it.
    //  - hang / jank: land on the screen, replay the one triggering action, box the
    //    trigger element the runner tags at the tap.
    // Unsupported/non-visual oracles still get a diagnostic recording. The
    // `diagnostic` bit keeps that film separate from exact reproduction truth.
    let mut plans: std::collections::BTreeMap<(String, String, String), Value> =
        std::collections::BTreeMap::new();
    let mut used_broken_routes = std::collections::BTreeSet::new();
    for f in input.findings {
        let oracle = crate::crosscut::classify(f).as_str().to_string();
        let sig = f.get("sig").and_then(Value::as_str).unwrap_or("");
        let route = route_of(sig);
        let detail = scan_detail(f.get("message").and_then(Value::as_str).unwrap_or(""));
        let is_reported = input.reported.get(&route).is_some_and(|items| {
            items.iter().any(|(reported_oracle, _, reported_detail)| {
                reported_oracle == &oracle && reported_detail == &detail
            })
        });
        if !is_reported {
            continue;
        }
        let goto = format!("{origin}{route}");
        let config = match oracle.as_str() {
            "content-bug" => {
                json!({ "replay": [], "highlight": oracle, "gotoUrl": goto })
            }
            "broken-route" => {
                // The dead TARGET route lives on the observation tuple; sig is
                // the screen the finding sits on, which for the end-of-crawl
                // link-check shape is the healthy SOURCE page. Boxing by
                // route_of(sig) hunted the source page's own path, and a
                // same-page "#..." anchor (a Skip to Content link) matched it,
                // so the clip boxed a visually hidden element. Box the tuple's
                // dead route instead; land on the recorded source page (or a
                // reverse edge match by the dead destination as fallback).
                // A source screen can contain several dead links. Match this
                // particular finding to the destination named in its message,
                // then consume the tuple so duplicate source sigs cannot all
                // collapse onto the first href.
                let message = f.get("message").and_then(Value::as_str).unwrap_or("");
                let Some((idx, (_s, dead, status, from))) = broken_route_for_finding(
                    &input.obs.broken_routes,
                    sig,
                    message,
                    &used_broken_routes,
                ) else {
                    plans
                        .entry((route, oracle, detail.clone()))
                        .or_insert_with(|| {
                            json!({
                                "replay": [], "highlight": "diagnostic", "gotoUrl": goto,
                                "diagnostic": true, "findingDetail": detail,
                            })
                        });
                    continue;
                };
                used_broken_routes.insert(idx);
                if from.as_deref() == Some(sig) {
                    // Link-check finding: stay on the healthy source page and
                    // point at the exact anchor the user would activate.
                    let src = route_of(sig);
                    json!({
                        "replay": [],
                        "highlight": oracle,
                        "gotoUrl": format!("{origin}{src}"),
                        "linkHref": dead
                    })
                } else {
                    // Visited dead document: there may be no source anchor in
                    // this state. Navigate straight to it and require the same
                    // 404/410 response plus genuine error-page rendering.
                    json!({
                        "replay": [],
                        "highlight": oracle,
                        "gotoUrl": format!("{origin}{dead}"),
                        "brokenRouteStatus": status
                    })
                }
            }
            "choice-anomaly" => {
                // The scan already identified WHICH option is the outlier, so the
                // clip does not re-run the full differential (that clicked through
                // every option + an A/B re-toggle ON CAMERA -- a jumpy, unwatchable
                // clip). Pass the outlier's label + magnitude so the clip is a calm,
                // minimal reproduction: land, slow-scroll to the picker only if it
                // is off-screen, select the outlier ONCE (the page visibly shifts),
                // box it. The runner falls back to live detection if these are absent.
                let choice = input
                    .obs
                    .choice_bugs
                    .iter()
                    .find(|(from, _r, _o, _s, _m)| from.as_str() == sig);
                match choice {
                    Some((_f, _r, outlier, csel, mag)) => json!({
                        "replay": [], "highlight": "no-choice-anomaly", "gotoUrl": goto,
                        "choiceOutlier": outlier, "choiceSel": csel, "choiceMag": mag,
                    }),
                    None => {
                        json!({ "replay": [], "highlight": "no-choice-anomaly", "gotoUrl": goto })
                    }
                }
            }
            "hang" => {
                let Some(action) = input
                    .obs
                    .hangs
                    .keys()
                    .find(|k| k.0.as_str() == sig)
                    .map(|k| k.1.clone())
                else {
                    plans
                        .entry((route, oracle, detail.clone()))
                        .or_insert_with(|| {
                            json!({
                                "replay": [], "highlight": "diagnostic", "gotoUrl": goto,
                                "diagnostic": true, "findingDetail": detail,
                            })
                        });
                    continue;
                };
                json!({ "replay": [action], "highlight": oracle, "gotoUrl": goto })
            }
            "jank" => {
                let Some(action) = input
                    .obs
                    .janks
                    .keys()
                    .find(|k| k.0.as_str() == sig)
                    .map(|k| k.1.clone())
                else {
                    plans
                        .entry((route, oracle, detail.clone()))
                        .or_insert_with(|| {
                            json!({
                                "replay": [], "highlight": "diagnostic", "gotoUrl": goto,
                                "diagnostic": true, "findingDetail": detail,
                            })
                        });
                    continue;
                };
                json!({ "replay": [action], "highlight": oracle, "gotoUrl": goto })
            }
            _ => json!({
                "replay": [], "highlight": "diagnostic", "gotoUrl": goto,
                "diagnostic": true, "findingDetail": detail,
            }),
        };
        plans.entry((route, oracle, detail)).or_insert(config);
    }

    if plans.is_empty() {
        say(
            json,
            "\nscan --record: no reported findings to record on this run.".to_string(),
        );
        return Vec::new();
    }
    say(
        json,
        format!(
            "\nscan --record: recording all {} distinct finding(s) to {}...",
            plans.len(),
            out.display()
        ),
    );
    let mut clips = Vec::new();
    for ((route, oracle, detail), config) in &plans {
        if std::fs::write(input.cfg_path, config.to_string()).is_err() {
            continue;
        }
        let duplicate_kind = plans
            .keys()
            .filter(|(candidate_route, candidate_oracle, _)| {
                candidate_route == route && candidate_oracle == oracle
            })
            .count()
            > 1;
        let label = if oracle == "broken-route" {
            let discriminator = config
                .get("linkHref")
                .or_else(|| config.get("gotoUrl"))
                .and_then(Value::as_str)
                .unwrap_or(detail);
            format!(
                "{}__{oracle}__{}",
                sanitize_route(route),
                sanitize_route(discriminator)
            )
        } else if duplicate_kind {
            format!(
                "{}__{oracle}__{:08x}",
                sanitize_route(route),
                stable_detail_id(detail)
            )
        } else {
            format!("{}__{oracle}", sanitize_route(route))
        };
        say(json, format!("  {label}..."));
        let outcome = match run_explorer(
            cfg,
            root,
            &args.journey,
            true,
            input.defines,
            false,
            args.sim,
            true,
        )
        .await
        {
            Ok(o) => o,
            Err(_) => {
                say(json, format!("    skipped {label}: run failed"));
                continue;
            }
        };
        // TRUST GATE: a clip whose box drew is a confirmed reproduction. One
        // whose box did NOT draw is still SAVED -- `--record` means every
        // finding gets its film -- but named `.did-not-reproduce.webm` so it can
        // never be mistaken for a confirmed repro (the old behavior silently
        // dropped it, which read as "recording is broken").
        let reproduced = match boxed_drew(&outcome.run_dir) {
            Some(t) => t,
            None => {
                // No FINDING:BOXED marker at all: the web runner is older than the
                // binary and does not support the clip protocol (it also ignores
                // the per-clip URL), so every clip would be wrong. Fail loudly with
                // a fix rather than silently dropping all of them.
                say(
                    json,
                    "\nscan --record: the web runner is out of date and cannot record clips for \
                     this version.\n  Refresh it: delete the cached runner (re-downloaded on next \
                     run), or set REPROIT_WEB_RUNNER_DIR to a matching runner."
                        .to_string(),
                );
                return clips;
            }
        };
        let Some(src) = newest_webm(&outcome.run_dir) else {
            say(json, format!("    no video produced for {label}"));
            continue;
        };
        let diagnostic = config.get("diagnostic").and_then(Value::as_bool) == Some(true);
        let dest = if reproduced {
            out.join(format!("{label}.webm"))
        } else if diagnostic {
            out.join(format!("{label}.diagnostic.webm"))
        } else {
            out.join(format!("{label}.did-not-reproduce.webm"))
        };
        if std::fs::create_dir_all(&out).is_err() {
            say(json, format!("    could not create {}", out.display()));
            continue;
        }
        if std::fs::copy(&src, &dest).is_ok() {
            if reproduced {
                say(json, format!("    saved {}", dest.display()));
            } else if diagnostic {
                say(
                    json,
                    format!(
                        "    saved diagnostic clip {} (no visual target; no reproduction claim)",
                        dest.display()
                    ),
                );
            } else {
                say(
                    json,
                    format!(
                        "    {label}: did not re-fire on this load; clip saved anyway as {}",
                        dest.display()
                    ),
                );
            }
            let clip = if diagnostic {
                json!({
                    "screen": route,
                    "oracle": oracle,
                    "clip": dest.to_string_lossy(),
                    "recorded": true,
                    "visualization": "diagnostic",
                })
            } else {
                json!({
                    "screen": route,
                    "oracle": oracle,
                    "clip": dest.to_string_lossy(),
                    "recorded": true,
                    "visualization": "exact-replay",
                    "reproduced": reproduced,
                })
            };
            clips.push(clip);
        }
    }
    clips
}

/// `--record` for NATIVE targets (desktop AX / mobile), which have no URL to
/// open. A native clip REPLAYS the map's action path from the crawl entry to
/// the exact state the finding was observed on, then the finding's own action
/// -- the runner films its own window (never the desktop) for the whole replay
/// and, when it settles, resolves the finding's element to a window-relative
/// rect + time window (box-spec.json). The host then draws the box with
/// box-overlay.mjs, the uniform post-capture path for every backend that cannot
/// inject a live overlay. Same trust gate as the web path: a clip whose box did
/// not draw is saved but named `.did-not-reproduce.mp4` rather than shipped
/// with a misleading caption.
async fn record_native_clips(
    cfg: &Config,
    root: &Path,
    args: &ScanArgs,
    input: ScanClipInput<'_>,
    out: &Path,
) -> Vec<Value> {
    let json = args.json;
    // box-overlay.mjs lives in the (materialized) web runner dir alongside its
    // node_modules (Playwright, for the caption chip). Resolve it once; without it
    // we can film but not annotate, so bail with a clear message.
    let web_dir = match crate::config::ensure_web_runner_dir(crate::VERSION, &|_| {}) {
        Ok(d) => d,
        Err(e) => {
            say(
                json,
                format!("\nscan --record: cannot locate the box-overlay tool: {e}"),
            );
            return Vec::new();
        }
    };
    let overlay = web_dir.join("box-overlay.mjs");

    // One clip per distinct reported finding. Exact hang/jank reproductions box
    // their triggering control. State findings without a native element selector
    // use a sentinel selector: it deliberately cannot draw a box, but it arms the
    // native recorder and yields an honestly labeled diagnostic film instead of
    // silently omitting the issue.
    let route_of = |sig: &str| {
        input
            .obs
            .routes
            .get(sig)
            .cloned()
            .unwrap_or_else(|| sig.to_string())
    };
    let mut plans: std::collections::BTreeMap<(String, String, String), Value> =
        std::collections::BTreeMap::new();
    for f in input.findings {
        let oracle = crate::crosscut::classify(f).as_str().to_string();
        let sig = f.get("sig").and_then(Value::as_str).unwrap_or("");
        let route = route_of(sig);
        let detail = scan_detail(f.get("message").and_then(Value::as_str).unwrap_or(""));
        let is_reported = input.reported.get(&route).is_some_and(|items| {
            items.iter().any(|(reported_oracle, _, reported_detail)| {
                reported_oracle == &oracle && reported_detail == &detail
            })
        });
        if !is_reported {
            continue;
        }
        // The finding's own action (a "tap:<label>" / "back") comes from the
        // hang/jank keyed maps.
        let action: Option<String> = match oracle.as_str() {
            "hang" => input
                .obs
                .hangs
                .keys()
                .find(|k| k.0.as_str() == sig)
                .map(|k| k.1.clone()),
            "jank" => input
                .obs
                .janks
                .keys()
                .find(|k| k.0.as_str() == sig)
                .map(|k| k.1.clone()),
            // crash/leak/etc: no single element to box on a native window.
            _ => None,
        };
        // Walk the map to the observed state (positional taps only mean anything
        // there); empty path = the state is the crawl entry itself.
        let path = match action_path_to(&input.obs.edges, sig) {
            Some((_start, p)) => p,
            None => Vec::new(),
        };
        let mut replay: Vec<Value> = path.into_iter().map(Value::String).collect();
        let (sel, diagnostic) = if let Some(action) = action {
            if let Some(sel) = action.strip_prefix("tap:").map(str::to_string) {
                replay.push(Value::String(action));
                (sel, false)
            } else {
                ("__reproit_diagnostic__".to_string(), true)
            }
        } else {
            ("__reproit_diagnostic__".to_string(), true)
        };
        let config = json!({
            "replay": replay,
            "diagnostic": diagnostic,
            "findingDetail": detail,
            "clip": { "sel": sel, "label": oracle, "oracle": oracle },
        });
        plans.entry((route, oracle, detail)).or_insert(config);
    }

    if plans.is_empty() {
        say(
            json,
            "\nscan --record: no reported findings to record on this run.".to_string(),
        );
        return Vec::new();
    }
    say(
        json,
        format!(
            "\nscan --record: recording all {} distinct finding(s) to {}...",
            plans.len(),
            out.display()
        ),
    );
    let mut clips = Vec::new();
    for ((screen, oracle, detail), config) in &plans {
        if std::fs::write(input.cfg_path, config.to_string()).is_err() {
            continue;
        }
        let duplicate_kind = plans
            .keys()
            .filter(|(candidate_screen, candidate_oracle, _)| {
                candidate_screen == screen && candidate_oracle == oracle
            })
            .count()
            > 1;
        let label = if duplicate_kind {
            format!(
                "{}__{oracle}__{:08x}",
                sanitize_route(screen),
                stable_detail_id(detail)
            )
        } else {
            format!("{}__{oracle}", sanitize_route(screen))
        };
        say(json, format!("  {label}..."));
        let outcome = match run_explorer(
            cfg,
            root,
            &args.journey,
            true,
            input.defines,
            false,
            args.sim,
            true,
        )
        .await
        {
            Ok(o) => o,
            Err(_) => {
                say(json, format!("    skipped {label}: run failed"));
                continue;
            }
        };
        // Trust gate: FINDING:BOXED drew means the element resolved and the box
        // was written; the runner still filmed the window regardless, so a clip
        // that did not re-fire is saved but flagged (never dropped silently).
        let reproduced = boxed_drew(&outcome.run_dir).unwrap_or(false);
        let Some(mov) = find_named(&outcome.run_dir, "clip.mov") else {
            say(json, format!("    no video produced for {label}"));
            continue;
        };
        let diagnostic = config.get("diagnostic").and_then(Value::as_bool) == Some(true);
        let dest = if reproduced {
            out.join(format!("{label}.mp4"))
        } else if diagnostic {
            out.join(format!("{label}.diagnostic.mp4"))
        } else {
            out.join(format!("{label}.did-not-reproduce.mp4"))
        };
        if std::fs::create_dir_all(out).is_err() {
            say(json, format!("    could not create {}", out.display()));
            continue;
        }
        // Draw the finding box post-capture. With a box-spec present the tool
        // annotates; without one (element never resolved) we still ship the raw
        // film so `--record` always yields a clip.
        let spec = find_named(&outcome.run_dir, "box-spec.json");
        let boxed = spec.is_some()
            && std::process::Command::new("node")
                .arg(&overlay)
                .arg(&mov)
                .arg(&dest)
                .arg(spec.as_ref().unwrap())
                .current_dir(&web_dir)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        if !boxed {
            // No spec, or the overlay failed: fall back to the raw window film so
            // the finding still gets a clip (unboxed, but honest).
            if std::fs::copy(&mov, &dest).is_err() {
                say(json, format!("    could not save clip for {label}"));
                continue;
            }
        }
        if reproduced {
            say(json, format!("    saved {}", dest.display()));
        } else if diagnostic {
            say(
                json,
                format!(
                    "    saved diagnostic clip {} (no visual target; no reproduction claim)",
                    dest.display()
                ),
            );
        } else {
            say(
                json,
                format!(
                    "    {label}: did not re-fire on this load; clip saved anyway as {}",
                    dest.display()
                ),
            );
        }
        let clip = if diagnostic {
            json!({
                "screen": screen,
                "oracle": oracle,
                "clip": dest.to_string_lossy(),
                "recorded": true,
                "visualization": "diagnostic",
            })
        } else {
            json!({
                "screen": screen,
                "oracle": oracle,
                "clip": dest.to_string_lossy(),
                "recorded": true,
                "visualization": "exact-replay",
                "reproduced": reproduced,
            })
        };
        clips.push(clip);
    }
    clips
}

/// Newest file named `name` anywhere under `run_dir` (native runners write
/// `clip.mov` / `box-spec.json` into a `video-<label>/` subdir).
fn find_named(run_dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let mut stack = vec![run_dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.file_name().and_then(|s| s.to_str()) == Some(name) {
                let mt = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                if best.as_ref().map(|(t, _)| mt > *t).unwrap_or(true) {
                    best = Some((mt, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The scheme://authority origin of a URL (e.g. "https://app.com:8080"), for
/// building a per-clip navigation URL by joining it with a finding's route
/// path.
pub(super) fn url_origin(u: &str) -> Option<String> {
    let (scheme, rest) = u.split_once("://")?;
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

/// Whether a clip run's box drew, from the LAST `FINDING:BOXED` marker in its
/// drive log. `Some(true)` drew, `Some(false)` did not reproduce (the clip is
/// still saved, marked did-not-reproduce),
/// The map's action path from a crawl entry to `target`: BFS over the observed
/// edges, starting from each root (a state that never appears as an edge
/// destination; the first edge source as a fallback for a cyclic map). Returns
/// `(start_state, actions)` for the first root that reaches `target`, `None`
/// when no root does. Native hang/jank clips use it because positional taps
/// only mean anything on the exact state where the walk observed them.
fn action_path_to(
    edges: &[(String, String, String)],
    target: &str,
) -> Option<(String, Vec<String>)> {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let dests: BTreeSet<&str> = edges.iter().map(|(_, _, t)| t.as_str()).collect();
    let mut roots: Vec<&str> = Vec::new();
    for (f, _, _) in edges {
        if !dests.contains(f.as_str()) && !roots.contains(&f.as_str()) {
            roots.push(f.as_str());
        }
    }
    if roots.is_empty() {
        roots.extend(edges.first().map(|(f, _, _)| f.as_str()));
    }
    for root in roots {
        if root == target {
            return Some((root.to_string(), Vec::new()));
        }
        // to-state -> (from-state, action) breadcrumb for path reconstruction.
        let mut prev: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        seen.insert(root);
        let mut q: VecDeque<&str> = VecDeque::new();
        q.push_back(root);
        while let Some(cur) = q.pop_front() {
            for (f, a, t) in edges {
                if f == cur && seen.insert(t.as_str()) {
                    prev.insert(t.as_str(), (f.as_str(), a.as_str()));
                    if t == target {
                        let mut path = Vec::new();
                        let mut node = target;
                        while node != root {
                            let (pf, pa) = prev[node];
                            path.push(pa.to_string());
                            node = pf;
                        }
                        path.reverse();
                        return Some((root.to_string(), path));
                    }
                    q.push_back(t.as_str());
                }
            }
        }
    }
    None
}

/// `None` means NO marker at all -- the runner is too old to support the clip
/// protocol, which the caller surfaces as an actionable error rather than a
/// silent drop (the old runner also ignores the per-clip URL).
pub(super) fn boxed_drew(run_dir: &Path) -> Option<bool> {
    let log = std::fs::read_to_string(run_dir.join("drive-a.log")).unwrap_or_default();
    log.lines().rev().find_map(|l| {
        let i = l.find("FINDING:BOXED ")?;
        let v: Value = serde_json::from_str(l[i + "FINDING:BOXED ".len()..].trim()).ok()?;
        v.get("drew").and_then(Value::as_bool)
    })
}

/// Newest `.webm` anywhere under a run dir (the web runner writes it into a
/// `video-<label>/` subdir with a Playwright-assigned name).
fn newest_webm(run_dir: &Path) -> Option<std::path::PathBuf> {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let mut stack = vec![run_dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().and_then(|x| x.to_str()) == Some("webm") {
                let mt = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                if best.as_ref().map(|(t, _)| mt > *t).unwrap_or(true) {
                    best = Some((mt, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// A filesystem-safe clip label from a route ("/docs/en/home" ->
/// "docs-en-home").
fn sanitize_route(route: &str) -> String {
    let s: String = route
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let t = s.trim_matches('-').to_string();
    if t.is_empty() {
        "root".to_string()
    } else {
        t
    }
}

/// Stable, compact suffix for two distinct details of the same oracle on one
/// screen. FNV-1a is intentionally fixed rather than process-randomized.
fn stable_detail_id(detail: &str) -> u32 {
    let mut hash = 0x811c9dc5_u32;
    for byte in detail.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

/// Print the "also saw N state-present issue(s)" footer pointing at `scan`.
/// `fuzz` bundles every violation into one per-seed finding and headlines the
/// crash, so the overflow/content/choice/broken-route issues it walked past
/// are otherwise invisible. This surfaces their counts and routes the user to
/// the command built to report + clip them. No-op when none were seen.
pub(super) fn state_present_footer(json: bool, sp: &std::collections::BTreeMap<String, String>) {
    if sp.is_empty() {
        return;
    }
    let mut by_oracle: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for o in sp.values() {
        *by_oracle.entry(o.as_str()).or_default() += 1;
    }
    let detail = by_oracle
        .iter()
        .map(|(o, n)| format!("{o} x{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    say(
        json,
        format!(
            "\nnote: also saw {} state-present issue(s) on the way ({detail}) -- run `reproit \
             scan` to list + clip them.",
            sp.len()
        ),
    );
}

/// Normalize a finding message into a short, route-stable detail: drop a
/// leading "state <sig> " (so the same issue under different state sigs
/// collapses) and a trailing explanatory parenthetical.
fn scan_detail(msg: &str) -> String {
    let s = msg
        .strip_prefix("state ")
        .and_then(|rest| rest.split_once(' ').map(|(_sig, tail)| tail))
        .unwrap_or(msg);
    s.split(" (").next().unwrap_or(s).trim().to_string()
}

#[cfg(test)]
mod tests {
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
    fn scan_keeps_replay_and_visualization_outcomes_separate() {
        let clips = vec![
            json!({ "visualization": "exact-replay", "reproduced": true }),
            json!({ "visualization": "exact-replay", "reproduced": false }),
            json!({ "visualization": "diagnostic" }),
        ];
        assert_eq!(clip_outcome_counts(&clips), (1, 1, 1));
        assert!(clips[2].get("reproduced").is_none());
    }
}
