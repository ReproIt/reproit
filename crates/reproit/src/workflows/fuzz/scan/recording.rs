use super::*;

pub(super) struct ScanClipInput<'a> {
    pub(super) findings: &'a [Value],
    pub(super) reported: &'a std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<(String, String, String)>,
    >,
    pub(super) obs: &'a crate::domain::map::RunObs,
    pub(super) scan_run_dir: &'a Path,
    pub(super) cfg_path: &'a Path,
    pub(super) defines: &'a [(String, String)],
}

type BrokenRoute = (String, String, i64, Option<String>);

pub(in crate::workflows::fuzz) fn broken_route_for_finding<'a>(
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

/// Record one clip per distinct reported scan finding. Precisely localized
/// findings are re-detected and boxed; other findings receive a diagnostic
/// recording without changing their authoritative oracle evidence.
///
/// Deduplication uses the same route, oracle, and normalized-detail identity as
/// the user-facing scan summary, so the finding count and recording plan cannot
/// silently diverge.
pub(super) async fn record_scan_clips(
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
        crate::runtime::project_layout::scan_recordings_dir(root, run)
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

    // One clip per distinct reported issue, using the visualization it needs:
    //  - content: land on the screen by URL, re-detect + box.
    //  - broken-route: land on the SOURCE page, box the dead <a> by its href.
    //  - choice-anomaly: land on the screen, tap the outlier option so the page
    //    shifts, box the choice that did it.
    //  - hang / jank: land on the screen, replay the one triggering action, box the
    //    trigger element the runner tags at the tap.
    // Unsupported/non-visual oracles still get a diagnostic recording. The
    // `diagnostic` keeps non-visual evidence separate from visual localization.
    let mut plans: std::collections::BTreeMap<(String, String, String), Value> =
        std::collections::BTreeMap::new();
    let mut used_broken_routes = std::collections::BTreeSet::new();
    for f in input.findings {
        let oracle = crate::domain::oracle::classify(f).as_str().to_string();
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
            "\nscan --record-video: no reported findings to record on this run.".to_string(),
        );
        return Vec::new();
    }
    say(
        json,
        format!(
            "\nscan --record-video: recording all {} distinct finding(s) to {}...",
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
        // VISUALIZATION GATE: the runner marker says whether it drew the requested
        // box. This controls only clip presentation; it never changes the finding
        // or makes a reproduction claim.
        let boxed = match boxed_drew(&outcome.run_dir) {
            Some(t) => t,
            None => {
                // No FINDING:BOXED marker at all: the web runner is older than the
                // binary and does not support the clip protocol (it also ignores
                // the per-clip URL), so every clip would be wrong. Fail loudly with
                // a fix rather than silently dropping all of them.
                say(
                    json,
                    "\nscan --record-video: the web runner is out of date and cannot record \
                     clips for this version.\n  Refresh it: delete the cached runner (re-downloaded \
                     on next run), or set REPROIT_WEB_RUNNER_DIR to a matching runner."
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
        let dest = if boxed && !diagnostic {
            out.join(format!("{label}.webm"))
        } else {
            out.join(format!("{label}.diagnostic.webm"))
        };
        if std::fs::create_dir_all(&out).is_err() {
            say(json, format!("    could not create {}", out.display()));
            continue;
        }
        if std::fs::copy(&src, &dest).is_ok() {
            if boxed && !diagnostic {
                say(json, format!("    saved {}", dest.display()));
            } else if diagnostic {
                say(
                    json,
                    format!(
                        "    saved diagnostic clip {} (this finding has no direct visual target)",
                        dest.display()
                    ),
                );
            } else {
                say(
                    json,
                    format!(
                        "    {label}: could not draw its visual target; saved diagnostic clip {}",
                        dest.display()
                    ),
                );
            }
            clips.push(json!({
                "screen": route,
                "oracle": oracle,
                "clip": dest.to_string_lossy(),
                "recorded": true,
                "visualization": if boxed && !diagnostic { "boxed" } else { "diagnostic" },
            }));
        }
    }
    clips
}

/// `--record-video` for NATIVE targets (desktop AX / mobile), which have no URL to
/// open. A native clip REPLAYS the map's action path from the crawl entry to
/// the exact state the finding was observed on, then the finding's own action
/// -- the runner films its own window (never the desktop) for the whole replay
/// and, when it settles, resolves the finding's element to a window-relative
/// rect + time window (box-spec.json). The host then draws the box with
/// box-overlay.mjs, the uniform post-capture path for every backend that cannot
/// inject a live overlay. A clip whose box cannot be drawn is saved as a
/// diagnostic; box rendering never determines the finding's validity.
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
    let web_dir = match crate::adapters::config::ensure_web_runner_dir(crate::VERSION, &|_| {}) {
        Ok(d) => d,
        Err(e) => {
            say(
                json,
                format!("\nscan --record-video: cannot locate the box-overlay tool: {e}"),
            );
            return Vec::new();
        }
    };
    let overlay = web_dir.join("box-overlay.mjs");

    // One clip per distinct reported finding. Hang/jank visualizations box
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
        let oracle = crate::domain::oracle::classify(f).as_str().to_string();
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
            "\nscan --record-video: no reported findings to record on this run.".to_string(),
        );
        return Vec::new();
    }
    say(
        json,
        format!(
            "\nscan --record-video: recording all {} distinct finding(s) to {}...",
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
        // The marker describes box resolution only. It is not a finding verdict.
        let box_resolved = boxed_drew(&outcome.run_dir).unwrap_or(false);
        let Some(mov) = find_named(&outcome.run_dir, "clip.mov") else {
            say(json, format!("    no video produced for {label}"));
            continue;
        };
        let diagnostic = config.get("diagnostic").and_then(Value::as_bool) == Some(true);
        if std::fs::create_dir_all(out).is_err() {
            say(json, format!("    could not create {}", out.display()));
            continue;
        }
        // Draw the finding box post-capture. With a box-spec present the tool
        // annotates; without one (element never resolved) we still ship the raw
        // film so `--record-video` always yields a clip.
        let spec = find_named(&outcome.run_dir, "box-spec.json");
        let boxed_dest = out.join(format!("{label}.mp4"));
        let diagnostic_dest = out.join(format!("{label}.diagnostic.mp4"));
        let boxed = box_resolved
            && !diagnostic
            && spec.is_some()
            && std::process::Command::new("node")
                .arg(&overlay)
                .arg(&mov)
                .arg(&boxed_dest)
                .arg(spec.as_ref().unwrap())
                .current_dir(&web_dir)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        let dest = if boxed { &boxed_dest } else { &diagnostic_dest };
        if !boxed {
            // No spec, or the overlay failed: fall back to the raw window film so
            // the finding still gets a clip (unboxed, but honest).
            if std::fs::copy(&mov, dest).is_err() {
                say(json, format!("    could not save clip for {label}"));
                continue;
            }
        }
        if boxed {
            say(json, format!("    saved {}", dest.display()));
        } else if diagnostic {
            say(
                json,
                format!(
                    "    saved diagnostic clip {} (this finding has no direct visual target)",
                    dest.display()
                ),
            );
        } else {
            say(
                json,
                format!(
                    "    {label}: could not draw its visual target; saved diagnostic clip {}",
                    dest.display()
                ),
            );
        }
        clips.push(json!({
            "screen": screen,
            "oracle": oracle,
            "clip": dest.to_string_lossy(),
            "recorded": true,
            "visualization": if boxed { "boxed" } else { "diagnostic" },
        }));
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
pub(in crate::workflows::fuzz) fn url_origin(u: &str) -> Option<String> {
    let (scheme, rest) = u.split_once("://")?;
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

/// Whether a clip run's visual box drew, from the LAST `FINDING:BOXED` marker
/// in its drive log. This is presentation state, never a finding verdict.
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
pub(in crate::workflows::fuzz) fn boxed_drew(run_dir: &Path) -> Option<bool> {
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
pub(in crate::workflows::fuzz) fn state_present_footer(
    json: bool,
    sp: &std::collections::BTreeMap<String, String>,
) {
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
pub(super) fn scan_detail(msg: &str) -> String {
    let s = msg
        .strip_prefix("state ")
        .and_then(|rest| rest.split_once(' ').map(|(_sig, tail)| tail))
        .unwrap_or(msg);
    s.split(" (").next().unwrap_or(s).trim().to_string()
}
