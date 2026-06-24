//! `reproit fuzz`: seeded, replayable walks over the app, with the structured
//! exception pipeline as the oracle and greedy shrinking of failures.
//!
//! Determinism contract: the action SEQUENCE is fully determined by
//! (seed, app build): the explorer's RNG is xorshift32 over the sorted
//! tappable set. The fuzz config travels via a host file whose PATH is the
//! only dart-define, so one build serves every seed and replay (warm runs).
//!
//! Oracle v0: any app exception record (kind not from the test framework)
//! or a failed run verdict. Shrinking: greedy single-removal replays until
//! no action can be dropped, capped; the shrunk trace is the repro.

use crate::config::Config;
use crate::orchestrator::{self, RunOutcome};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MAX_SHRINK_REPLAYS: usize = 10;
/// Perf oracle: a walk whose jank exceeds this is a finding. Generous on
/// purpose: debug builds are JIT-skewed, and startup frames always jank.
const JANK_PCT_MAX: f64 = 25.0;

#[derive(Clone)]
pub struct FuzzArgs {
    pub journey: String,
    pub seed: u64,
    pub runs: u32,
    pub budget: u32,
    pub shrink: bool,
    /// Collect findings across the whole seed budget instead of stopping at the
    /// first, then group them by crash signature into UNIQUE bugs (so the same
    /// bug reached by different paths is reported once). The "fuzz and fix"
    /// work-list: an agent gets the deduped set of real bugs in one shot.
    pub all: bool,
    /// Coverage-guided: replay a computed path to the least-visited state,
    /// then explore from there with the seeded suffix.
    pub frontier: bool,
    /// A/B control: disable inverse-visit-count scoring + power schedule
    /// (uniform-random pick, fixed budget). For measuring the upgrades.
    pub uniform: bool,
    /// Production-seeded fuzzing: path to a JSON array of real user action
    /// paths (e.g. exported from SDK telemetry: [["key:Tab","key:Enter"], ...]).
    /// The fuzzer replays one per session, then branches outward from it. Bugs
    /// cluster where users actually go, and reaching a valid deep state is the
    /// costly part, so a real path gets us there for free.
    pub seeds_file: Option<String>,
    /// Seeds per drive session. 0 = all `runs` in one session (the big win:
    /// install/launch/connect amortized once). 1 = legacy one-drive-per-seed.
    pub batch: u32,
    /// Print the per-phase wall-clock breakdown for each drive session.
    pub profile_timing: bool,
    /// Force the SIMULATOR tier (today's `flutter drive` on an iOS sim). The
    /// default is the HEADLESS tier (`flutter test`, no sim). Use --sim when an
    /// oracle needs the live runtime: jank/frame-timing, keyboard/IME,
    /// platform plugins, or to record a repro video.
    pub sim: bool,
    /// On a headless finding, do ONE simulator run of the minimized repro to
    /// confirm on the real runtime (and be where the annotated video later
    /// gets recorded). Off by default so verification stays pure-headless.
    pub confirm_on_sim: bool,
    /// Cloud base URL. When set, a finding triggers the delivery pipeline:
    /// annotate + upload the minimized-repro clip, then emit the PR-comment
    /// markdown (dry-run unless a GitHub repo+token+PR are resolvable). Without
    /// it, fuzz just writes fuzz.md as before.
    pub cloud: Option<String>,
    /// Cloud app id the finding's evidence attaches to (required with --cloud).
    pub app: Option<String>,
    /// Finding index in the cloud error list to attach evidence to.
    pub app_idx: usize,
    /// Actually POST the PR comment (otherwise the delivery pipeline emits the
    /// markdown as a dry-run). Posting still needs GITHUB_TOKEN + repo + PR.
    pub post_comment: bool,
    /// Global `--json`: stdout must be a single clean JSON object (the caller
    /// emits it after `fuzz` returns). All human progress lines are routed
    /// to stderr so they never corrupt the machine surface, matching how
    /// `repros --json` / `map --json` behave.
    pub json: bool,
    /// Locales to fuzz across (`--locale de,ar,ja`). Empty = the app default
    /// (one run, no REPROIT_LOCALE define). When non-empty the flow runs once
    /// per locale; every finding is tagged with the locale it was found under
    /// and the locale travels to the runner as REPROIT_LOCALE.
    pub locales: Vec<String>,
    /// Oracle include/exclude filter from `--only`/`--no`. Default is all-on.
    /// Kept findings are tagged with their `oracle` category.
    pub oracle_filter: crate::crosscut::OracleFilter,
    /// `fuzz --from <journey>`: a journey's resolved action sequence, replayed as
    /// the prefix for every seed so the seeded walk branches outward from the
    /// journey's end state. Resolved host-side in main.rs (secrets bound, map
    /// `goto`s expanded) so a bad journey fails before any drive. Takes
    /// precedence over `--frontier` (the journey IS the chosen path in).
    pub from_prefix: Option<Vec<String>>,
}

/// Human progress line. Under `--json`, stdout must stay a single clean JSON
/// object, so every human line is routed to stderr instead (matching how
/// `repros --json` / `map --json` keep stdout machine-clean).
fn say(json: bool, line: impl std::fmt::Display) {
    if json {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

/// One seed's resolved walk config within a batch: the exact inputs the
/// explorer needs to reproduce this seed's walk byte-for-byte. Built from the
/// PRE-BATCH map/visits snapshot (see `fuzz` doc on the shared-snapshot
/// tradeoff).
struct SeedPlan {
    seed: u64,
    config: Value,
}

/// `fuzz` entry: with no `--locale`, runs the flow once (app default). With one
/// or more locales, runs the flow once PER locale (each with REPROIT_LOCALE
/// set), tags every finding with its locale, and reports findings that appear
/// in some locale but not all (locale-specific i18n findings).
pub async fn fuzz(cfg: &Config, root: &Path, args: &FuzzArgs) -> Result<()> {
    // Crash-reporter suppression (native backends only). A native fuzz that
    // finds N crashes would pop N OS crash dialogs; this turns the dialog off
    // for the run and RESTORES the prior setting on Drop (even on error/panic).
    // Web/headless runs get an inert guard and touch no system setting.
    let _crash_guard = crash_guard_for(cfg);
    if args.locales.is_empty() {
        fuzz_one_locale(cfg, root, args, None).await?;
        return Ok(());
    }
    let json = args.json;
    say(
        json,
        format!(
            "fuzz: across {} locale(s): {}",
            args.locales.len(),
            args.locales.join(", ")
        ),
    );
    // locale -> the set of finding signatures it produced, for the cross-locale
    // i18n diff. A signature is "<oracle>:<kind>:<message>" (stable enough to
    // tell "same finding in another locale" from "only here").
    let mut per_locale: Vec<(String, std::collections::BTreeSet<String>)> = Vec::new();
    for locale in &args.locales {
        say(json, format!("\n=== locale {locale} ==="));
        let sigs = fuzz_one_locale(cfg, root, args, Some(locale)).await?;
        per_locale.push((locale.clone(), sigs));
    }
    // Cross-locale i18n report: a finding present in some but not all locales is
    // a locale-specific finding (e.g. an overflow only under `de`).
    let specific = crate::crosscut::locale_specific_findings(&per_locale);
    if specific.is_empty() {
        say(
            json,
            "\nlocale diff: no locale-specific findings (all findings reproduce across every locale)",
        );
    } else {
        say(json, "\nlocale diff: locale-specific findings (i18n):");
        for (sig, locs) in &specific {
            say(json, format!("  [{}] only in: {}", sig, locs.join(", ")));
        }
    }
    Ok(())
}

pub struct SweepArgs {
    pub journey: String,
    pub seed: u64,
    pub budget: u32,
    pub sim: bool,
    pub json: bool,
    /// `--record`: after the crawl, record one annotated clip per boxable finding.
    pub record: bool,
    /// `--out <dir>`: where the clips land (default `.reproit/sweep-clips`).
    pub out: Option<std::path::PathBuf>,
}

/// SWEEP: the coverage finder. Where `fuzz` permutes action sequences to provoke
/// SEQUENCE-dependent bugs (crash/jank/hang), `sweep` does ONE crawl that visits
/// every reachable screen once and reports the STATE-PRESENT bugs simply visible
/// on each (overflow / content / a11y / choice-anomaly) - one finding per
/// (screen x issue), no per-seed collapse. The runner already emits these markers
/// on any walk; sweep is about COLLECTING and reporting them, not new detection.
/// Returns `true` when the coverage walk COMPLETED (the runner declared done),
/// `false` when it was cut short (timeout / killed) so its coverage is partial.
/// The caller turns `false` into a non-zero exit so CI never reads an incomplete
/// sweep as a clean pass.
pub async fn sweep(cfg: &Config, root: &Path, args: &SweepArgs) -> Result<bool> {
    let json = args.json;
    let cfg_path = root.join(".reproit/fuzz_config.json");
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    let defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    // One coverage walk: a generous budget lets the explorer reach the reachable
    // screens once. We do not permute seeds - state-present bugs are path-independent.
    let config = json!({ "seed": args.seed, "budget": args.budget });
    std::fs::write(&cfg_path, config.to_string())?;
    say(
        json,
        "sweep: one coverage walk (every reachable screen, checked once)...".to_string(),
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
    let completed = outcome.passed;

    // ALL per-state findings (every state x oracle), NOT collapsed to one-per-seed,
    // then filtered to the STATE-PRESENT oracles -- the bugs visible on a single
    // screen (overflow, content-bug, a11y, choice-anomaly, broken-route, dead-end).
    // The sequence-dependent oracles (crash, jank, hang, leak, flicker) are
    // `fuzz`'s job: a single coverage crawl can trip them flakily, so surfacing
    // them here contradicted the documented sweep contract and was the main source
    // of sweep non-determinism. They still land in the run log for `fuzz`.
    let findings: Vec<Value> = findings_for_tier(cfg, &outcome.run_dir, args.sim)
        .into_iter()
        .filter(|f| is_state_present(&crate::crosscut::classify(f)))
        .collect();
    let log = std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
    let obs = crate::map::parse_run(&log);
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
        std::collections::BTreeSet<(String, String)>,
    > = std::collections::BTreeMap::new();
    for f in &findings {
        let oracle = crate::crosscut::classify(f).as_str().to_string();
        let sig = f.get("sig").and_then(Value::as_str).unwrap_or("-");
        let route = obs
            .routes
            .get(sig)
            .cloned()
            .unwrap_or_else(|| sig.to_string());
        let detail = sweep_detail(f.get("message").and_then(Value::as_str).unwrap_or(""));
        by_screen.entry(route).or_default().insert((oracle, detail));
    }

    let issues: usize = by_screen.values().map(|s| s.len()).sum();

    // `--record`: replay each boxable finding's path and save an annotated clip.
    // Done after the report grouping so the clips can be listed alongside it.
    let clips = if args.record {
        record_sweep_clips(cfg, root, args, &findings, &obs, &cfg_path, &defines).await
    } else {
        Vec::new()
    };

    if json {
        let results: Vec<Value> = by_screen
            .iter()
            .map(|(route, items)| {
                json!({
                    "screen": route,
                    "findings": items.iter().map(|(o, d)| json!({"oracle": o, "detail": d})).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            json!({ "command": "sweep", "complete": completed, "screens_swept": swept, "screens_with_findings": by_screen.len(), "issues": issues, "results": results, "clips": clips })
        );
        return Ok(completed);
    }

    say(
        json,
        format!(
            "\nsweep: {swept} screen(s) swept; {} with {issues} distinct issue(s)",
            by_screen.len(),
        ),
    );
    for (route, items) in &by_screen {
        say(json, format!("\n  {route}"));
        for (oracle, detail) in items {
            say(json, format!("    {oracle:16} {detail}"));
        }
    }
    if !clips.is_empty() {
        say(json, format!("\n{} clip(s) recorded.", clips.len()));
    }
    // Honest about partial coverage: a cut-short crawl did NOT check every screen,
    // so don't let it read as a clean pass (the caller also exits non-zero).
    if !completed {
        say(
            json,
            "\nsweep: coverage INCOMPLETE -- the crawl was cut short (timeout/killed), \
             so some screens were not checked. Raise --budget or journeys.timeoutSec \
             to go deeper."
                .to_string(),
        );
    }
    Ok(completed)
}

/// The STATE-PRESENT oracles: bugs visible on a single screen, which is what
/// `sweep` reports. Everything else (crash/jank/hang/leak/flicker and the
/// cross-cutting visual/divergence/i18n classes) is sequence-dependent or a
/// different mode's job and belongs to `fuzz`/`soak`/`baseline`, not a one-pass
/// coverage crawl.
fn is_state_present(oracle: &crate::crosscut::Oracle) -> bool {
    use crate::crosscut::Oracle;
    matches!(
        oracle,
        Oracle::Overflow
            | Oracle::ContentBug
            | Oracle::A11y
            | Oracle::ChoiceAnomaly
            | Oracle::BrokenRoute
            | Oracle::Graph
    )
}

/// Record one annotated clip per BOXABLE sweep finding. overflow / content-bug
/// are re-detected by drawFindingBoxes on the loaded screen, so a clip = replay
/// the crawl's own action path to that screen, then the runner draws the red box
/// at the end and saves the video. a11y / dead-end / leak have no on-screen
/// element, and choice-anomaly needs the live exercise on replay (a known-flaky
/// below-fold case), so those are skipped here. Deduped by (route, oracle), each
/// taking the SHORTEST path to its screen for the cleanest clip.
async fn record_sweep_clips(
    cfg: &Config,
    root: &Path,
    args: &SweepArgs,
    findings: &[Value],
    obs: &crate::map::RunObs,
    cfg_path: &Path,
    defines: &[(String, String)],
) -> Vec<Value> {
    let json = args.json;
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| root.join(".reproit/sweep-clips"));
    let _ = std::fs::create_dir_all(&out);

    // Clips navigate by REAL URL (a faithful, hand-followable "open this URL"),
    // not by replaying drifty positional taps, so they need the app's origin.
    let Some(origin) = cfg.app.url.as_deref().and_then(url_origin) else {
        say(
            json,
            "\nsweep --record: clips need a web URL target; skipped.".to_string(),
        );
        return Vec::new();
    };
    let route_of = |sig: &str| {
        obs.routes
            .get(sig)
            .cloned()
            .unwrap_or_else(|| sig.to_string())
    };

    // One clip per (route, oracle), each with the reproduction its bug needs:
    //  - overflow / content: land on the screen by URL, re-detect + box.
    //  - broken-route: land on the SOURCE page, box the dead <a> by its href.
    //  - choice-anomaly: land on the screen, tap the outlier option so the page
    //    shifts, box the choice that did it.
    //  - hang / jank: land on the screen, replay the one triggering action, box
    //    the trigger element the runner tags at the tap.
    // Each config is gated downstream on FINDING:BOXED, so a clip that does not
    // reproduce is dropped rather than shipped with a misleading caption.
    let mut plans: std::collections::BTreeMap<(String, String), Value> =
        std::collections::BTreeMap::new();
    for f in findings {
        let oracle = crate::crosscut::classify(f).as_str().to_string();
        let sig = f.get("sig").and_then(Value::as_str).unwrap_or("");
        let route = route_of(sig);
        let goto = format!("{origin}{route}");
        let config = match oracle.as_str() {
            // a11y is a MISSING label -- there's no element to box, so a clip
            // never reproduces (the FINDING:BOXED trust gate drops it); recording
            // one only wasted a run. Matches the docs, which say a11y is skipped.
            "overflow" | "content-bug" => {
                json!({ "replay": [], "highlight": oracle, "gotoUrl": goto })
            }
            "broken-route" => {
                let Some(src) = obs
                    .edges
                    .iter()
                    .find_map(|(from, _a, to)| (route_of(to) == route).then(|| route_of(from)))
                else {
                    continue;
                };
                json!({ "replay": [], "highlight": oracle, "gotoUrl": format!("{origin}{src}"), "linkHref": route })
            }
            "choice-anomaly" => {
                // The runner re-runs the choice differential on the loaded screen
                // and boxes the outlier (a single tap can't reproduce it), so the
                // clip just needs to land there with the choice highlight.
                json!({ "replay": [], "highlight": "no-choice-anomaly", "gotoUrl": goto })
            }
            "hang" => {
                let Some(action) = obs
                    .hangs
                    .keys()
                    .find(|k| k.0.as_str() == sig)
                    .map(|k| k.1.clone())
                else {
                    continue;
                };
                json!({ "replay": [action], "highlight": oracle, "gotoUrl": goto })
            }
            "jank" => {
                let Some(action) = obs
                    .janks
                    .keys()
                    .find(|k| k.0.as_str() == sig)
                    .map(|k| k.1.clone())
                else {
                    continue;
                };
                json!({ "replay": [action], "highlight": oracle, "gotoUrl": goto })
            }
            _ => continue, // crash (no edge trigger), dead-end, leak: no clip
        };
        plans.entry((route, oracle)).or_insert(config);
    }

    if plans.is_empty() {
        say(
            json,
            "\nsweep --record: no boxable findings to clip on this run.".to_string(),
        );
        return Vec::new();
    }
    say(
        json,
        format!(
            "\nsweep --record: recording up to {} clip(s) to {}...",
            plans.len(),
            out.display()
        ),
    );
    let mut clips = Vec::new();
    for ((route, oracle), config) in &plans {
        if std::fs::write(cfg_path, config.to_string()).is_err() {
            continue;
        }
        let label = format!("{}__{oracle}", sanitize_route(route));
        say(json, format!("  {label}..."));
        let outcome = match run_explorer(
            cfg,
            root,
            &args.journey,
            true,
            defines,
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
        // TRUST GATE: only keep a clip whose box actually drew (the finding
        // re-detected on this load). A clip that did not reproduce is dropped
        // rather than shipped with a misleading caption.
        match boxed_drew(&outcome.run_dir) {
            Some(true) => {}
            Some(false) => {
                say(
                    json,
                    format!("    {label}: did not reproduce on load, skipped"),
                );
                continue;
            }
            None => {
                // No FINDING:BOXED marker at all: the web runner is older than the
                // binary and does not support the clip protocol (it also ignores
                // the per-clip URL), so every clip would be wrong. Fail loudly with
                // a fix rather than silently dropping all of them.
                say(
                    json,
                    "\nsweep --record: the web runner is out of date and cannot \
                     record clips for this version.\n  Refresh it: delete the cached \
                     runner (re-downloaded on next run), or set REPROIT_WEB_RUNNER_DIR \
                     to a matching runner."
                        .to_string(),
                );
                return clips;
            }
        }
        let Some(src) = newest_webm(&outcome.run_dir) else {
            say(json, format!("    no video produced for {label}"));
            continue;
        };
        let dest = out.join(format!("{label}.webm"));
        if std::fs::copy(&src, &dest).is_ok() {
            say(json, format!("    saved {}", dest.display()));
            clips.push(json!({
                "screen": route,
                "oracle": oracle,
                "clip": dest.to_string_lossy(),
            }));
        }
    }
    clips
}

/// The scheme://authority origin of a URL (e.g. "https://app.com:8080"), for
/// building a per-clip navigation URL by joining it with a finding's route path.
fn url_origin(u: &str) -> Option<String> {
    let (scheme, rest) = u.split_once("://")?;
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

/// Whether a clip run's box drew, from the LAST `FINDING:BOXED` marker in its
/// drive log. `Some(true)` drew, `Some(false)` did not reproduce (drop the clip),
/// `None` means NO marker at all -- the runner is too old to support the clip
/// protocol, which the caller surfaces as an actionable error rather than a
/// silent drop (the old runner also ignores the per-clip URL).
fn boxed_drew(run_dir: &Path) -> Option<bool> {
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

/// A filesystem-safe clip label from a route ("/docs/en/home" -> "docs-en-home").
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

/// Print the "also saw N state-present issue(s)" footer pointing at `sweep`.
/// `fuzz` bundles every violation into one per-seed finding and headlines the
/// crash, so the overflow/content/a11y/choice/broken-route issues it walked past
/// are otherwise invisible. This surfaces their counts and routes the user to the
/// command built to report + clip them. No-op when none were seen.
fn state_present_footer(json: bool, sp: &std::collections::BTreeMap<String, String>) {
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
            "\nnote: also saw {} state-present issue(s) on the way ({detail}) -- \
             run `reproit sweep` to list + clip them.",
            sp.len()
        ),
    );
}

/// Normalize a finding message into a short, route-stable detail: drop a leading
/// "state <sig> " (so the same issue under different state sigs collapses) and a
/// trailing explanatory parenthetical.
fn sweep_detail(msg: &str) -> String {
    let s = msg
        .strip_prefix("state ")
        .and_then(|rest| rest.split_once(' ').map(|(_sig, tail)| tail))
        .unwrap_or(msg);
    s.split(" (").next().unwrap_or(s).trim().to_string()
}

/// Like `fuzz`, but returns the union of finding signatures across every locale
/// it ran. The `--target` dispatch uses this to diff findings across targets
/// (a signature present on one target but not another is a divergence).
pub async fn fuzz_targeted(
    cfg: &Config,
    root: &Path,
    args: &FuzzArgs,
) -> Result<std::collections::BTreeSet<String>> {
    // Same scoped crash-reporter suppression as `fuzz` (native backends only),
    // covering every locale's run on this target. Restored on Drop.
    let _crash_guard = crash_guard_for(cfg);
    if args.locales.is_empty() {
        return fuzz_one_locale(cfg, root, args, None).await;
    }
    let mut all = std::collections::BTreeSet::new();
    for locale in &args.locales {
        say(args.json, format!("\n=== locale {locale} ==="));
        let sigs = fuzz_one_locale(cfg, root, args, Some(locale)).await?;
        all.extend(sigs);
    }
    Ok(all)
}

/// Run the fuzz flow for a single locale (None = app default). Returns the set
/// of finding signatures it produced, so the locale loop can diff across
/// locales. `locale` is emitted to the runner as REPROIT_LOCALE (a dart-define
/// for Flutter, an env var for other backends; the orchestrator carries the
/// define list through to whichever the backend reads).
/// `--all` accumulator: crash signature -> (human label, [(repro id, action
/// count, seed)]). One entry per unique bug.
type BugBuckets = std::collections::BTreeMap<String, (String, Vec<(String, usize, u64)>)>;

async fn fuzz_one_locale(
    cfg: &Config,
    root: &Path,
    args: &FuzzArgs,
    locale: Option<&str>,
) -> Result<std::collections::BTreeSet<String>> {
    let mut found_sigs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // State-present issues (overflow/content/a11y/choice/broken-route) seen on the
    // way, deduped by signature -> oracle, for the footer that points at `sweep`.
    let mut state_present: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // --all: crash-signature -> (human label, [(repro id, action count, seed)]).
    // Same signature = same bug; the buckets become the unique-bugs summary.
    let mut buckets: BugBuckets = BugBuckets::new();
    let cfg_path = root.join(".reproit/fuzz_config.json");
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    let mut defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    // LOCALE contract: REPROIT_LOCALE travels as a dart-define (Flutter) / env
    // var (other backends) via the orchestrator's define list. The explorers
    // honor it (owned by a separate agent); here we only emit + tag.
    if let Some(loc) = locale {
        defines.push((crate::crosscut::LOCALE_ENV.to_string(), loc.to_string()));
    }

    // Batch size: 0 means "all runs in one drive session" (the default, the
    // big win). 1 means the legacy one-drive-per-seed path. Clamp to runs.
    let batch_size = if args.batch == 0 {
        args.runs.max(1)
    } else {
        args.batch.clamp(1, args.runs.max(1))
    };
    let json = args.json;
    if batch_size > 1 {
        say(
            json,
            format!(
                "fuzz: {} seed(s) in batches of {} (startup amortized per batch)",
                args.runs, batch_size
            ),
        );
    }

    // PURE: fuzz reads the committed map/visits ONCE, then accrues coverage
    // guidance IN MEMORY across batches/seeds (it never writes the committed
    // graph, so a fixed seed replays identically across invocations; `map` is
    // what folds discoveries in). Each seed in a batch shares the snapshot as it
    // stands at the START of that batch; the in-memory snapshot updates BETWEEN
    // batches (via absorb_run_inmem below), not within. Smaller batches tighten
    // the guidance loop at the cost of more startups.
    let mut map = crate::map::load_map(root, cfg);
    // Routes the aggregate map can leave: folded into each per-seed dead-end
    // check so a sparse seed does not false-flag an escapable page as a sink.
    // Grows as seeds reveal exits the shallow map-build never reached.
    let mut escapable = map_escapable_routes(&map);
    let mut visits = crate::map::load_visits(root);
    let mut warm = false;
    let mut done = 0u32;
    // Seeds that ACTUALLY executed (one log segment each), vs `done` which counts
    // seeds DISPATCHED into a batch. A wall-clock timeout can kill a multi-seed
    // batch after only the first seed, so the summary must report seeds_run, not
    // the configured count, or it overstates how much was explored.
    let mut seeds_run = 0u32;
    while done < args.runs {
        let this_batch = batch_size.min(args.runs - done);
        let plans: Vec<SeedPlan> = (0..this_batch)
            .map(|j| {
                let seed = args.seed + (done + j) as u64;
                plan_seed(cfg, root, args, &map, &visits, seed, done + j)
            })
            .collect();

        // Write the config the explorer reads. A single-seed batch keeps the
        // exact legacy shape ({"seed":..}), preserving the byte-for-byte
        // determinism contract and the replay/shrink paths. Multi-seed batches
        // use {"batch":[...]} which the explorer runs in sequence, resetting
        // the widget tree between seeds.
        let config = if plans.len() == 1 {
            plans[0].config.clone()
        } else {
            json!({ "batch": plans.iter().map(|p| p.config.clone()).collect::<Vec<_>>() })
        };
        std::fs::write(&cfg_path, config.to_string())?;

        let outcome = run_explorer(
            cfg,
            root,
            &args.journey,
            warm,
            &defines,
            args.profile_timing,
            args.sim,
            false,
        )
        .await?;
        warm = true;
        done += this_batch;

        // Split the single drive log per seed by the SEED:BEGIN/END markers,
        // so coverage, trace, and findings are attributed to the right seed.
        let full_log =
            std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
        let segments = split_seed_segments(&full_log, &plans);
        seeds_run += segments.len() as u32;

        // Pool escapable routes across ALL seeds in this batch BEFORE judging any
        // of them. A dead end is a graph property, so one seed's sparse view is
        // too partial: an early seed that only reached a page as its budget
        // terminus would false-flag it even though a sibling seed left it cleanly.
        // Pooling (and accumulating into `escapable` across batches) means a page
        // any seed could leave via a forward action is never a dead end. This is
        // the trust fix: dead-end findings must survive the whole run's evidence.
        for (_s, seg_log) in &segments {
            let o = crate::map::parse_run(seg_log);
            for (from, action, to) in &o.edges {
                if action != "back" && to != from {
                    if let Some(r) = o.routes.get(from) {
                        escapable.insert(r.clone());
                    }
                }
            }
        }

        for (idx, (seed, seg_log)) in segments.iter().enumerate() {
            // Accrue this walk's coverage into the IN-MEMORY snapshot only, so
            // later batches in THIS run get the guidance, but the committed
            // map/visits stay untouched (fuzz is pure; re-run `map` to fold in).
            let _ = crate::map::absorb_run_inmem(&mut map, &mut visits, seg_log);
            // Findings attributed to THIS seed: exceptions parsed from the
            // seed's log slice, plus the per-device perf oracle (whole-session;
            // attributed to whichever seed it lands in only when we can't split
            // perf per seed, frame timing is session-wide, so it is attributed
            // to the run as a whole on the first seed that has the manifest).
            // The INVARIANTS oracle: evaluate the built-in + custom invariant
            // set over THIS seed's parsed state graph + exceptions (shared with
            // findings_for_tier/sweep via findings_from_log). no-exception
            // subsumes the old raw-exception oracle, so the exceptions are fed in
            // and folded back when that invariant is disabled. The pooled
            // `escapable` routes keep a dead-end flagged only when no batch's
            // evidence escapes it. Jank/leak stay handled by perf_findings below
            // for the sim tier (session-wide frame stream).
            let exceptions = exceptions_in_log(seg_log);
            let mut findings =
                findings_from_log(cfg, seg_log, exceptions, args.sim, escapable.clone());
            // Perf is session-wide (one frame stream); attribute it once. The
            // sim manifest's per-device jank is the authoritative no-jank signal;
            // headless has a fake clock so this is empty there (sim-only).
            if idx == 0 {
                findings.extend(perf_findings(&outcome.run_dir));
            }
            // ORACLE filter: tag every kept finding with its `oracle` category
            // and drop the categories `--only`/`--no` excluded. Done before the
            // empty check so an all-filtered seed is correctly reported clean.
            let dropped;
            (findings, dropped) = args.oracle_filter.apply(findings);
            if !dropped.is_empty() {
                say(
                    json,
                    format!(
                        "  seed {seed}: {} finding(s) filtered out by --only/--no",
                        dropped.len()
                    ),
                );
            }
            // LOCALE tag: stamp every kept finding with the locale it was found
            // under, and record its signature for the cross-locale i18n diff.
            if let Some(loc) = locale {
                for f in findings.iter_mut() {
                    crate::crosscut::tag_finding_locale(f, loc);
                }
            }
            for f in &findings {
                found_sigs.insert(finding_signature(f));
                // Tally the STATE-PRESENT issues this walk passed (overflow /
                // content / a11y / choice / broken-route), deduped by signature,
                // so the report can point them at `sweep` instead of burying them
                // under the per-seed crash headline.
                let oracle = crate::crosscut::classify(f).as_str();
                if matches!(
                    oracle,
                    "overflow" | "content-bug" | "a11y" | "choice-anomaly" | "broken-route"
                ) {
                    state_present.insert(finding_signature(f), oracle.to_string());
                }
            }
            if findings.is_empty() {
                say(json, format!("  seed {seed}: clean"));
                continue;
            }
            // Summarize which named invariants fired (count per invariant id).
            let mut by_inv: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for f in &findings {
                *by_inv
                    .entry(
                        f.get("invariant")
                            .and_then(Value::as_str)
                            .unwrap_or("exception"),
                    )
                    .or_default() += 1;
            }
            let summary = by_inv
                .iter()
                .map(|(k, n)| format!("{k} x{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            say(
                json,
                format!(
                    "  seed {seed}: FINDING ({} violation(s): {summary})",
                    findings.len()
                ),
            );
            let trace = trace_in_log(seg_log);
            let mut shrunk = trace.clone();
            if args.shrink && !trace.is_empty() {
                shrunk = shrink(
                    cfg,
                    root,
                    &args.journey,
                    &cfg_path,
                    &defines,
                    trace.clone(),
                    args.sim,
                    &shrink_target(&findings),
                    json,
                )
                .await?;
            }
            // The finding's content-hash id (over seed + the minimized actions,
            // exactly what `keep` later hashes), plus the two commands it teaches:
            // `check <id>` confirms it replays NOW (before you commit it to the
            // suite), `keep <id>` saves it as a guard.
            let repro_id = crate::repro::repro_id(*seed, &shrunk);
            // `--all` batches every seed into ONE drive run_dir, so writing each
            // finding's report to that shared dir would overwrite the previous
            // fuzz.md and only the last finding would be resolvable by
            // check/keep. Give each finding its OWN report dir, keyed by id and an
            // immediate child of the evidence out dir, so find_finding_by_id can
            // resolve EVERY unique bug the run reports, not just the last.
            let report_dir = if args.all {
                let d = root
                    .join(&cfg.evidence.out_dir)
                    .join(format!("finding-{repro_id}"));
                std::fs::create_dir_all(&d)?;
                d
            } else {
                outcome.run_dir.clone()
            };
            write_report(&report_dir, *seed, &findings, &trace, &shrunk)?;
            // In --all the per-seed id is intermediate: the SAME bug reached by
            // different seeds yields different ids, so teaching check/keep here
            // hands the agent several competing ids for one bug. The deduped
            // summary at the end is authoritative and teaches the commands on the
            // one canonical id; here we just note the finding. Without --all this
            // IS the single finding, so teach its commands directly.
            if args.all {
                say(
                    json,
                    format!("  found ({} action(s)) -> id {repro_id}", shrunk.len()),
                );
            } else {
                say(
                    json,
                    format!(
                        "  id {repro_id}   confirm: reproit check {repro_id}   save: reproit keep {repro_id} --as <name>"
                    ),
                );
            }
            say(
                json,
                format!("  report: {}", report_dir.join("fuzz.md").display()),
            );
            // --all: file this finding under its crash signature so the same bug
            // reached by different paths collapses to one bucket.
            if args.all {
                if let Some(primary) = primary_finding(&findings) {
                    let sig = finding_signature(primary);
                    buckets
                        .entry(sig)
                        .or_insert_with(|| (finding_label(primary), Vec::new()))
                        .1
                        .push((repro_id.clone(), shrunk.len(), *seed));
                }
            }

            // Auto-escalate: when a HEADLESS finding lands, optionally replay the
            // MINIMIZED repro ONCE on the simulator to (a) confirm it on the
            // real runtime and (b) be the run where the annotated repro video
            // gets recorded later. Gated behind --confirm-on-sim (default off),
            // so the default fuzz stays pure-headless and fast.
            // The run dir whose video the delivery pipeline records from: the
            // sim-confirm run when we have one, else the discovering run (already
            // a sim run when --sim was used directly).
            let mut deliver_dir = outcome.run_dir.clone();
            let mut confirmed = args.sim && !findings.is_empty();
            if args.confirm_on_sim && !args.sim && !shrunk.is_empty() {
                say(
                    json,
                    format!(
                        "  confirm-on-sim: replaying {} minimized action(s) on the simulator",
                        shrunk.len()
                    ),
                );
                std::fs::write(&cfg_path, json!({ "replay": shrunk }).to_string())?;
                match run_explorer(
                    cfg,
                    root,
                    &args.journey,
                    false,
                    &defines,
                    args.profile_timing,
                    true,
                    false,
                )
                .await
                {
                    Ok(o) => {
                        confirmed = !all_findings(&o.run_dir).is_empty() || !o.passed;
                        say(
                            json,
                            format!(
                                "  confirm-on-sim: {} (sim evidence: {})",
                                if confirmed {
                                    "CONFIRMED on real runtime"
                                } else {
                                    "did NOT reproduce on the simulator (headless-only finding)"
                                },
                                o.run_dir.display()
                            ),
                        );
                        // The sim run holds the .mov; copy the finding's report
                        // (with the minimized repro block) into it so the
                        // delivery pipeline reads the repro + summary from there.
                        write_report(&o.run_dir, *seed, &findings, &trace, &shrunk)?;
                        deliver_dir = o.run_dir;
                    }
                    Err(e) => say(json, format!("  confirm-on-sim: sim run failed: {e}")),
                }
            }

            // Delivery pipeline (the CodeRabbit moment): with --cloud set, record
            // + upload the annotated minimized-repro clip, then emit the PR
            // comment (dry-run unless --post-comment with a resolvable GitHub
            // repo/PR/token). Best-effort: a delivery failure never fails fuzz.
            if let (Some(cloud), Some(app)) = (&args.cloud, &args.app) {
                if let Err(e) = deliver_finding(
                    cfg,
                    root,
                    &deliver_dir,
                    cloud,
                    app,
                    args.app_idx,
                    args.post_comment,
                    confirmed,
                    json,
                )
                .await
                {
                    say(json, format!("  deliver: {e}"));
                }
            } else if args.cloud.is_some() || args.app.is_some() {
                say(
                    json,
                    "  deliver: need BOTH --cloud and --app to deliver; skipping",
                );
            }
            // Neutralize: a later `reproit run --warm` must not replay this.
            let _ = std::fs::write(&cfg_path, "{}");
            // Default: one finding per invocation (shrinking is expensive; fix it
            // before hunting more). With --all, keep going to collect every bug.
            if !args.all {
                state_present_footer(json, &state_present);
                return Ok(found_sigs);
            }
        }
    }
    // --all: report the deduped unique bugs (one bucket per crash signature).
    if args.all && !buckets.is_empty() {
        let total: usize = buckets.values().map(|(_, v)| v.len()).sum();
        say(
            json,
            format!(
                "\nunique bugs: {} (from {total} finding(s) over {seeds_run} seed(s))",
                buckets.len(),
            ),
        );
        for (_sig, (label, mut entries)) in buckets {
            // Canonical repro for the bug: the shortest (fewest actions).
            entries.sort_by_key(|(_, n, _)| *n);
            let (id, n, _) = entries[0].clone();
            let dups = entries.len().saturating_sub(1);
            let also = if dups > 0 {
                format!("  (+{dups} more path(s) reach the same bug)")
            } else {
                String::new()
            };
            say(json, format!("  {id}  {label}  [{n} action(s)]{also}"));
            say(
                json,
                format!("    confirm: reproit check {id}   keep: reproit keep {id} --as <name>"),
            );
        }
        state_present_footer(json, &state_present);
        let _ = std::fs::write(&cfg_path, "{}");
        return Ok(found_sigs);
    }
    say(
        json,
        format!(
            "\nno findings over {seeds_run} seed(s), budget {}",
            args.budget
        ),
    );
    // Neutralize: a later `reproit run --warm` must not replay fuzz state.
    let _ = std::fs::write(&cfg_path, "{}");
    Ok(found_sigs)
}

/// Build the crash-reporter suppression guard for this run from the configured
/// platform's backend. Native backends (desktop AX/UIA/AT-SPI, Appium) get a
/// real guard that suppresses the OS crash dialog for the run and restores it on
/// Drop; web/headless/in-process backends get an inert guard that touches
/// nothing. An unknown platform also yields an inert guard (no setting changed).
fn crash_guard_for(cfg: &Config) -> crate::crashreporter::CrashReporterGuard {
    match crate::platform::resolve(&cfg.app.platform) {
        Some(p) => crate::crashreporter::CrashReporterGuard::engage(p.backend),
        None => crate::crashreporter::CrashReporterGuard::engage_inert(),
    }
}

/// A stable cross-locale signature for a finding: `<oracle>:<kind>:<message>`.
/// Used to tell "the same finding showed up in another locale" from "only here"
/// so the locale loop can flag locale-specific i18n findings.
fn finding_signature(f: &Value) -> String {
    let oracle = crate::crosscut::classify(f).as_str();
    let kind = f.get("kind").and_then(Value::as_str).unwrap_or("?");
    let message = f.get("message").and_then(Value::as_str).unwrap_or("");
    // The top stack frame (the crash LOCATION) makes this a robust bug-bucket
    // key: two walks that reach the same crash by different paths share it, while
    // same-message crashes at different code locations stay distinct.
    let frame = f
        .get("frames")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .unwrap_or("");
    // Crashes bucket on the exact message + top frame (the crash LOCATION): two
    // walks that reach the same crash by different paths share it, same-message
    // crashes at different code locations stay distinct. For NON-crash oracles the
    // message carries run/locale-varying detail ("3 overflowing elements", "jank
    // 54.5%", a localized label) that must NOT split one defect into many buckets,
    // so we key on a normalized message: digit runs -> `#`, quoted labels -> `<q>`.
    if oracle == "crash" {
        format!("{oracle}:{kind}:{message}:{frame}")
    } else {
        format!("{oracle}:{kind}:{}:{frame}", normalize_message(message))
    }
}

/// Collapse run/locale-varying detail in a finding message so the same defect
/// buckets to one signature: every digit run (counts, percentages, px, decimals)
/// becomes `#`, and every quoted run (a localized label) becomes `<q>`.
fn normalize_message(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let q = c;
                for n in chars.by_ref() {
                    if n == q {
                        break;
                    }
                }
                out.push_str("<q>");
            }
            d if d.is_ascii_digit() => {
                out.push('#');
                while matches!(chars.peek(), Some(n) if n.is_ascii_digit() || *n == '.' || *n == ',')
                {
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A short human label for a bug bucket (oracle + kind + first line of the
/// message), for the `--all` unique-bugs summary.
fn finding_label(f: &Value) -> String {
    let oracle = crate::crosscut::classify(f).as_str();
    let kind = f.get("kind").and_then(Value::as_str).unwrap_or("?");
    let message = f
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    let message = if message.len() > 80 {
        format!("{}…", &message[..80])
    } else {
        message.to_string()
    };
    if message.is_empty() {
        format!("{oracle}:{kind}")
    } else {
        format!("{oracle}: {message}")
    }
}

/// Resolve one seed's walk config from the (pre-batch) map/visits snapshot.
/// Identical inputs to the legacy per-run computation, just hoisted so a batch
/// can carry several. `i` is the global run index (for the progress line).
fn plan_seed(
    cfg: &Config,
    root: &Path,
    args: &FuzzArgs,
    map: &crate::appmap::AppMap,
    visits: &crate::map::Visits,
    seed: u64,
    i: u32,
) -> SeedPlan {
    // Inverse-visit-count action scoring (Adamo et al.): weight each candidate
    // edge by 1/(1+globalVisits) using this snapshot. --uniform zeroes it.
    let edge_weights = if args.uniform {
        std::collections::BTreeMap::<String, std::collections::BTreeMap<String, u64>>::new()
    } else {
        visits.edge_weights(map)
    };

    // Power schedule (AFLFast FAST): a rare, edge-rich frontier state earns
    // more budget; a saturated one earns less.
    let mut budget = args.budget;
    let mut prefix: Option<Vec<String>> = None;
    if let Some(p) = &args.from_prefix {
        // `--from <journey>`: replay the journey to its end state, then explore
        // outward. The journey IS the path in, so it takes precedence over
        // frontier pathfinding; the seeded walk gets its full budget AFTER the
        // prefix (the runner adds prefix length to the action budget).
        say(
            args.json,
            format!(
                "fuzz seed {seed} (run {}/{}): from journey ({} action(s)) then explore, budget {budget}",
                i + 1,
                args.runs,
                p.len()
            ),
        );
        prefix = Some(p.clone());
    } else if args.frontier {
        match crate::map::frontier_path(map, visits) {
            Some((target, path)) if !path.is_empty() => {
                if !args.uniform {
                    budget = energy_budget(map, visits, &target, args.budget);
                }
                say(
                    args.json,
                    format!(
                        "fuzz seed {seed} (run {}/{}): frontier {} via {} action(s), budget {budget}",
                        i + 1,
                        args.runs,
                        target,
                        path.len()
                    ),
                );
                prefix = Some(path);
            }
            _ => say(
                args.json,
                format!(
                    "fuzz seed {seed} (run {}/{}): no frontier yet (empty map), plain walk",
                    i + 1,
                    args.runs
                ),
            ),
        }
    } else {
        say(
            args.json,
            format!("fuzz seed {seed} (run {}/{})", i + 1, args.runs),
        );
    }

    let mut config = json!({ "seed": seed, "budget": budget, "edgeWeights": edge_weights });
    if let Some(p) = prefix {
        config["prefix"] = json!(p);
    }
    // Production seed corpus: real user paths to branch outward from.
    if let Some(path) = &args.seeds_file {
        match std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        {
            Some(v) if v.is_array() => config["seeds"] = v,
            _ => eprintln!("warning: --seeds {path} not readable as a JSON array; ignoring"),
        }
    }
    let _ = (cfg, root); // reserved for future per-seed file resolution
    SeedPlan { seed, config }
}

/// Split a batched drive log into per-seed `(seed, log_slice)` pairs by the
/// `SEED:BEGIN <seed>` ... `SEED:END <seed>` boundary markers the explorer
/// emits. For a single-seed (legacy) run with no markers, the whole log is
/// returned under that one seed, so the non-batched path is unchanged.
fn split_seed_segments(log: &str, plans: &[SeedPlan]) -> Vec<(u64, String)> {
    if plans.len() == 1 {
        return vec![(plans[0].seed, log.to_string())];
    }
    let mut out: Vec<(u64, String)> = Vec::new();
    let mut current: Option<(u64, Vec<&str>)> = None;
    for line in log.lines() {
        if let Some(seed) = marker_seed(line, "SEED:BEGIN ") {
            // Flush any unterminated previous segment defensively.
            if let Some((s, buf)) = current.take() {
                out.push((s, buf.join("\n")));
            }
            current = Some((seed, Vec::new()));
            continue;
        }
        if marker_seed(line, "SEED:END ").is_some() {
            if let Some((s, buf)) = current.take() {
                out.push((s, buf.join("\n")));
            }
            continue;
        }
        if let Some((_, buf)) = current.as_mut() {
            buf.push(line);
        }
    }
    if let Some((s, buf)) = current.take() {
        out.push((s, buf.join("\n")));
    }
    // If the markers were absent (e.g. an old vendored explorer), fall back to
    // attributing the whole log to each planned seed so nothing is dropped.
    if out.is_empty() {
        return plans.iter().map(|p| (p.seed, log.to_string())).collect();
    }
    out
}

/// Split a batched drive log into one segment per `SEED:BEGIN`/`SEED:END` pair,
/// in order, WITHOUT needing the seed plans (the caller knows how many entries
/// it wrote). Used by `check` to batch a repro's N repeat-replays into a single
/// drive (one browser launch) and still read a per-replay verdict. An unmarked
/// log (legacy single run) returns the whole log as one segment, so the
/// non-batched path is unchanged.
pub(crate) fn split_log_segments(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in log.lines() {
        if marker_seed(line, "SEED:BEGIN ").is_some() {
            if let Some(buf) = current.take() {
                out.push(buf.join("\n"));
            }
            current = Some(Vec::new());
            continue;
        }
        if marker_seed(line, "SEED:END ").is_some() {
            if let Some(buf) = current.take() {
                out.push(buf.join("\n"));
            }
            continue;
        }
        if let Some(buf) = current.as_mut() {
            buf.push(line);
        }
    }
    if let Some(buf) = current.take() {
        out.push(buf.join("\n"));
    }
    if out.is_empty() {
        return vec![log.to_string()];
    }
    out
}

/// Parse `<prefix><number>` -> the seed, if the line carries the marker.
fn marker_seed(line: &str, prefix: &str) -> Option<u64> {
    let i = line.find(prefix)?;
    line[i + prefix.len()..]
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
}

/// Power schedule (AFLFast FAST): give a frontier state energy inverse to
/// its visit count and proportional to how many of its outgoing edges are
/// still unexplored, clamped to [base/2, base*4]. Rare, edge-rich states
/// get more actions per run; saturated ones get fewer.
fn energy_budget(
    map: &crate::appmap::AppMap,
    visits: &crate::map::Visits,
    target_id: &str,
    base: u32,
) -> u32 {
    let sig = map
        .states
        .get(target_id)
        .and_then(|s| s.signature.semantics_hash.clone())
        .unwrap_or_default();
    let v = visits.counts.get(&sig).copied().unwrap_or(0);
    // Outgoing edges from this state, and how many have ever been traversed.
    let out_edges: Vec<&str> = map
        .transitions
        .iter()
        .filter(|t| t.from == target_id)
        .map(|t| t.from.as_str())
        .collect();
    let known_out = out_edges.len().max(1) as f64;
    let traversed = map
        .transitions
        .iter()
        .filter(|t| t.from == target_id)
        .filter(|t| {
            let action = crate::map::action_str(&t.action);
            visits
                .edge_counts
                .get(&format!("{sig}|{action}"))
                .copied()
                .unwrap_or(0)
                > 0
        })
        .count() as f64;
    let unexplored_factor = 1.0 + (known_out - traversed) / known_out; // 1.0..2.0
    let energy = base as f64 * unexplored_factor / (1.0 + v as f64).sqrt();
    energy
        .round()
        .clamp((base / 2).max(8) as f64, (base * 4) as f64) as u32
}

#[allow(clippy::too_many_arguments)]
async fn run_explorer(
    cfg: &Config,
    root: &Path,
    journey: &str,
    warm: bool,
    defines: &[(String, String)],
    profile_timing: bool,
    sim: bool,
    record_video: bool,
) -> Result<RunOutcome> {
    let opts = orchestrator::RunOpts {
        devices: 1,
        warm,
        extra_defines: defines,
        profile_timing,
        record_video,
        ..Default::default()
    };
    // Default: the HEADLESS tier (flutter test, no simulator) for Flutter; any
    // non-Flutter backend has no headless tier and routes through the real tier.
    // --sim forces the simulator tier (flutter drive), needed for jank/runtime
    // oracles + video. `run_journey_tier` is the shared selector `check` mirrors.
    orchestrator::run_journey_tier(cfg, root, journey, &opts, sim).await
}

/// Exception records not produced by the test framework itself.
pub(crate) fn app_exceptions(run_dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(run_dir.join("exceptions.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            !v.get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("TEST FRAMEWORK")
        })
        .collect()
}

/// Perf oracle: the run's frame summary (manifest) exceeding the jank
/// threshold is a finding too. Discovered the hard way: the bug zoo's
/// jank-loop fired at 54.5% jank and the exception-only oracle shrugged.
fn perf_findings(run_dir: &Path) -> Vec<Value> {
    let Ok(manifest) = std::fs::read_to_string(run_dir.join("manifest.json")) else {
        return vec![];
    };
    let Ok(m) = serde_json::from_str::<Value>(&manifest) else {
        return vec![];
    };
    let mut out = Vec::new();
    for d in m
        .get("devices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let Some(f) = d.get("frames") else { continue };
        let jank = f.get("jank_pct").and_then(Value::as_f64).unwrap_or(0.0);
        if jank > JANK_PCT_MAX {
            out.push(serde_json::json!({
                "kind": "PERF",
                "message": format!(
                    "jank {jank:.1}% (threshold {JANK_PCT_MAX}%), p90 build {:.1}ms, worst {:.0}ms",
                    f.get("p90_build_ms").and_then(Value::as_f64).unwrap_or(0.0),
                    f.get("worst_ms").and_then(Value::as_f64).unwrap_or(0.0),
                ),
                "frames": [],
            }));
        }
    }
    out
}

fn all_findings(run_dir: &Path) -> Vec<Value> {
    let mut f = app_exceptions(run_dir);
    f.extend(perf_findings(run_dir));
    f
}

/// Build the observation bundle the INVARIANTS oracle evaluates: this seed's
/// parsed state graph (EXPLORE:STATE/EDGE), the already-parsed exception
/// findings, and the tier. Per-state jank and a non-exception leak signal are
/// sim-tier inputs we do not have per-seed in the headless log, so they are
/// left empty here (no-jank then reports nothing headless, as documented).
/// The session-wide sim jank is still surfaced by `perf_findings`.
fn invariant_observations(
    seg_log: &str,
    exceptions: Vec<Value>,
    sim: bool,
    escapable_routes: std::collections::BTreeSet<String>,
) -> crate::invariants::Observations {
    let mut obs = crate::map::parse_run(seg_log);
    obs.escapable_routes = escapable_routes;
    crate::invariants::Observations {
        obs,
        exceptions,
        jank_by_sig: std::collections::BTreeMap::new(),
        leak_signal: None,
        sim,
    }
}

/// Routes the AGGREGATE map can leave via a forward (non-back) action. Folded
/// into each per-seed dead-end evaluation so a state on an escapable page is not
/// flagged as a sink just because one sparse seed recorded no exit from it (the
/// animated single-page-app false positive).
fn map_escapable_routes(map: &crate::appmap::AppMap) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for t in &map.transitions {
        if matches!(t.action, crate::appmap::Action::Back) || t.from == t.to {
            continue;
        }
        if let Some(route) = map
            .states
            .get(&t.from)
            .and_then(|s| s.signature.route.as_ref())
        {
            out.insert(route.clone());
        }
    }
    out
}

/// The category of a finding: its named invariant id when present (e.g.
/// `no-exception`, `no-dead-end`, `all-labeled`), else its `kind`, else
/// "exception". Shrink minimizes toward the SAME category that was originally
/// discovered, so a short trace that merely trips a different invariant (an
/// empty walk trivially trips graph invariants like `no-dead-end`) does NOT
/// count as reproducing.
fn finding_category(f: &Value) -> String {
    f.get("invariant")
        .and_then(Value::as_str)
        .or_else(|| f.get("kind").and_then(Value::as_str))
        .unwrap_or("exception")
        .to_string()
}

/// The set of categories the discovering run produced. The shrink oracle
/// requires a candidate to reproduce at least one of these.
fn finding_categories(findings: &[Value]) -> std::collections::BTreeSet<String> {
    findings.iter().map(finding_category).collect()
}

/// Shrink-targeting severity of a finding category. Graph/structural invariants
/// (`no-dead-end`, `all-labeled`) are trivially tripped by short or even empty
/// traces, so they rank LOWEST: a real bug must never be minimized away in favor
/// of one. Every other category (a crash, jank, leak, a11y, ...) is a "real"
/// finding worth preserving. Unknown ids default to "real".
fn category_severity(cat: &str) -> u8 {
    match cat {
        "no-dead-end" | "all-labeled" => 0,
        _ => 1,
    }
}

/// The categories shrink must preserve: only the MOST-SEVERE among the
/// originals. When a finding bundles a real bug (a crash) with an incidental
/// graph violation (a `no-dead-end` the same walk happened to trip), this keeps
/// the crash as the target, so ddmin can't drift onto the trivially-reproducible
/// dead-end and emit a shrunk "repro" that doesn't actually re-crash. Ties (e.g.
/// two graph invariants, or two real findings) keep every category at the top
/// severity, preserving the old any-of behavior.
fn shrink_target(findings: &[Value]) -> std::collections::BTreeSet<String> {
    let cats = finding_categories(findings);
    let Some(top) = cats.iter().map(|c| category_severity(c)).max() else {
        return cats;
    };
    cats.into_iter()
        .filter(|c| category_severity(c) == top)
        .collect()
}

/// The PRIMARY finding to headline: the most-severe one, so a real bug (overflow,
/// crash, jank, content, flicker, ...) wins over an incidental graph/label
/// invariant (`all-labeled`, `no-dead-end`) that merely co-occurred on the same
/// trace - matching what `shrink_target` already minimizes toward. `findings`
/// is not severity-ordered, so `findings.first()` could headline the incidental
/// invariant, making the report + kept meta name the wrong oracle (e.g. `a11y`
/// when the run actually shrank for `no-overflow`), which in turn left a scoped
/// `check --record` clip nothing box-able to show. Stable: keeps the first
/// finding among equal-severity ties, preserving the old order otherwise.
fn primary_finding(findings: &[Value]) -> Option<&Value> {
    findings.iter().reduce(|best, f| {
        if category_severity(&finding_category(f)) > category_severity(&finding_category(best)) {
            f
        } else {
            best
        }
    })
}

/// Does this candidate's finding set reproduce the original finding? True iff
/// at least one candidate finding shares a category with the originals. An
/// empty `want` (no categorizable original, defensive) falls back to "any
/// finding reproduces" to preserve old behavior.
fn reproduces_original(candidate: &[Value], want: &std::collections::BTreeSet<String>) -> bool {
    if want.is_empty() {
        return !candidate.is_empty();
    }
    candidate
        .iter()
        .any(|f| want.contains(&finding_category(f)))
}

/// The shared crawl -> per-state-findings core, given an already-read drive log
/// plus the exceptions parsed for it. Runs the log's state graph + exceptions
/// through the INVARIANTS oracle (built-in + custom) and folds the app
/// exceptions back in when `no-exception` is disabled. This is the one place the
/// invariant evaluation lives; `findings_for_tier` (a whole run dir), the
/// per-seed fuzz loop (a session segment), and `sweep` all funnel through it,
/// differing only in where the log/exceptions/escapable set come from and how
/// perf is attributed. `escapable` is the pool of routes any walk could leave
/// via a forward action, so a dead-end is only flagged when NO evidence escapes
/// it (the per-seed loop pools across batches; single-finding re-verify passes
/// an empty set).
fn findings_from_log(
    cfg: &Config,
    log: &str,
    exceptions: Vec<Value>,
    sim: bool,
    escapable: std::collections::BTreeSet<String>,
) -> Vec<Value> {
    let inv_obs = invariant_observations(log, exceptions.clone(), sim, escapable);
    let mut f = crate::invariants::evaluate(&inv_obs, &cfg.invariants);
    if !cfg.invariants.no_exception {
        f.extend(exceptions);
    }
    f
}

/// Findings for a run, by tier, run through the INVARIANTS oracle so a shrink
/// replay is judged by the SAME named invariants that discovered the finding
/// (a graph/label/exception invariant must reproduce, not just exceptions).
/// The simulator tier writes a structured exceptions.jsonl + a frames manifest
/// (perf), so `all_findings` supplies the exception+perf inputs; the HEADLESS
/// tier (flutter test) parses exceptions from the drive log. Per-state jank is
/// sim-only, surfaced separately via perf_findings.
fn findings_for_tier(cfg: &Config, run_dir: &Path, sim: bool) -> Vec<Value> {
    let log = std::fs::read_to_string(run_dir.join("drive-a.log")).unwrap_or_default();
    let exceptions = if sim {
        app_exceptions(run_dir)
    } else {
        exceptions_in_log(&log)
    };
    // The check path re-verifies a specific recorded finding without the
    // aggregate map in scope; an empty set keeps its dead-end check unchanged.
    let mut f = findings_from_log(
        cfg,
        &log,
        exceptions,
        sim,
        std::collections::BTreeSet::new(),
    );
    if sim {
        f.extend(perf_findings(run_dir));
    }
    f
}

/// The performed action sequence, from FUZZ:ACT lines in a log slice.
fn trace_in_log(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|l| {
            l.find("FUZZ:ACT ")
                .map(|i| l[i + "FUZZ:ACT ".len()..].trim().to_string())
        })
        .collect()
}

/// App exception findings parsed directly from a drive-log SLICE (one seed's
/// segment of a batched session). Mirrors `app_exceptions` but works on the
/// per-seed text so findings are attributed to the right seed. Captures each
/// "EXCEPTION CAUGHT BY ..." block (excluding the test framework's own) up to
/// the closing ═ rule, pulling kind / message / Dart source frames.
fn exceptions_in_log(log: &str) -> Vec<Value> {
    let clean = |l: &str| l.trim_start_matches("flutter: ").trim().to_string();
    let mut out = Vec::new();
    let mut buf: Option<Vec<String>> = None;
    for raw in log.lines() {
        if raw.contains("EXCEPTION CAUGHT BY") {
            // Flush an unterminated previous block defensively.
            if let Some(b) = buf.take() {
                if let Some(rec) = exception_record(&b) {
                    out.push(rec);
                }
            }
            buf = Some(vec![raw.to_string()]);
            continue;
        }
        if let Some(b) = buf.as_mut() {
            let trimmed = clean(raw);
            let is_close = !trimmed.is_empty() && trimmed.chars().all(|c| c == '═');
            if is_close || b.len() > 300 {
                if let Some(rec) = exception_record(b) {
                    out.push(rec);
                }
                buf = None;
            } else {
                b.push(raw.to_string());
            }
        }
    }
    if let Some(b) = buf {
        if let Some(rec) = exception_record(&b) {
            out.push(rec);
        }
    }
    out
}

/// Turn one captured exception block into a finding Value, or None if it is the
/// test framework's own exception (not an app bug).
fn exception_record(buf: &[String]) -> Option<Value> {
    let clean = |l: &String| l.trim_start_matches("flutter: ").trim().to_string();
    let kind = buf
        .first()
        .and_then(|l| {
            let l = clean(l);
            let start = l.find('╡')? + '╡'.len_utf8();
            let end = l.find('╞')?;
            Some(l[start..end].trim().to_string())
        })
        .unwrap_or_else(|| "EXCEPTION".to_string());
    if kind.contains("TEST FRAMEWORK") {
        return None;
    }
    let mut message = String::new();
    if let Some(start) = buf
        .iter()
        .position(|l| clean(l).starts_with("The following"))
    {
        for l in &buf[start + 1..] {
            let l = clean(l);
            if l.is_empty() {
                break;
            }
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(&l);
        }
    }
    let frames: Vec<String> = buf
        .iter()
        .map(clean)
        .filter(|l| l.contains(".dart") && (l.contains("package:") || l.contains("file://")))
        .take(12)
        .collect();
    Some(json!({ "kind": kind, "message": message, "frames": frames }))
}

/// ddmin (Zeller & Hildebrand 2002): minimize a failing trace by removing
/// CHUNKS at decreasing granularity rather than one action at a time. Each
/// replay is an expensive device run, so we want the 1-minimal trace in
/// O(log n) replays, not O(n). Granularity starts at 2 (remove halves) and
/// doubles only when no chunk at the current granularity can be dropped.
#[allow(clippy::too_many_arguments)]
async fn shrink(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    trace: Vec<String>,
    sim: bool,
    want: &std::collections::BTreeSet<String>,
    json: bool,
) -> Result<Vec<String>> {
    say(
        json,
        format!(
            "  ddmin shrinking from {} actions (cap {MAX_SHRINK_REPLAYS} replays), \
             oracle: reproduce [{}]",
            trace.len(),
            want.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
    );
    // ZERO-ACTION test: a "broken on arrival" finding (an overflow / content bug /
    // a11y issue already present at load) needs NO action to reproduce. ddmin
    // floors at one action and never tries the empty replay, so without this it
    // keeps a meaningless leftover tap - often one that MISSES on replay - which
    // makes the repro and its recorded clip nonsensical (the HUD shows a phantom
    // action while the box sits on a load-state element). Test load-only FIRST: if
    // the SAME finding category fires with zero actions, that IS the minimal repro.
    // The reproduces_original category gate rejects an empty replay that merely
    // trips an incidental graph invariant (no-dead-end / all-labeled), which is the
    // case the in-loop `candidate.is_empty() -> false` guard was worried about.
    std::fs::write(
        cfg_path,
        json!({ "replay": Vec::<String>::new() }).to_string(),
    )?;
    let load_only_reproduces =
        match run_explorer(cfg, root, journey, true, defines, false, sim, false).await {
            Ok(o) => reproduces_original(&findings_for_tier(cfg, &o.run_dir, sim), want),
            Err(_) => false,
        };
    if load_only_reproduces {
        say(
            json,
            "    -[0..0): reproduces on load, repro is empty (0 actions)",
        );
        return Ok(Vec::new());
    }

    let mut current = trace;
    let mut granularity = 2usize;
    let mut replays = 1usize; // the zero-action probe above counts as one replay
    while current.len() >= 2 && replays < MAX_SHRINK_REPLAYS {
        let chunk = current.len().div_ceil(granularity);
        let mut removed_any = false;
        // Try removing each chunk (the "complement" subsets of ddmin).
        let mut start = 0;
        while start < current.len() && replays < MAX_SHRINK_REPLAYS {
            let end = (start + chunk).min(current.len());
            let candidate: Vec<String> = current[..start]
                .iter()
                .chain(current[end..].iter())
                .cloned()
                .collect();
            replays += 1;
            let reproduces = if candidate.is_empty() {
                false
            } else {
                std::fs::write(cfg_path, json!({ "replay": candidate }).to_string())?;
                // Shrink replays run on the SAME tier as the discovering run
                // (headless replays are deterministic with the sim path). A
                // candidate reproduces ONLY if it trips the SAME finding
                // category as the original. Without this, a short/empty replay
                // trivially fires graph invariants (no-dead-end, all-labeled)
                // and shrink would minimize toward a non-reproducing trace.
                match run_explorer(cfg, root, journey, true, defines, false, sim, false).await {
                    Ok(o) => reproduces_original(&findings_for_tier(cfg, &o.run_dir, sim), want),
                    Err(_) => false,
                }
            };
            if reproduces {
                say(
                    json,
                    format!(
                        "    -[{start}..{end}): still reproduces ({} actions)",
                        candidate.len()
                    ),
                );
                current = candidate;
                removed_any = true;
                granularity = granularity.max(2); // reset toward fine
                break;
            }
            start += chunk;
        }
        if !removed_any {
            if granularity >= current.len() {
                break; // 1-minimal at this point
            }
            granularity = (granularity * 2).min(current.len());
        }
    }
    say(
        json,
        format!("  shrunk to {} actions in {replays} replays", current.len()),
    );
    // Truncate a CRASH repro at the action that fires the exception. Everything
    // after the crash is unnecessary to reproduce it, and a repro that ENDS at
    // its trigger keeps trigger_index == len == the crash point, so a guard-style
    // fix (one that stops the crash) replays cleanly UP TO that point and is
    // judged Green/PASS. Without this, the trailing post-crash actions, which the
    // fix often makes unreachable, look like a pre-trigger miss and the fixed
    // repro is misclassified STALE. One replay of the minimized trace locates the
    // crash; the truncated trace still reproduces (the crash fires at its end).
    if want.iter().any(|c| is_crash_category(c)) && current.len() >= 2 {
        std::fs::write(cfg_path, json!({ "replay": current }).to_string())?;
        if let Ok(o) = run_explorer(cfg, root, journey, true, defines, false, sim, false).await {
            let log = std::fs::read_to_string(o.run_dir.join("drive-a.log")).unwrap_or_default();
            if let Some(n0) = crash_trigger_index(&log) {
                // Back the cut off any TRAILING fragile actions to the last KEYED
                // tap at/before the crash. A `pageerror` is async, so the logged
                // crash position can land a step past the action that caused it
                // (often an unkeyed error-overlay button); ending a repro on a
                // positional `role:...#idx` (or `back`) makes it misclassify STALE
                // after a fix, because that index shifts. A keyed action survives.
                let mut n = n0.min(current.len());
                while n >= 1 && !is_keyed_action(&current[n - 1]) {
                    n -= 1;
                }
                if (1..current.len()).contains(&n) {
                    // Re-verify the keyed-truncated trace still reproduces from
                    // cold before adopting it; keep the longer trace otherwise.
                    let candidate: Vec<String> = current[..n].to_vec();
                    std::fs::write(cfg_path, json!({ "replay": candidate }).to_string())?;
                    let still =
                        match run_explorer(cfg, root, journey, true, defines, false, sim, false)
                            .await
                        {
                            Ok(o2) => {
                                reproduces_original(&findings_for_tier(cfg, &o2.run_dir, sim), want)
                            }
                            Err(_) => false,
                        };
                    if still {
                        current = candidate;
                        say(
                            json,
                            format!("  truncated to {n} actions at the crash (keyed)"),
                        );
                    }
                }
            }
        }
    }
    Ok(current)
}

/// True for the crash/exception finding category (the invariant id or kind a
/// thrown app exception is recorded under).
fn is_crash_category(cat: &str) -> bool {
    cat == "no-exception" || cat == "exception"
}

/// Whether an action targets a stable DEVELOPER KEY (`tap:key:...` /
/// `type:key:...`) rather than a positional `role:...#idx` selector or a `back`
/// navigation. Keyed actions survive layout changes; positional ones shift.
fn is_keyed_action(action: &str) -> bool {
    let sel = action
        .strip_prefix("tap:")
        .or_else(|| action.strip_prefix("type:"))
        .unwrap_or(action);
    sel.starts_with("key:")
}

/// The 1-based action count at which a replay log first fired an app exception:
/// the number of `FUZZ:ACT` lines up to and including the one that produced the
/// `EXCEPTION CAUGHT BY` block. None if the log has no exception (e.g. a graph
/// finding, which is not truncated).
fn crash_trigger_index(log: &str) -> Option<usize> {
    let mut acts = 0usize;
    for line in log.lines() {
        if line.contains("FUZZ:ACT ") {
            acts += 1;
        }
        if line.contains("EXCEPTION CAUGHT BY") {
            return Some(acts.max(1));
        }
    }
    None
}

fn write_report(
    run_dir: &Path,
    seed: u64,
    findings: &[Value],
    trace: &[String],
    shrunk: &[String],
) -> Result<()> {
    let mut md = format!("# fuzz finding (seed {seed})\n\n");
    // Each finding now carries an `invariant` id (the named property it
    // violates), so the report leads with the invariant summary, then the
    // detail. Findings with no `invariant` (legacy/raw) fall under "exception".
    let invariant_of = |f: &Value| {
        f.get("invariant")
            .and_then(Value::as_str)
            .unwrap_or("exception")
            .to_string()
    };
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for f in findings {
        *counts.entry(invariant_of(f)).or_default() += 1;
    }
    md.push_str("## invariants violated\n\n");
    for (inv, n) in &counts {
        md.push_str(&format!("- **{inv}** ({n})\n"));
    }
    // PRIMARY finding header: a machine-readable line `keep` parses to record the
    // finding's ORACLE category, its named INVARIANT, and (for graph invariants)
    // the offending STATE SIG, so `check` can re-confirm the SAME finding by its
    // oracle rather than only looking for exceptions. The primary finding is the
    // MOST-SEVERE one (a real bug over an incidental graph/label invariant on the
    // same trace), consistent with the shrink target.
    if let Some(primary) = primary_finding(findings) {
        let oracle = crate::crosscut::classify(primary).as_str();
        let inv = invariant_of(primary);
        let sig = primary.get("sig").and_then(Value::as_str).unwrap_or("");
        md.push_str(&format!(
            "\n## oracle\n\n- oracle: `{oracle}`\n- invariant: `{inv}`\n- sig: `{sig}`\n"
        ));
    }
    md.push_str("\n## findings\n\n");
    for f in findings.iter().take(8) {
        md.push_str(&format!(
            "- `{}` **{}**: {}\n",
            invariant_of(f),
            f.get("kind").and_then(Value::as_str).unwrap_or("?"),
            f.get("message").and_then(Value::as_str).unwrap_or("")
        ));
        for frame in f
            .get("frames")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .take(2)
        {
            md.push_str(&format!("  - `{}`\n", frame.as_str().unwrap_or("")));
        }
    }
    md.push_str(&format!(
        "\n## repro ({} actions{})\n\n```\n{}\n```\n\nReplay: write {{\"replay\": [...]}} to .reproit/fuzz_config.json and `reproit check explore` (then `reproit record <id>` for an annotated video).\n",
        shrunk.len(),
        if shrunk.len() < trace.len() {
            format!(", shrunk from {}", trace.len())
        } else {
            String::new()
        },
        shrunk.join("\n")
    ));
    std::fs::write(run_dir.join("fuzz.md"), md).context("writing fuzz report")
}

/// Run the find -> PR delivery pipeline for one finding: annotate + upload the
/// minimized-repro clip to the cloud, then emit the PR comment (dry-run unless
/// `post` and a GitHub repo/PR/token are resolvable). Reuses the `deliver`
/// module so `reproit publish` / `reproit comment` and the in-fuzz path share
/// one implementation.
#[allow(clippy::too_many_arguments)]
async fn deliver_finding(
    cfg: &Config,
    root: &Path,
    run_dir: &Path,
    cloud: &str,
    app: &str,
    idx: usize,
    post: bool,
    confirmed: bool,
    json: bool,
) -> Result<()> {
    let run_name = run_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned());
    say(
        json,
        format!("  deliver: publishing finding to {cloud} (app {app}, idx {idx})"),
    );
    crate::deliver::publish(
        cfg,
        root,
        app,
        idx,
        run_name.as_deref(),
        None,
        Some(cloud.to_string()),
        None,
    )
    .await?;
    // Emit the PR comment. Dry-run unless --post-comment AND the GitHub env is
    // present (we never claim to post what we can't). `confirmed` flows through
    // the run dir's exceptions/manifest the comment formatter already reads.
    let _ = confirmed;
    crate::deliver::comment(
        cfg,
        root,
        app,
        idx,
        run_name.as_deref(),
        !post, // dry_run when not explicitly posting
        None,
        None,
        None,
        Some(cloud.to_string()),
        None,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(seed: u64) -> SeedPlan {
        SeedPlan {
            seed,
            config: json!({ "seed": seed }),
        }
    }

    // The shrink reproduction oracle: a shorter candidate counts as
    // reproducing ONLY if it trips the SAME finding category that was
    // originally discovered, never just "any invariant fired".

    #[test]
    fn shrink_oracle_requires_the_original_finding_category() {
        // Discovered a crash (no-exception). The original category set:
        let want = finding_categories(&[json!({
            "kind": "EXCEPTION CAUGHT BY WIDGETS LIBRARY",
            "invariant": "no-exception",
            "message": "boom",
        })]);
        assert!(want.contains("no-exception"));

        // A crash-free shorter candidate that only trips graph invariants
        // (no-dead-end / all-labeled) must NOT count as reproducing: this is
        // the over-shrink bug, an empty/short trace trivially fires these.
        let crash_free = vec![
            json!({ "invariant": "no-dead-end", "kind": "GRAPH", "message": "sink" }),
            json!({ "invariant": "all-labeled", "kind": "SEMANTICS", "message": "unlabeled" }),
        ];
        assert!(
            !reproduces_original(&crash_free, &want),
            "a crash-free trace that only trips graph invariants must NOT reproduce a crash finding"
        );

        // A candidate that still trips the original no-exception finding DOES.
        let still_crashes = vec![json!({
            "invariant": "no-exception",
            "kind": "EXCEPTION CAUGHT BY WIDGETS LIBRARY",
            "message": "boom again",
        })];
        assert!(reproduces_original(&still_crashes, &want));

        // No findings at all: never reproduces.
        assert!(!reproduces_original(&[], &want));
    }

    #[test]
    fn shrink_target_prefers_a_crash_over_an_incidental_dead_end() {
        // The over-shrink fix: a finding that bundles a crash (no-exception)
        // with graph dead-ends the same walk tripped must shrink toward the
        // CRASH only, so ddmin can't minimize onto a trivially-reproducible
        // dead-end and emit a "repro" that never re-crashes.
        let findings = vec![
            json!({ "invariant": "no-dead-end", "kind": "GRAPH" }),
            json!({ "invariant": "no-exception", "kind": "EXCEPTION", "message": "boom" }),
            json!({ "invariant": "no-dead-end", "kind": "GRAPH" }),
        ];
        let target = shrink_target(&findings);
        assert_eq!(
            target,
            std::collections::BTreeSet::from(["no-exception".to_string()]),
            "shrink must target the crash, dropping the incidental graph invariants"
        );
        // And a dead-end-only candidate then fails the oracle against that target.
        let dead_end_only = vec![json!({ "invariant": "no-dead-end", "kind": "GRAPH" })];
        assert!(!reproduces_original(&dead_end_only, &target));
    }

    #[test]
    fn primary_finding_headlines_the_real_bug_over_incidental_a11y() {
        // The headline-oracle fix: an `all-labeled` that merely co-occurred with
        // the real overflow must NOT become the reported oracle (which would make
        // a scoped clip have nothing box-able). The most-severe finding wins.
        let findings = vec![
            json!({ "invariant": "all-labeled", "kind": "SEMANTICS" }),
            json!({ "invariant": "no-overflow", "kind": "LAYOUT" }),
        ];
        assert_eq!(
            finding_category(primary_finding(&findings).unwrap()),
            "no-overflow"
        );
    }

    #[test]
    fn primary_finding_keeps_a11y_when_it_is_the_only_finding() {
        let findings = vec![json!({ "invariant": "all-labeled", "kind": "SEMANTICS" })];
        assert_eq!(
            finding_category(primary_finding(&findings).unwrap()),
            "all-labeled"
        );
    }

    #[test]
    fn primary_finding_is_stable_among_equal_severity_reals() {
        // Two real bugs: keep the first (preserve the old order).
        let findings = vec![
            json!({ "invariant": "no-overflow", "kind": "LAYOUT" }),
            json!({ "invariant": "no-exception", "kind": "EXCEPTION", "message": "boom" }),
        ];
        assert_eq!(
            finding_category(primary_finding(&findings).unwrap()),
            "no-overflow"
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
        let a = json!({"kind":"EXCEPTION","message":"Cannot read 'id'","frames":["updateSummary (app:537)"]});
        let b = json!({"kind":"EXCEPTION","message":"Cannot read 'id'","frames":["updateSummary (app:537)","changeQty (app:469)"]});
        assert_eq!(finding_signature(&a), finding_signature(&b));
        // A different crash LOCATION is a different bug, even with the same message.
        let c = json!({"kind":"EXCEPTION","message":"Cannot read 'id'","frames":["renderCart (app:200)"]});
        assert_ne!(finding_signature(&a), finding_signature(&c));
    }

    #[test]
    fn finding_signature_normalizes_counts_and_labels_for_non_crash() {
        // Same overflow defect, message differs only in a run-varying count and a
        // localized label. Both must bucket to ONE signature, else `--all`
        // over-reports the same bug and the locale diff false-flags it as i18n.
        let a = json!({"invariant":"no-overflow","kind":"OVERFLOW","message":"3 overflowing elements near \"Sign In\""});
        let b = json!({"invariant":"no-overflow","kind":"OVERFLOW","message":"5 overflowing elements near \"Se connecter\""});
        assert_eq!(finding_signature(&a), finding_signature(&b));
        // A genuinely different overflow message still gets its own bucket.
        let c = json!({"invariant":"no-overflow","kind":"OVERFLOW","message":"clipped header overlaps nav"});
        assert_ne!(finding_signature(&a), finding_signature(&c));
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
    fn shrink_target_keeps_all_categories_when_only_graph_invariants() {
        // No real finding present: keep every graph category (the old any-of
        // behavior), since there is no more-severe bug to preserve.
        let findings = vec![
            json!({ "invariant": "no-dead-end", "kind": "GRAPH" }),
            json!({ "invariant": "all-labeled", "kind": "SEMANTICS" }),
        ];
        let target = shrink_target(&findings);
        assert_eq!(
            target,
            std::collections::BTreeSet::from([
                "no-dead-end".to_string(),
                "all-labeled".to_string()
            ])
        );
    }

    #[test]
    fn write_report_emits_machine_readable_oracle_block() {
        // The `## oracle` block is what `keep` parses to record the finding's
        // oracle category + violating sig. For a graph dead-end finding it must
        // carry oracle=graph and the offending state sig.
        let dir = std::env::temp_dir().join(format!("reproit-wr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let findings = vec![json!({
            "invariant": "no-dead-end",
            "kind": "GRAPH",
            "message": "state advanced is a dead end",
            "sig": "advanced",
            "frames": [],
        })];
        write_report(
            &dir,
            9,
            &findings,
            &["tap:Advanced".into()],
            &["tap:Advanced".into()],
        )
        .unwrap();
        let md = std::fs::read_to_string(dir.join("fuzz.md")).unwrap();
        assert!(md.contains("## oracle"), "missing oracle block:\n{md}");
        assert!(md.contains("- oracle: `graph`"), "{md}");
        assert!(md.contains("- invariant: `no-dead-end`"), "{md}");
        assert!(md.contains("- sig: `advanced`"), "{md}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shrink_oracle_categorizes_a_dead_end_finding_distinctly() {
        // A finding discovered as no-dead-end is NOT reproduced by a crash.
        let want = finding_categories(&[json!({
            "invariant": "no-dead-end", "kind": "GRAPH", "message": "sink",
        })]);
        let crash = vec![json!({
            "invariant": "no-exception", "kind": "EXCEPTION", "message": "boom",
        })];
        assert!(!reproduces_original(&crash, &want));
        let dead_end = vec![json!({
            "invariant": "no-dead-end", "kind": "GRAPH", "message": "still a sink",
        })];
        assert!(reproduces_original(&dead_end, &want));
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
        assert_eq!(trace_in_log(&segs[0].1), vec!["tap:A", "back"]);
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
        assert_eq!(trace_in_log(&segs[0].1), vec!["tap:A"]);
        assert_eq!(segs[1].0, 2);
        assert_eq!(trace_in_log(&segs[1].1), vec!["tap:B", "back"]);
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
        assert_eq!(trace_in_log(&segs[0]), vec!["tap:A"]);
        assert_eq!(trace_in_log(&segs[1]), vec!["tap:A"]);
    }

    #[test]
    fn split_log_segments_unmarked_is_whole_log() {
        // The single-replay (times == 1) path has no markers: one segment = all.
        let log = "FUZZ:ACT tap:A\nJOURNEY DONE\n";
        let segs = split_log_segments(log);
        assert_eq!(segs.len(), 1);
        assert_eq!(trace_in_log(&segs[0]), vec!["tap:A"]);
    }

    #[test]
    fn missing_markers_attributes_whole_log_to_each_planned_seed() {
        // An old vendored explorer with no SEED markers: don't drop anything.
        let log = "FUZZ:ACT tap:A\nJOURNEY DONE\n";
        let segs = split_seed_segments(log, &[plan(1), plan(2)]);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].0, 1);
        assert_eq!(segs[1].0, 2);
        assert_eq!(trace_in_log(&segs[0].1), vec!["tap:A"]);
    }

    #[test]
    fn exceptions_in_a_slice_skip_the_test_framework_block() {
        let app = "\
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: The following assertion was thrown:
flutter: A leaked AnimationController was found.
flutter:
flutter: #0 main (package:bugzoo/main.dart:210:5)
flutter: ════════════════════════════════════════════════
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
flutter: ════════════════════════════════════════════════
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
    fn boxed_drew_reads_the_last_marker() {
        let dir = std::env::temp_dir().join(format!("reproit-boxed-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("drive-a.log"),
            "FINDING:BOXED {\"oracle\":\"overflow\",\"drew\":false}\n\
             FINDING:BOXED {\"oracle\":\"overflow\",\"drew\":true}\n",
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
}
