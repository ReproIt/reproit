//! The app map as LIVE state: every exploration/fuzz run's EXPLORE records
//! merge into .reproit/map/appmap.json (states/transitions union by semantics
//! signature) and .reproit/map/visits.json (per-sig visit counts + the start
//! state). Frontier fuzzing and authoring path over this; normal commands keep
//! the model fresh, while `reproit debug map` exposes diagnostics.

use crate::adapters::config::Config;
use crate::adapters::orchestrator;
use crate::domain::appmap::AppMap;
#[cfg(test)]
use crate::domain::appmap::{
    Action, OperabilityGaps, Reversibility, State, StateSignature, Transition,
    APP_MAP_SCHEMA_VERSION,
};
use crate::runtime::project_layout as layout;
use anyhow::{Context, Result};
#[cfg(test)]
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

mod advice;
mod analysis;
mod frontier;
mod index;
mod merge;
mod parse;
mod persistence;
mod provenance;

pub(crate) use advice::{budget_advice, contract_drafts, shadow_model};
pub(crate) use analysis::GraphGuidance;
#[cfg(any(test, feature = "perf-bench"))]
pub(crate) use frontier::frontier_path;
pub(crate) use frontier::frontier_path_with_index;
#[cfg(test)]
use frontier::VISIT_WEIGHT_CAP;
#[allow(unused_imports)] // Compatibility façade; several helpers are agent-facing APIs.
pub(crate) use frontier::{edges_summary, entry_state, path_to_label, Visits};
pub(crate) use index::GraphIndex;
pub(crate) use merge::{action_str, merge};
use merge::{parse_action, sig_index};
#[allow(unused_imports)]
// Types remain reachable at their pre-split `crate::domain::map` paths.
pub(crate) use parse::{
    parse_run, parse_runner_events, AccessibilityStateCheck, EscapableRoutes, LeakMetric,
    RelationCheck, RelationViolation, RunObs,
};
#[cfg(test)]
use persistence::load_visits;
pub(crate) use persistence::{appmap_path, load_existing_map, load_map, load_snapshot};
use persistence::{load_existing_map_unlocked, load_visits_unlocked, save_snapshot, with_map_lock};
#[allow(unused_imports)] // MapProvenance is part of the existing façade contract.
pub(crate) use provenance::{map_freshness, MapFreshness, MapProvenance};

#[cfg(feature = "perf-bench")]
pub(crate) fn benchmark_save_snapshot(
    root: &Path,
    map: &AppMap,
    visits: &mut Visits,
) -> Result<()> {
    with_map_lock(root, || save_snapshot(root, map, visits))
}

#[cfg(feature = "perf-bench")]
pub(crate) fn benchmark_fingerprint(root: &Path, revision: u64) -> Result<String> {
    Ok(provenance::build_map_provenance(root, revision)?.source_fingerprint)
}

/// Merge one run's observations into an IN-MEMORY map + visits, returning the
/// parsed observations. Does no I/O, so callers that must stay pure (notably
/// `fuzz`, which reports discoveries but never mutates the committed graph) can
/// accrue cross-seed/cross-batch coverage guidance within a single invocation
/// without touching `.reproit/map/appmap.json` / `.reproit/map/visits.json`.
#[cfg(test)]
fn absorb_run_inmem(map: &mut AppMap, visits: &mut Visits, log: &str) -> RunObs {
    let obs = parse_run(log);
    absorb_obs_inmem(map, visits, &obs);
    obs
}

/// Merge observations that were already parsed by the run-analysis pipeline.
/// Keeping parsing outside this reducer prevents fuzz, findings, and graph
/// accumulation from reparsing the same marker stream.
pub(crate) fn absorb_obs_inmem(map: &mut AppMap, visits: &mut Visits, obs: &RunObs) {
    if obs.states.is_empty() {
        return;
    }
    if merge(map, obs) {
        map.mark_changed();
    }
    if visits.start.is_none() {
        visits.start = obs.start.clone();
    }
    for sig in obs.states.keys() {
        *visits.counts.entry(sig.clone()).or_insert(0) += 1;
    }
    for (from, action, _to) in &obs.edges {
        *visits
            .edge_counts
            .entry(format!("{from}|{action}"))
            .or_insert(0) += 1;
    }
}

fn unsupported_edge_summary(obs: &RunObs) -> (usize, BTreeSet<String>) {
    const MAX_REPORTED_KINDS: usize = 8;
    const MAX_KIND_LEN: usize = 32;

    let mut count = 0;
    let mut kinds = BTreeSet::new();
    for (_, action, _) in &obs.edges {
        if parse_action(action).is_some() {
            continue;
        }
        count += 1;
        if kinds.len() >= MAX_REPORTED_KINDS {
            continue;
        }
        let candidate = action
            .split_once(':')
            .map_or(action.as_str(), |(kind, _)| kind);
        let kind = if !candidate.is_empty()
            && candidate.len() <= MAX_KIND_LEN
            && candidate
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            candidate
        } else {
            "unrecognized"
        };
        kinds.insert(kind.to_string());
    }
    (count, kinds)
}

/// Merge one run's observations into both live files and persist them. This is
/// `map`'s commit path: `map` is what folds discovered coverage into the
/// committed graph. `fuzz` must NOT call this (it would make a fixed seed drift
/// across invocations as visit counts accumulate); it uses
/// [`absorb_obs_inmem`].
#[cfg(test)]
fn absorb_run(root: &Path, cfg: &Config, log: &str) -> Result<RunObs> {
    let obs = parse_run(log);
    commit_observations(root, cfg, &obs, false)?;
    Ok(obs)
}

/// Commit parsed observations. A replacement is assembled entirely in memory,
/// leaving the last good on-disk graph untouched until a usable new graph is
/// ready to commit.
fn commit_observations(root: &Path, cfg: &Config, obs: &RunObs, replace: bool) -> Result<()> {
    if obs.states.is_empty() {
        return Ok(());
    }
    with_map_lock(root, || {
        let existing = load_existing_map_unlocked(root)?;
        let mut map = if replace {
            let mut replacement = AppMap::empty(cfg.app.bundle_id.clone());
            if let Some(existing) = &existing {
                replacement.revision = existing.revision;
            }
            replacement
        } else {
            existing.unwrap_or_else(|| AppMap::empty(cfg.app.bundle_id.clone()))
        };
        let mut visits = if replace {
            Visits::default()
        } else {
            load_visits_unlocked(root, map.revision)?
        };
        absorb_obs_inmem(&mut map, &mut visits, obs);
        save_snapshot(root, &map, &mut visits)
    })
}

/// Concatenate every device's drive log in a run dir (`drive-a.log`,
/// `drive-b.log`, ...), sorted by name, so a multi-actor run's full traversal
/// feeds the map and not just device a's. A single-device run just yields
/// `drive-a.log`.
fn read_all_device_logs(run_dir: &Path) -> Result<String> {
    let mut logs: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(run_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("drive-") && name.ends_with(".log") {
                if let Ok(s) = std::fs::read_to_string(e.path()) {
                    logs.push((name, s));
                }
            }
        }
    }
    if logs.is_empty() {
        anyhow::bail!("no drive-*.log files in {}", run_dir.display());
    }
    logs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(logs
        .into_iter()
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Fold one completed crawl into the committed app map without launching a
/// second journey. Scan uses this after its own coverage walk so first-run and
/// stale-map refreshes stay single-pass. `replace` discards the old graph only
/// after this run supplied at least one usable state.
pub(crate) fn commit_run(
    root: &Path,
    cfg: &Config,
    run_dir: &Path,
    replace: bool,
    complete: bool,
) -> Result<bool> {
    if replace && !complete {
        return Ok(false);
    }
    let log = read_all_device_logs(run_dir)?;
    let obs = parse_run(&log);
    if obs.states.is_empty() {
        return Ok(false);
    }
    commit_observations(root, cfg, &obs, replace)?;
    Ok(true)
}

pub async fn build_map(
    cfg: &Config,
    root: &Path,
    journey: &str,
    budget: Option<u32>,
    label: bool,
    from_run: Option<&Path>,
    replace: bool,
) -> Result<()> {
    let run_dir = match from_run {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => root.join(p),
        None => {
            let mut extra_defines: Vec<(String, String)> = Vec::new();
            if let Some(budget) = budget {
                let cfg_path = layout::fuzz_config_path(root);
                std::fs::create_dir_all(cfg_path.parent().unwrap())?;
                std::fs::write(
                    &cfg_path,
                    serde_json::json!({ "seed": 0, "budget": budget }).to_string(),
                )?;
                extra_defines.push((
                    "REPROIT_FUZZ_CONFIG".to_string(),
                    cfg_path.to_string_lossy().into_owned(),
                ));
            }
            let outcome = orchestrator::run_journey(
                cfg,
                root,
                journey,
                &orchestrator::RunOpts {
                    devices: 1,
                    extra_defines: &extra_defines,
                    ..Default::default()
                },
            )
            .await?;
            if !outcome.passed {
                eprintln!(
                    "  note: exploration run did not pass cleanly; mapping what was observed"
                );
            }
            outcome.run_dir
        }
    };
    // Fold in EVERY device's log, not just device a: a multi-actor scenario run
    // has each actor traverse different (often deeper) screens, and a scenario
    // now emits the same EXPLORE records the crawl does, so the dual-user
    // journeys double as the mapper for screens a single actor can't reach.
    let log = read_all_device_logs(&run_dir)?;
    let obs = parse_run(&log);
    if let Some(line) = log.lines().find(|line| line.contains("EXPLORE:TRUNCATED ")) {
        let detail = line
            .split_once("EXPLORE:TRUNCATED ")
            .map(|(_, detail)| detail)
            .unwrap_or("{}");
        eprintln!(
            "  note: map reached its deterministic work limit; saved bounded partial coverage \
             ({detail})"
        );
    }
    if obs.states.is_empty() {
        // UNSCANNABLE (a WAF bot-challenge interstitial): the runner never reached
        // the app, so there are legitimately no states to map. Do NOT treat this as
        // a "missing explorer journey" error; return with an empty map so the caller
        // (scan) can surface the runner's blocked diagnostic instead.
        if log.contains("EXPLORE:UNSCANNABLE") {
            return Ok(());
        }
        anyhow::bail!(
            "no EXPLORE:STATE records in {} (is the generated explorer journey installed?)",
            run_dir.display()
        );
    }
    let (unsupported_edge_count, unsupported_edge_kinds) = unsupported_edge_summary(&obs);
    if unsupported_edge_count > 0 {
        eprintln!(
            "  warn: omitted {unsupported_edge_count} edge(s) with unsupported or malformed action \
             kinds: {}",
            unsupported_edge_kinds.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    commit_observations(root, cfg, &obs, replace)?;

    if label {
        let map = load_map(root, cfg)?;
        let state_labels: BTreeMap<String, Vec<String>> = map
            .states
            .values()
            .filter_map(|s| {
                let sig = s.signature.semantics_hash.clone()?;
                Some((sig, s.description.split(", ").map(String::from).collect()))
            })
            .collect();
        match label_states(cfg, &state_labels).await {
            Ok(names) => {
                with_map_lock(root, || {
                    let mut current = persistence::load_map_unlocked(root, cfg)?;
                    let mut visits = load_visits_unlocked(root, current.revision)?;
                    let index = sig_index(&current);
                    let mut changed = false;
                    for (sig, name) in &names {
                        if let Some(state_id) = index.get(sig) {
                            if let Some(state) = current.states.get_mut(state_id) {
                                if state.name.as_deref() != Some(name.as_str()) {
                                    state.name = Some(name.clone());
                                    changed = true;
                                }
                            }
                        }
                    }
                    if changed {
                        current.mark_changed();
                        save_snapshot(root, &current, &mut visits)?;
                    }
                    Ok(())
                })?;
            }
            Err(e) => eprintln!("  warn: labeling pass failed ({e}); keeping current names"),
        }
    }

    // The graph, visits, and provenance are committed as one recoverable
    // snapshot. The next graph-consuming command compares actual project inputs
    // to this stamp and refreshes automatically when they differ.
    let map = load_map(root, cfg)?;
    // Progress lines go to STDERR: stdout is reserved for machine output (e.g. a
    // `--json` scan/fuzz that auto-builds the map on first run), and these landing
    // on stdout corrupted the JSON object a piped consumer parses.
    eprintln!(
        "  map: {} states, {} transitions -> {}",
        map.states.len(),
        map.transitions.len(),
        appmap_path(root).display()
    );
    Ok(())
}

/// Ask the LLM to name states from their visible labels. Resilient: any
/// parse failure keeps the current names.
async fn label_states(
    cfg: &Config,
    state_labels: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, String>> {
    let provider = llm::from_spec(&cfg.llm.to_spec())?;
    let mut listing = String::new();
    for (sig, labels) in state_labels {
        listing.push_str(&format!("{sig}: {}\n", labels.join(" | ")));
    }
    let prompt = format!(
        "These are screens of a mobile app, identified by signature, with the visible semantic \
         labels observed on each. Give each a short snake_case name (login, meet_feed, profile, \
         settings, ...). Reply with ONLY a JSON object mapping signature to name, no commentary, \
         no code fences.\n\n{listing}"
    );
    let response = provider.complete(&llm::Task::new(prompt)).await?;
    let json_str = response
        .find('{')
        // Guard the slice: an LLM reply could place `}` before its first `{`, and
        // `&response[s..=e]` would panic when e < s. Require e >= s.
        .and_then(|s| {
            response
                .rfind('}')
                .filter(|&e| e >= s)
                .map(|e| &response[s..=e])
        })
        .context("no JSON object in labeling response")?;
    let parsed: BTreeMap<String, String> = serde_json::from_str(json_str)?;
    let mut used = std::collections::HashSet::new();
    let mut out = BTreeMap::new();
    for (sig, name) in parsed {
        let mut clean: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string();
        if clean.is_empty() || clean.chars().next().unwrap().is_ascii_digit() {
            clean = format!("s_{sig}");
        }
        while !used.insert(clean.clone()) {
            clean.push('_');
        }
        out.insert(sig, clean);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
