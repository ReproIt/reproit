//! The app map as LIVE state: every exploration/fuzz run's EXPLORE records
//! merge into .reproit/map/appmap.json (states/transitions union by semantics
//! signature) and .reproit/map/visits.json (per-sig visit counts + the start
//! state). Frontier fuzzing and authoring path over this; normal commands keep
//! the model fresh, while `reproit debug map` exposes diagnostics.

use crate::adapters::config::Config;
use crate::domain::appmap::AppMap;
#[cfg(test)]
use crate::domain::appmap::{
    Action, OperabilityGaps, Reversibility, State, StateSignature, Transition,
    APP_MAP_SCHEMA_VERSION,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
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
#[cfg(test)]
use frontier::{edges_summary, path_to_label};
pub(crate) use frontier::{entry_state, Visits};
pub(crate) use index::GraphIndex;
pub(crate) use merge::{action_str, merge};
use merge::{parse_action, sig_index};
#[cfg(test)]
pub(crate) use parse::RelationViolation;
pub(crate) use parse::{parse_run, parse_runner_events, EscapableRoutes, RunObs};
#[cfg(test)]
use persistence::load_visits;
pub(crate) use persistence::{appmap_path, load_existing_map, load_map, load_snapshot};
use persistence::{load_existing_map_unlocked, load_visits_unlocked, save_snapshot, with_map_lock};
pub(crate) use provenance::{map_freshness, MapFreshness};

#[cfg(feature = "perf-bench")]
pub(crate) fn benchmark_save_snapshot(
    root: &Path,
    map: &AppMap,
    visits: &mut Visits,
    now: DateTime<Utc>,
) -> Result<()> {
    with_map_lock(root, || save_snapshot(root, map, visits, now))
}

#[cfg(feature = "perf-bench")]
pub(crate) fn benchmark_fingerprint(
    root: &Path,
    revision: u64,
    now: DateTime<Utc>,
) -> Result<String> {
    Ok(provenance::build_map_provenance(root, revision, now)?.source_fingerprint)
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
fn absorb_run(root: &Path, cfg: &Config, log: &str, now: DateTime<Utc>) -> Result<RunObs> {
    let obs = parse_run(log);
    commit_observations(root, cfg, &obs, false, now)?;
    Ok(obs)
}

/// Commit parsed observations. A replacement is assembled entirely in memory,
/// leaving the last good on-disk graph untouched until a usable new graph is
/// ready to commit.
fn commit_observations(
    root: &Path,
    cfg: &Config,
    obs: &RunObs,
    replace: bool,
    now: DateTime<Utc>,
) -> Result<()> {
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
        save_snapshot(root, &map, &mut visits, now)
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

fn explicit_coverage_failure(log: &str) -> Option<String> {
    for line in log.lines() {
        let Some(detail) = line.strip_prefix("EXPLORE:COVERAGE ") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(detail) else {
            return Some("malformed EXPLORE:COVERAGE marker".to_string());
        };
        match value.get("complete").and_then(serde_json::Value::as_bool) {
            Some(true) => {}
            Some(false) => {
                let reason = value
                    .get("stopReason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("runner reported incomplete coverage");
                // A crash is a candidate observation, not a coverage success. Keep its
                // observed state so the finding pipeline can confirm and minimize it.
                if reason != "crash" {
                    return Some(reason.to_string());
                }
            }
            None => return Some("coverage marker omitted complete".to_string()),
        }
    }
    None
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
    now: DateTime<Utc>,
) -> Result<bool> {
    if replace && !complete {
        return Ok(false);
    }
    let log = read_all_device_logs(run_dir)?;
    let obs = parse_run(&log);
    if obs.states.is_empty() {
        return Ok(false);
    }
    commit_observations(root, cfg, &obs, replace, now)?;
    Ok(true)
}

pub(crate) fn commit_map_run(
    cfg: &Config,
    root: &Path,
    run_dir: &Path,
    replace: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    // Fold in EVERY device's log, not just device a: a multi-actor scenario run
    // has each actor traverse different (often deeper) screens, and a scenario
    // now emits the same EXPLORE records the crawl does, so the dual-user
    // journeys double as the mapper for screens a single actor can't reach.
    let log = read_all_device_logs(run_dir)?;
    if let Some(reason) = explicit_coverage_failure(&log) {
        anyhow::bail!("app-map exploration coverage incomplete: {reason}");
    }
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
    commit_observations(root, cfg, &obs, replace, now)?;

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

/// The visible semantic labels per state signature, the input a naming pass
/// (an LLM in the workflow layer) turns into `apply_state_labels` names.
pub(crate) fn state_semantic_labels(map: &AppMap) -> BTreeMap<String, Vec<String>> {
    map.states
        .values()
        .filter_map(|s| {
            let sig = s.signature.semantics_hash.clone()?;
            Some((sig, s.description.split(", ").map(String::from).collect()))
        })
        .collect()
}

/// Commit externally produced state names (signature -> name) into the map.
/// The names' origin is the caller's business; the domain only applies them
/// under the map lock, as one recoverable snapshot.
pub(crate) fn apply_state_labels(
    root: &Path,
    cfg: &Config,
    names: &BTreeMap<String, String>,
    now: DateTime<Utc>,
) -> Result<()> {
    with_map_lock(root, || {
        let mut current = persistence::load_map_unlocked(root, cfg)?;
        let mut visits = load_visits_unlocked(root, current.revision)?;
        let index = sig_index(&current);
        let mut changed = false;
        for (sig, name) in names {
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
            save_snapshot(root, &current, &mut visits, now)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests;
