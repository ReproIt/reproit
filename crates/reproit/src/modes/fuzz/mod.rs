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

//! Exploration, confirmation, shrinking, and finding persistence.

use crate::backends::orchestrator::{self, RunOutcome};
use crate::config::Config;
use crate::layout;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

mod confirmation;
mod findings;
mod log;
mod reporting;
mod scan;

use confirmation::{confirm_trace, shrink, shrink_causal_capsule};
#[cfg(test)]
use confirmation::{crash_trigger_index, is_keyed_action};
#[cfg(test)]
use findings::finding_category;
use findings::{
    all_findings, batch_completed, equivalent_findings_key, finding_label, findings_for_tier,
    findings_from_parsed, map_escapable_routes, perf_findings, primary_finding,
    reproduces_original, reserve_shrink_representative, shrink_target, target_identity,
    NormalizedEvidence,
};
pub(crate) use findings::{
    app_exceptions, finding_signature, finding_signatures_for_log, normalize_message,
};
#[cfg(test)]
use log::exceptions_in_log;
#[allow(unused_imports)] // Preserve the existing crate-level log-splitting façade.
pub(crate) use log::split_log_segments;
#[cfg(test)]
use log::trace_in_log;
use log::{marker_seed, split_seed_segments};
use reporting::{
    deliver_finding, persist_causal_capsule, persist_finding_report, write_report,
    write_run_evidence_graph,
};
use scan::state_present_footer;
#[cfg(test)]
use scan::{boxed_drew, broken_route_for_finding, url_origin};
#[allow(unused_imports)] // Preserve the existing scan façade for crate callers.
pub use scan::{scan, ScanArgs, ScanSummary};

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
    /// paths (e.g. exported from SDK telemetry: [["key:Tab","key:Enter"],
    /// ...]). The fuzzer replays one per session, then branches outward
    /// from it. Bugs cluster where users actually go, and reaching a valid
    /// deep state is the costly part, so a real path gets us there for
    /// free.
    pub seeds_file: Option<String>,
    /// Seeds per drive session. 0 = all `runs` in one session (the big win:
    /// install/launch/connect amortized once). 1 = one drive per seed.
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
    /// Cloud bucket id the finding's evidence attaches to.
    pub app_bucket: Option<String>,
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
    /// Oracle include/exclude filter from `--only`/`--no`. Default is the
    /// stable, objectively replayable detector set.
    /// Kept findings are tagged with their `oracle` category.
    pub oracle_filter: crate::crosscut::OracleFilter,
    /// `fuzz --from <journey>`: a journey's resolved action sequence, replayed
    /// as the prefix for every seed so the seeded walk branches outward
    /// from the journey's end state. Resolved host-side in main.rs (secrets
    /// bound, map `goto`s expanded) so a bad journey fails before any
    /// drive. Takes precedence over `--frontier` (the journey IS the chosen
    /// path in).
    pub from_prefix: Option<Vec<String>>,
}

/// Human progress line. Under `--json`, stdout must stay a single clean JSON
/// object, so every human line is routed to stderr instead (matching how
/// `repros --json` / `map --json` keep stdout machine-clean).
pub(super) fn say(json: bool, line: impl std::fmt::Display) {
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

/// Guidance shared by every seed planned from the same pre-batch graph.
struct StaticGuidance {
    contract_actions: Vec<String>,
    seeds: Option<Value>,
}

struct BatchGuidance<'a> {
    edge_weights: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u64>>,
    contract_actions: &'a [String],
    budget: u32,
    prefix: Option<Vec<String>>,
    frontier: Option<String>,
    seeds: Option<&'a Value>,
}

/// `fuzz` entry: with no `--locale`, runs the flow once (app default). With one
/// or more locales, runs the flow once PER locale (each with REPROIT_LOCALE
/// set), tags every finding with its locale, and reports findings that appear
/// in some locale but not all (locale-specific i18n findings).
#[derive(Debug, Default)]
pub struct FuzzSummary {
    pub signatures: std::collections::BTreeSet<String>,
    pub complete: bool,
    pub seeds_run: u32,
    pub seeds_requested: u32,
    pub evidence: crate::model::evidence::EvidenceCounts,
}

pub async fn fuzz(cfg: &Config, root: &Path, args: &FuzzArgs) -> Result<FuzzSummary> {
    // Crash-reporter suppression (native backends only). A native fuzz that
    // finds N crashes would pop N OS crash dialogs; this turns the dialog off
    // for the run and RESTORES the prior setting on Drop (even on error/panic).
    // Web/headless runs get an inert guard and touch no system setting.
    let _crash_guard = crash_guard_for(cfg);
    if args.locales.is_empty() {
        return fuzz_one_locale(cfg, root, args, None).await;
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
    let mut summary = FuzzSummary {
        complete: true,
        seeds_requested: args.runs.saturating_mul(args.locales.len() as u32),
        ..Default::default()
    };
    for locale in &args.locales {
        say(json, format!("\n=== locale {locale} ==="));
        let result = fuzz_one_locale(cfg, root, args, Some(locale)).await?;
        summary.complete &= result.complete;
        summary.seeds_run = summary.seeds_run.saturating_add(result.seeds_run);
        summary.signatures.extend(result.signatures.iter().cloned());
        summary.evidence.merge(&result.evidence);
        per_locale.push((locale.clone(), result.signatures));
    }
    // Cross-locale i18n report: a finding present in some but not all locales is
    // a locale-specific finding (e.g. an overflow only under `de`).
    let specific = crate::crosscut::locale_specific_findings(&per_locale);
    if specific.is_empty() {
        say(
            json,
            "\nlocale diff: no locale-specific findings (all findings reproduce across every \
             locale)",
        );
    } else {
        say(json, "\nlocale diff: locale-specific findings (i18n):");
        for (sig, locs) in &specific {
            say(json, format!("  [{}] only in: {}", sig, locs.join(", ")));
        }
    }
    Ok(summary)
}

/// Like `fuzz`, but returns the union of finding signatures across every locale
/// it ran. The `--target` dispatch uses this to diff findings across targets
/// (a signature present on one target but not another is a divergence).
pub async fn fuzz_targeted(cfg: &Config, root: &Path, args: &FuzzArgs) -> Result<FuzzSummary> {
    // Same scoped crash-reporter suppression as `fuzz` (native backends only),
    // covering every locale's run on this target. Restored on Drop.
    let _crash_guard = crash_guard_for(cfg);
    if args.locales.is_empty() {
        return fuzz_one_locale(cfg, root, args, None).await;
    }
    let mut all = FuzzSummary {
        complete: true,
        seeds_requested: args.runs.saturating_mul(args.locales.len() as u32),
        ..Default::default()
    };
    for locale in &args.locales {
        say(args.json, format!("\n=== locale {locale} ==="));
        let result = fuzz_one_locale(cfg, root, args, Some(locale)).await?;
        all.complete &= result.complete;
        all.seeds_run = all.seeds_run.saturating_add(result.seeds_run);
        all.signatures.extend(result.signatures);
        all.evidence.merge(&result.evidence);
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
) -> Result<FuzzSummary> {
    let mut found_sigs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // State-present issues (overflow/content/choice/broken-route) seen on the
    // way, deduped by signature -> oracle, for the footer that points at `scan`.
    let mut state_present: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // --all: crash-signature -> (human label, [(repro id, action count, seed)]).
    // Same signature = same bug; the buckets become the unique-bugs summary.
    let mut buckets: BugBuckets = BugBuckets::new();
    // Equivalent findings reached by another seed reuse the representative's
    // minimized actions. This avoids paying ddmin's replay cost once per seed.
    let mut shrink_cache: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut shrink_representatives = std::collections::BTreeSet::new();
    let cfg_path = crate::layout::fuzz_config_path(root);
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
    // big win). 1 means one drive per seed. Clamp to runs.
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
    let (mut map, mut visits) = crate::model::map::load_snapshot(root, cfg)?;
    // Routes the aggregate map can leave: folded into each per-seed permission
    // trap check so a sparse seed does not false-flag an escapable page.
    // Grows as seeds reveal exits the shallow map-build never reached.
    let mut escapable = map_escapable_routes(&map);
    let mut warm = false;
    let mut done = 0u32;
    let static_guidance = static_guidance(cfg, args);
    // Seeds that ACTUALLY executed (one log segment each), vs `done` which counts
    // seeds DISPATCHED into a batch. A wall-clock timeout can kill a multi-seed
    // batch after only the first seed, so the summary must report seeds_run, not
    // the configured count, or it overstates how much was explored.
    let mut seeds_run = 0u32;
    let mut complete = true;
    let mut evidence = crate::model::evidence::EvidenceCounts::default();
    while done < args.runs {
        let this_batch = batch_size.min(args.runs - done);
        let guidance = batch_guidance(args, &map, &visits, &static_guidance);
        let plans: Vec<SeedPlan> = (0..this_batch)
            .map(|j| {
                let seed = args.seed + (done + j) as u64;
                plan_seed(args, &guidance, seed, done + j)
            })
            .collect();

        // Write the config the explorer reads. A single-seed batch uses the
        // compact {"seed":..} shape; multi-seed batches use {"batch":[...]} and
        // the explorer resets the widget tree between seeds.
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
        evidence.merge(&crate::model::evidence::EvidenceCounts::from_log(&full_log));
        let segments = split_seed_segments(&full_log, &plans);
        seeds_run += segments.len() as u32;
        complete &= batch_completed(&full_log, &plans);
        let parsed_segments: Vec<_> = segments
            .into_iter()
            .map(|(seed, log)| {
                let parsed = crate::model::runner::ParsedRun::new(
                    log,
                    &[],
                    !cfg.contracts.is_empty(),
                    cfg.backend.enabled,
                );
                (seed, parsed)
            })
            .collect();

        // Pool escapable routes across ALL seeds in this batch BEFORE judging any
        // of them. A permission trap is a graph property, so one seed's sparse view is
        // too partial: an early seed that only reached a page as its budget
        // terminus would false-flag it even though a sibling seed left it cleanly.
        // Pooling (and accumulating into `escapable` across batches) means a page
        // any seed could leave via a forward action is never a trap.
        for (_, parsed) in &parsed_segments {
            let o = &parsed.map;
            for (from, action, to) in &o.edges {
                if action != "back" && to != from {
                    if let Some(r) = o.routes.get(from) {
                        let labels: std::collections::BTreeSet<String> = o
                            .states
                            .get(from)
                            .map(|labels| labels.iter().cloned().collect())
                            .unwrap_or_default();
                        std::sync::Arc::make_mut(&mut escapable)
                            .entry(r.clone())
                            .or_default()
                            .insert(labels);
                    }
                }
            }
        }

        for (idx, (seed, parsed)) in parsed_segments.into_iter().enumerate() {
            let trace = parsed.trace.clone();
            // Accrue this walk's coverage into the IN-MEMORY snapshot only, so
            // later batches in THIS run get the guidance, but the committed
            // map/visits stay untouched (fuzz is pure; re-run `map` to fold in).
            crate::model::map::absorb_obs_inmem(&mut map, &mut visits, &parsed.map);
            // Findings attributed to THIS seed: exceptions parsed from the
            // seed's log slice, plus the per-device perf oracle (whole-session;
            // attributed to whichever seed it lands in only when we can't split
            // perf per seed, frame timing is session-wide, so it is attributed
            // to the run as a whole on the first seed that has the manifest).
            // The INVARIANTS oracle: evaluate the built-in + custom invariant
            // set over THIS seed's parsed state graph + exceptions (shared with
            // findings_for_tier/scan via findings_from_log). no-exception
            // subsumes the old raw-exception oracle, so the exceptions are fed in
            // and folded back when that invariant is disabled. The pooled
            // `escapable` routes keep a permission trap only when no batch's
            // evidence escapes it. Jank/leak stay handled by perf_findings below
            // for the sim tier (session-wide frame stream).
            let normalized_evidence = NormalizedEvidence {
                observations: &parsed.observations,
                backend_events: &parsed.backend,
                stream_defects: &parsed.defects,
            };
            let mut findings = findings_from_parsed(
                cfg,
                parsed.map,
                parsed.exceptions,
                args.sim,
                escapable.clone(),
                normalized_evidence,
            );
            let contract_evaluations = crate::model::contracts::evaluate_stream(
                &cfg.contracts,
                &parsed.observations,
                &parsed.defects,
            );
            let _ = crate::model::contracts::write_evidence(
                &outcome
                    .run_dir
                    .join(format!("contract-evidence-{seed}.json")),
                &cfg.contracts,
                &parsed.observations,
                &contract_evaluations,
                &parsed.defects,
            );
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
            // ADVISORY split: non-deterministic pixel/timing signals (e.g.
            // paint-flicker) are reported for information but NEVER counted as a
            // verdict-bearing repro, per reproit's "reproduces on any machine"
            // promise. Pull them out before the verdict is formed so they never
            // create a FINDING or a saved repro.
            let (verdict_findings, advisory): (Vec<_>, Vec<_>) =
                findings.into_iter().partition(|finding| {
                    !finding
                        .get("advisory")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                });
            findings = verdict_findings;
            let unreported_violations = findings
                .iter()
                .filter(|finding| {
                    let oracle = crate::crosscut::classify(finding).as_str();
                    !crate::model::evidence::has_explicit_status_marker(oracle)
                })
                .count();
            evidence.observe_unreported_violations(unreported_violations);
            for f in &advisory {
                say(
                    json,
                    format!(
                        "  seed {seed}: advisory (not a repro): {}",
                        f.get("message").and_then(Value::as_str).unwrap_or("")
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
                let signature = finding_signature(f);
                found_sigs.insert(signature.clone());
                // Tally the STATE-PRESENT issues this walk passed (content /
                // choice / broken-route), deduped by signature,
                // so the report can point them at `scan` instead of burying them
                // under the per-seed crash headline.
                let oracle = crate::crosscut::classify(f).as_str();
                if matches!(
                    oracle,
                    "content-bug"
                        | "detached-indicator"
                        | "choice-anomaly"
                        | "broken-route"
                        | "security"
                ) {
                    state_present.insert(signature, oracle.to_string());
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
            let mut shrunk = trace.clone();
            let want = shrink_target(&findings);
            // Confirmation is the product trust gate, not an optional polish
            // pass: replay in a clean session and require the same oracle before
            // a candidate receives a public finding id. The shrinker starts with
            // a zero-action replay, so load-state failures are confirmed too.
            if args.shrink {
                if !confirm_trace(
                    cfg,
                    root,
                    &args.journey,
                    &cfg_path,
                    &defines,
                    &trace,
                    args.sim,
                    &want,
                )
                .await?
                {
                    say(
                        json,
                        format!(
                            "  seed {seed}: candidate did NOT reproduce in a clean session; \
                             discarded"
                        ),
                    );
                    continue;
                }
                say(json, format!("  seed {seed}: CONFIRMED in a clean replay"));
                let equivalent = equivalent_findings_key(&findings);
                if args.all {
                    if !reserve_shrink_representative(&mut shrink_representatives, &findings) {
                        let representative = shrink_cache
                            .get(&equivalent)
                            .expect("reserved shrink representative must be cached");
                        shrunk = representative.clone();
                        say(
                            json,
                            "  shrink: equivalent finding already minimized; reusing \
                             representative",
                        );
                    } else {
                        shrunk = shrink(
                            cfg,
                            root,
                            &args.journey,
                            &cfg_path,
                            &defines,
                            trace.clone(),
                            args.sim,
                            &want,
                            json,
                        )
                        .await?;
                        shrink_cache.insert(equivalent, shrunk.clone());
                    }
                } else {
                    shrunk = shrink(
                        cfg,
                        root,
                        &args.journey,
                        &cfg_path,
                        &defines,
                        trace.clone(),
                        args.sim,
                        &want,
                        json,
                    )
                    .await?;
                }
            }
            // The finding's content-hash id (over seed + the minimized actions,
            // exactly what `keep` later hashes), plus the two commands it teaches:
            // `check <id>` confirms it replays NOW (before you commit it to the
            // suite), `keep <id>` saves it as a guard.
            let primary_sig = primary_finding(&findings)
                .map(finding_signature)
                .unwrap_or_else(|| "unknown".to_string());
            let repro_id =
                crate::model::repro::finding_id(&target_identity(cfg), &primary_sig, seed, &shrunk);
            let finding_id = crate::model::repro::display_finding_id(&repro_id);
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
            write_report(&report_dir, &repro_id, seed, &findings, &trace, &shrunk)?;
            write_run_evidence_graph(&report_dir, &outcome.run_dir, &trace, &findings, &shrunk)?;
            persist_finding_report(root, &repro_id, &report_dir)?;
            if let Some(guard) = crate::model::contracts::FrozenContractGuard::from_findings(
                &cfg.contracts,
                &findings,
            ) {
                guard.save(&layout::finding_dir(root, &repro_id).join("contract.json"))?;
            }
            if let Some(guard) =
                crate::model::backend::FrozenBackendGuard::from_findings(&cfg.backend, &findings)
            {
                guard.save(&layout::finding_dir(root, &repro_id).join("backend-contract.json"))?;
            }
            if let Some(primary) = primary_finding(&findings) {
                let capsule =
                    persist_causal_capsule(cfg, root, &outcome.run_dir, primary, &shrunk, seed)?;
                let capsule = shrink_causal_capsule(
                    cfg,
                    root,
                    &args.journey,
                    &cfg_path,
                    &defines,
                    &shrunk,
                    args.sim,
                    &want,
                    capsule,
                    json,
                )
                .await?;
                let capsule_id = capsule.id.clone();
                let guard = crate::capsule::Capsule::materialize_plaintext(root, &capsule_id)?;
                let mut capsule_defines = defines.clone();
                capsule_defines.push((
                    "REPROIT_CAPSULE".into(),
                    guard.path().to_string_lossy().into_owned(),
                ));
                if !confirm_trace(
                    cfg,
                    root,
                    &args.journey,
                    &cfg_path,
                    &capsule_defines,
                    &shrunk,
                    args.sim,
                    &want,
                )
                .await?
                {
                    let _ = std::fs::remove_dir_all(crate::layout::capsule_dir(root, &capsule_id));
                    let _ = std::fs::remove_dir_all(layout::finding_dir(root, &repro_id));
                    say(
                        json,
                        format!(
                            "  seed {seed}: live failure confirmed, but causal capsule did not \
                             reproduce exactly; quarantined"
                        ),
                    );
                    continue;
                }
                let finding_dir = layout::finding_dir(root, &repro_id);
                std::fs::create_dir_all(&finding_dir)?;
                std::fs::write(finding_dir.join("capsule-id"), &capsule_id)?;
                let bug_id = capsule.finding.bug_id();
                std::fs::write(
                    finding_dir.join("identity.json"),
                    serde_json::to_vec_pretty(&json!({
                        "bugId": &bug_id,
                        "identity": capsule.finding,
                    }))?,
                )?;
                say(json, format!("  capsule: {capsule_id}"));
                say(json, format!("  structural bug: {bug_id}"));
            }
            // In --all the per-seed id is intermediate: the SAME bug reached by
            // different seeds yields different ids, so teaching check/keep here
            // hands the agent several competing ids for one bug. The deduped
            // summary at the end is authoritative and teaches the commands on the
            // one canonical id; here we just note the finding. Without --all this
            // IS the single finding, so teach its commands directly.
            if args.all {
                say(
                    json,
                    format!("  found ({} action(s)) -> id {finding_id}", shrunk.len()),
                );
            } else {
                say(
                    json,
                    format!(
                        "  confirmed bug {finding_id}   reproduce: reproit {finding_id}   keep: \
                         reproit keep {finding_id} --as <name>"
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
                        .push((repro_id.clone(), shrunk.len(), seed));
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
                        write_report(&o.run_dir, &repro_id, seed, &findings, &trace, &shrunk)?;
                        write_run_evidence_graph(
                            &o.run_dir, &o.run_dir, &trace, &findings, &shrunk,
                        )?;
                        deliver_dir = o.run_dir;
                    }
                    Err(e) => say(json, format!("  confirm-on-sim: sim run failed: {e}")),
                }
            }

            // Delivery pipeline (the CodeRabbit moment): with --cloud set, record
            // + upload the annotated minimized-repro clip, then emit the PR
            // comment (dry-run unless --post-comment with a resolvable GitHub
            // repo/PR/token). Best-effort: a delivery failure never fails fuzz.
            if let (Some(cloud), Some(app), Some(bucket)) =
                (&args.cloud, &args.app, &args.app_bucket)
            {
                if let Err(e) = deliver_finding(
                    cfg,
                    root,
                    &deliver_dir,
                    cloud,
                    app,
                    bucket,
                    args.post_comment,
                    confirmed,
                    json,
                )
                .await
                {
                    say(json, format!("  deliver: {e}"));
                }
            } else if args.cloud.is_some() || args.app.is_some() || args.app_bucket.is_some() {
                say(
                    json,
                    "  deliver: need --cloud, --app, and --bucket to deliver; skipping",
                );
            }
            // Neutralize: a later warm replay must not reuse this fuzz state.
            let _ = std::fs::write(&cfg_path, "{}");
            // Default: one finding per invocation (shrinking is expensive; fix it
            // before hunting more). With --all, keep going to collect every bug.
            if !args.all {
                state_present_footer(json, &state_present);
                return Ok(FuzzSummary {
                    signatures: found_sigs,
                    complete,
                    seeds_run,
                    seeds_requested: args.runs,
                    evidence,
                });
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
            let finding_id = crate::model::repro::display_finding_id(&id);
            let dups = entries.len().saturating_sub(1);
            let also = if dups > 0 {
                format!("  (+{dups} more path(s) reach the same bug)")
            } else {
                String::new()
            };
            say(
                json,
                format!("  {finding_id}  {label}  [{n} action(s)]{also}"),
            );
            say(
                json,
                format!(
                    "    reproduce: reproit {finding_id}   keep: reproit keep {finding_id} --as \
                     <name>"
                ),
            );
        }
        state_present_footer(json, &state_present);
        let _ = std::fs::write(&cfg_path, "{}");
        if !complete || seeds_run < args.runs {
            say(
                json,
                format!(
                    "\nincomplete fuzz coverage: ran {seeds_run} of {} requested seed(s)",
                    args.runs
                ),
            );
        }
        return Ok(FuzzSummary {
            signatures: found_sigs,
            complete: complete && seeds_run == args.runs,
            seeds_run,
            seeds_requested: args.runs,
            evidence,
        });
    }
    say(
        json,
        format!(
            "\nno findings over {seeds_run} seed(s), budget {}",
            args.budget
        ),
    );
    // Neutralize: a later warm replay must not reuse fuzz state.
    let _ = std::fs::write(&cfg_path, "{}");
    if !complete || seeds_run < args.runs {
        say(
            json,
            format!(
                "incomplete fuzz coverage: ran {seeds_run} of {} requested seed(s)",
                args.runs
            ),
        );
    }
    Ok(FuzzSummary {
        signatures: found_sigs,
        complete: complete && seeds_run == args.runs,
        seeds_run,
        seeds_requested: args.runs,
        evidence,
    })
}

/// Build the crash-reporter suppression guard for this run from the configured
/// platform's backend. Native backends (desktop AX/UIA/AT-SPI, Appium) get a
/// real guard that suppresses the OS crash dialog for the run and restores it
/// on Drop; web/headless/in-process backends get an inert guard that touches
/// nothing. An unknown platform also yields an inert guard (no setting
/// changed).
fn crash_guard_for(cfg: &Config) -> crate::crashreporter::CrashReporterGuard {
    match crate::backends::platform::resolve(&cfg.app.platform) {
        Some(p) => crate::crashreporter::CrashReporterGuard::engage(p.backend),
        None => crate::crashreporter::CrashReporterGuard::engage_inert(),
    }
}

/// Resolve one seed's walk config from the (pre-batch) map/visits snapshot.
/// Resolve the same per-run inputs for each seed, hoisted so a batch can carry
/// several. `i` is the global run index (for the progress line).
fn static_guidance(cfg: &Config, args: &FuzzArgs) -> StaticGuidance {
    let seeds = args.seeds_file.as_ref().and_then(|path| {
        let parsed = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        match parsed {
            Some(value) if value.is_array() => Some(value),
            _ => {
                eprintln!("warning: --seeds {path} not readable as a JSON array; ignoring");
                None
            }
        }
    });
    StaticGuidance {
        contract_actions: crate::model::contracts::action_hints(&cfg.contracts),
        seeds,
    }
}

fn batch_guidance<'a>(
    args: &FuzzArgs,
    map: &crate::model::appmap::AppMap,
    visits: &crate::model::map::Visits,
    static_guidance: &'a StaticGuidance,
) -> BatchGuidance<'a> {
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
    let mut prefix = args.from_prefix.clone();
    let mut frontier = None;
    if args.from_prefix.is_none() && args.frontier {
        let graph = crate::model::map::GraphIndex::new(map);
        match crate::model::map::frontier_path_with_index(map, visits, &graph) {
            Some((target, path)) if !path.is_empty() => {
                if !args.uniform {
                    budget = energy_budget(map, visits, &graph, &target, args.budget);
                }
                prefix = Some(path);
                frontier = Some(target);
            }
            _ => {}
        }
    }
    BatchGuidance {
        edge_weights,
        contract_actions: &static_guidance.contract_actions,
        budget,
        prefix,
        frontier,
        seeds: static_guidance.seeds.as_ref(),
    }
}

fn plan_seed(args: &FuzzArgs, guidance: &BatchGuidance<'_>, seed: u64, i: u32) -> SeedPlan {
    if let Some(prefix) = &args.from_prefix {
        say(
            args.json,
            format!(
                "fuzz seed {seed} (run {}/{}): from journey ({} action(s)) then explore, budget \
                 {}",
                i + 1,
                args.runs,
                prefix.len(),
                guidance.budget
            ),
        );
    } else if let (Some(target), Some(path)) = (&guidance.frontier, &guidance.prefix) {
        say(
            args.json,
            format!(
                "fuzz seed {seed} (run {}/{}): frontier {target} via {} action(s), budget {}",
                i + 1,
                args.runs,
                path.len(),
                guidance.budget
            ),
        );
    } else if args.frontier {
        say(
            args.json,
            format!(
                "fuzz seed {seed} (run {}/{}): no frontier yet (empty map), plain walk",
                i + 1,
                args.runs
            ),
        );
    } else {
        say(
            args.json,
            format!("fuzz seed {seed} (run {}/{})", i + 1, args.runs),
        );
    }
    let mut config = json!({
        "seed": seed,
        "budget": guidance.budget,
        "edgeWeights": guidance.edge_weights,
        "contractActions": guidance.contract_actions,
    });
    if let Some(p) = &guidance.prefix {
        config["prefix"] = json!(p);
    }
    if let Some(seeds) = guidance.seeds {
        config["seeds"] = seeds.clone();
    }
    SeedPlan { seed, config }
}

/// Power schedule (AFLFast FAST): give a frontier state energy inverse to
/// its visit count and proportional to how many of its outgoing edges are
/// still unexplored, clamped to [base/2, base*4]. Rare, edge-rich states
/// get more actions per run; saturated ones get fewer.
fn energy_budget(
    map: &crate::model::appmap::AppMap,
    visits: &crate::model::map::Visits,
    graph: &crate::model::map::GraphIndex<'_>,
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
    let summary = graph.summary(target_id);
    debug_assert!(summary.distinct_actions <= summary.outgoing);
    let known_out = summary.distinct_actions;
    let mut traversed = 0_usize;
    for transition in graph.outgoing(target_id) {
        let action = crate::model::map::action_str(&transition.action);
        if visits
            .edge_counts
            .get(&format!("{sig}|{action}"))
            .copied()
            .unwrap_or(0)
            > 0
        {
            traversed += 1;
        }
    }
    let known_out = known_out.max(1) as f64;
    let traversed = traversed as f64;
    let unexplored_factor = 1.0 + (known_out - traversed) / known_out; // 1.0..2.0
    let energy = base as f64 * unexplored_factor / (1.0 + v as f64).sqrt();
    energy
        .round()
        .clamp((base / 2).max(8) as f64, (base * 4) as f64) as u32
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_explorer(
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

#[cfg(test)]
mod tests {
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
}
