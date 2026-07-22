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

use crate::adapters::config::Config;
use crate::adapters::orchestrator::{self, RunOutcome};
use crate::runtime::project_layout as layout;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

mod campaign;
mod confirmation;
mod findings;
mod log;
mod reporting;
mod scan;

use campaign::fuzz_one_locale;
use confirmation::{
    capture_confirmed_trace, confirm_trace, replay_is_hermetic, shrink, shrink_causal_capsule,
};
#[cfg(test)]
use confirmation::{crash_trigger_index, is_keyed_action};
#[cfg(test)]
use findings::finding_category;
pub(crate) use findings::{
    app_exceptions, finding_signature, finding_signatures_for_log, normalize_message,
};
use findings::{
    batch_completed, equivalent_findings_key, finding_label, findings_for_tier,
    findings_from_parsed, map_escapable_routes, perf_findings, primary_finding,
    reproduces_original, reserve_shrink_representative, shrink_target, target_identity,
    NormalizedEvidence,
};
#[cfg(test)]
use log::exceptions_in_log;
#[allow(unused_imports)] // Preserve the existing crate-level log-splitting façade.
pub(crate) use log::split_log_segments;
#[cfg(test)]
use log::trace_in_log;
use log::{marker_seed, split_seed_segments};
use reporting::{
    deliver_finding, persist_causal_capsule, persist_finding_report, promote_finding, write_report,
    write_run_evidence_graph, RunEvidence,
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
    pub oracle_filter: crate::domain::oracle::OracleFilter,
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
    pub evidence: crate::domain::evidence::EvidenceCounts,
    pub confirmed_findings: Vec<ConfirmedFinding>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedFinding {
    pub id: String,
    pub cause: crate::domain::capsule::CauseCategory,
    pub action_count: usize,
    pub artifact: std::path::PathBuf,
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
        merge_confirmed_findings(&mut summary.confirmed_findings, result.confirmed_findings);
        per_locale.push((locale.clone(), result.signatures));
    }
    // Cross-locale i18n report: a finding present in some but not all locales is
    // a locale-specific finding (e.g. an overflow only under `de`).
    let specific = crate::domain::locale::locale_specific_findings(&per_locale);
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
        merge_confirmed_findings(&mut all.confirmed_findings, result.confirmed_findings);
    }
    Ok(all)
}

fn merge_confirmed_findings(target: &mut Vec<ConfirmedFinding>, source: Vec<ConfirmedFinding>) {
    for finding in source {
        if target.iter().all(|existing| existing.id != finding.id) {
            target.push(finding);
        }
    }
}

/// Run the fuzz flow for a single locale (None = app default). Returns the set
/// of finding signatures it produced, so the locale loop can diff across
/// locales. `locale` is emitted to the runner as REPROIT_LOCALE (a dart-define
/// for Flutter, an env var for other backends; the orchestrator carries the
/// define list through to whichever the backend reads).
/// `--all` accumulator: crash signature -> (human label, [(repro id, action
/// count, seed)]). One entry per unique bug.
type BugBuckets = std::collections::BTreeMap<String, (String, Vec<(String, usize, u64)>)>;

/// Build the crash-reporter suppression guard for this run from the configured
/// platform's backend. Native backends (desktop AX/UIA/AT-SPI, Appium) get a
/// real guard that suppresses the OS crash dialog for the run and restores it
/// on Drop; web/headless/in-process backends get an inert guard that touches
/// nothing. An unknown platform also yields an inert guard (no setting
/// changed).
fn crash_guard_for(cfg: &Config) -> crate::adapters::crash_reporter::CrashReporterGuard {
    match crate::adapters::platform::resolve(&cfg.app.platform) {
        Some(p) => crate::adapters::crash_reporter::CrashReporterGuard::engage(p.backend),
        None => crate::adapters::crash_reporter::CrashReporterGuard::engage_inert(),
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
        contract_actions: crate::domain::contracts::action_hints(&cfg.contracts),
        seeds,
    }
}

fn batch_guidance<'a>(
    args: &FuzzArgs,
    map: &crate::domain::appmap::AppMap,
    visits: &crate::domain::map::Visits,
    static_guidance: &'a StaticGuidance,
) -> BatchGuidance<'a> {
    // Inverse-visit-count action scoring weights each candidate edge by
    // 1/(1+globalVisits) using this snapshot. --uniform zeroes it.
    let edge_weights = if args.uniform {
        std::collections::BTreeMap::<String, std::collections::BTreeMap<String, u64>>::new()
    } else {
        visits.edge_weights(map)
    };

    // A rare, edge-rich frontier state earns more budget; a saturated one earns
    // less.
    let mut budget = args.budget;
    let mut prefix = args.from_prefix.clone();
    let mut frontier = None;
    if args.from_prefix.is_none() && args.frontier {
        let graph = crate::domain::map::GraphIndex::new(map);
        match crate::domain::map::frontier_path_with_index(map, visits, &graph) {
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

/// Give a frontier state energy inverse to its visit count and proportional to
/// how many outgoing edges are still unexplored, clamped to [base/2, base*4].
fn energy_budget(
    map: &crate::domain::appmap::AppMap,
    visits: &crate::domain::map::Visits,
    graph: &crate::domain::map::GraphIndex<'_>,
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
        let action = crate::domain::map::action_str(&transition.action);
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
    run_explorer_with_exclusions(
        cfg,
        root,
        journey,
        warm,
        defines,
        &[],
        profile_timing,
        sim,
        record_video,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_explorer_with_exclusions(
    cfg: &Config,
    root: &Path,
    journey: &str,
    warm: bool,
    defines: &[(String, String)],
    excluded_defines: &[String],
    profile_timing: bool,
    sim: bool,
    record_video: bool,
) -> Result<RunOutcome> {
    let opts = orchestrator::RunOpts {
        devices: 1,
        warm,
        extra_defines: defines,
        excluded_defines,
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
mod tests;
