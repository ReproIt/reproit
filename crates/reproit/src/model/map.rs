//! The app map as LIVE state: every exploration/fuzz run's EXPLORE records
//! merge into .reproit/appmap.json (states/transitions union by semantics
//! signature) and .reproit/visits.json (per-sig visit counts + the start
//! state). Frontier fuzzing and author v2 path over this; `reproit map` is
//! the explicit build/label entry point.

use crate::appmap::{
    Action, AppMap, OperabilityGap, OperabilityGaps, Reversibility, State, StateSignature,
    Transition,
};
use crate::config::Config;
use crate::orchestrator;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

/// One run's observations, keyed by semantics signature.
pub(crate) struct RunObs {
    /// sig -> (labels, unlabeled tappable count)
    pub states: BTreeMap<String, (Vec<String>, u32)>,
    /// sig -> route/page identity, when the runner reports one. Framework-neutral:
    /// any runner that puts `"route"` in its EXPLORE:STATE record (the Flutter
    /// route anchor, the web URL path, ...) gets it merged, so the candidate map
    /// can reconcile by route instead of by a name that may not line up.
    pub routes: BTreeMap<String, String>,
    /// (from sig, action string e.g. "tap:X"/"back", to sig)
    pub edges: Vec<(String, String, String)>,
    /// First state observed: the app's start state.
    pub start: Option<String>,
    /// sig -> operability/accessibility gaps, from `EXPLORE:GROUNDTRUTH` records
    /// (the graph-1-minus-graph-2 diff). Empty for runners that don't emit it.
    pub gaps: BTreeMap<String, OperabilityGaps>,
}

/// Compute a state's operability gaps from an `EXPLORE:GROUNDTRUTH` element
/// list. Each element carries `operable` (graph 1) and an `a11y` object with
/// `inTabOrder`/`keyboardActivatable`/`rolePresent`; a gap is a ground-truth-
/// operable element that fails an accessibility dimension. Pure + deterministic.
fn gaps_from_groundtruth(json: &Value) -> OperabilityGaps {
    let mut g = OperabilityGaps {
        focus_trap: json
            .get("focusTrap")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    };
    let Some(els) = json.get("elements").and_then(Value::as_array) else {
        return g;
    };
    for el in els {
        if !el.get("operable").and_then(Value::as_bool).unwrap_or(false) {
            continue; // not ground-truth operable -> not a gap candidate
        }
        let a = el.get("a11y");
        let get = |k: &str| a.and_then(|a| a.get(k)).and_then(Value::as_bool);
        // Default the a11y dims to "true" when unreported, so a missing field is
        // never counted as a gap (conservative: only count confirmed failures).
        let mut kinds: Vec<String> = Vec::new();
        if !get("keyboardActivatable").unwrap_or(true) {
            g.pointer_only += 1;
            kinds.push("pointer_only".into());
        }
        if !get("inTabOrder").unwrap_or(true) {
            g.keyboard_unreachable += 1;
            kinds.push("keyboard_unreachable".into());
        }
        if !get("rolePresent").unwrap_or(true) {
            g.no_role += 1;
            kinds.push("no_role".into());
        }
        // Keep the grounded per-element detail: which selector failed which
        // dimension(s), so the diff is actionable, not just a tally.
        if !kinds.is_empty() {
            let selector = el
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            g.items.push(OperabilityGap { selector, kinds });
        }
    }
    g
}

pub(crate) fn parse_run(log: &str) -> RunObs {
    let mut obs = RunObs {
        states: BTreeMap::new(),
        routes: BTreeMap::new(),
        edges: Vec::new(),
        start: None,
        gaps: BTreeMap::new(),
    };
    for line in log.lines() {
        if let Some(json) = extract(line, "EXPLORE:STATE ") {
            if let (Some(sig), Some(labels)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("labels").and_then(Value::as_array),
            ) {
                if obs.start.is_none() {
                    obs.start = Some(sig.to_string());
                }
                let unlabeled = json.get("unlabeled").and_then(Value::as_u64).unwrap_or(0) as u32;
                // Route is optional and runner-supplied; record the first
                // non-empty one seen for a signature.
                if let Some(route) = json.get("route").and_then(Value::as_str) {
                    if !route.is_empty() {
                        obs.routes
                            .entry(sig.to_string())
                            .or_insert_with(|| route.to_string());
                    }
                }
                obs.states.entry(sig.to_string()).or_insert_with(|| {
                    (
                        labels
                            .iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect(),
                        unlabeled,
                    )
                });
            }
        } else if let Some(json) = extract(line, "EXPLORE:EDGE ") {
            if let (Some(f), Some(a), Some(t)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
                json.get("to").and_then(Value::as_str),
            ) {
                obs.edges
                    .push((f.to_string(), a.to_string(), t.to_string()));
            }
        } else if let Some(json) = extract(line, "EXPLORE:GROUNDTRUTH ") {
            // The operability/accessibility graph for a state: ground-truth
            // operable elements vs their a11y/keyboard dimensions. We store the
            // computed gap counts keyed by signature (last write wins).
            if let Some(sig) = json.get("sig").and_then(Value::as_str) {
                obs.gaps
                    .insert(sig.to_string(), gaps_from_groundtruth(&json));
            }
        }
    }
    obs
}

fn extract(line: &str, marker: &str) -> Option<Value> {
    let idx = line.find(marker)?;
    serde_json::from_str(line[idx + marker.len()..].trim()).ok()
}

pub(crate) fn load_map(root: &Path, cfg: &Config) -> AppMap {
    std::fs::read_to_string(root.join(".reproit/appmap.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| AppMap {
            app: cfg.app.bundle_id.clone(),
            version: 1,
            states: BTreeMap::new(),
            transitions: Vec::new(),
            invariants: Vec::new(),
            interrupts: Vec::new(),
        })
}

fn save_map(root: &Path, map: &AppMap) -> Result<()> {
    let out = root.join(".reproit/appmap.json");
    std::fs::create_dir_all(out.parent().unwrap())?;
    std::fs::write(&out, serde_json::to_string_pretty(map)?)?;
    Ok(())
}

/// sig -> existing state id (states are keyed by id; sig lives in the
/// signature, so labeling renames never break identity).
fn sig_index(map: &AppMap) -> HashMap<String, String> {
    map.states
        .iter()
        .filter_map(|(id, s)| {
            s.signature
                .semantics_hash
                .clone()
                .map(|sig| (sig, id.clone()))
        })
        .collect()
}

/// Union this run's observations into the map (by sig).
pub(crate) fn merge(map: &mut AppMap, obs: &RunObs) {
    let mut index = sig_index(map);
    for (sig, (labels, unlabeled)) in &obs.states {
        match index.get(sig) {
            Some(id) => {
                // Known state: refresh the a11y observation (fixes show up
                // as the count dropping on the next exploration), and backfill
                // the route if a later run reported one we didn't have.
                if let Some(state) = map.states.get_mut(id) {
                    state.unlabeled_tappables = *unlabeled;
                    if let Some(g) = obs.gaps.get(sig) {
                        state.operability_gaps = g.clone();
                    }
                    if state.signature.route.is_none() {
                        if let Some(r) = obs.routes.get(sig) {
                            state.signature.route = Some(r.clone());
                        }
                    }
                }
            }
            None => {
                let id = format!("s_{sig}");
                map.states.insert(
                    id.clone(),
                    State {
                        description: labels
                            .iter()
                            .take(4)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                        signature: StateSignature {
                            screenshot_phash: None,
                            semantics_hash: Some(sig.clone()),
                            route: obs.routes.get(sig).cloned(),
                        },
                        parameters: vec![],
                        unlabeled_tappables: *unlabeled,
                        operability_gaps: obs.gaps.get(sig).cloned().unwrap_or_default(),
                    },
                );
                index.insert(sig.clone(), id);
            }
        }
    }
    let existing: std::collections::HashSet<String> = map
        .transitions
        .iter()
        .map(|t| format!("{}|{}|{}", t.from, action_str(&t.action), t.to))
        .collect();
    for (from, action, to) in &obs.edges {
        let (Some(f), Some(t)) = (index.get(from), index.get(to)) else {
            continue;
        };
        let key = format!("{f}|{action}|{t}");
        if existing.contains(&key) {
            continue;
        }
        map.transitions.push(Transition {
            from: f.clone(),
            to: t.clone(),
            action: parse_action(action),
            guards: vec![],
            reversibility: Reversibility::ProposedReversible,
            expected: None,
        });
    }
}

pub(crate) fn action_str(a: &Action) -> String {
    match a {
        Action::Tap { finder } => {
            format!("tap:{}", finder.strip_prefix("label:").unwrap_or(finder))
        }
        Action::Back => "back".to_string(),
        Action::Type { finder, .. } => format!("type:{finder}"),
        Action::Scroll { finder, .. } => format!("scroll:{finder}"),
        Action::System { event } => format!("system:{event}"),
    }
}

fn parse_action(s: &str) -> Action {
    match s.strip_prefix("tap:") {
        Some(l) => Action::Tap {
            finder: format!("label:{l}"),
        },
        None => Action::Back,
    }
}

/// The app's entry state: one with no incoming transition, else the first by
/// name. Where authoring/exploration starts.
// Graph helpers retained for the agnostic journey executor (goto pathfinding)
// and MCP/agent grounding; the journeys feature wires them back in.
#[allow(dead_code)]
pub(crate) fn entry_state(map: &AppMap) -> Option<String> {
    let has_incoming: std::collections::BTreeSet<&str> =
        map.transitions.iter().map(|t| t.to.as_str()).collect();
    map.states
        .keys()
        .find(|k| !has_incoming.contains(k.as_str()))
        .or_else(|| map.states.keys().next())
        .cloned()
}

/// Shortest action path from the entry state to the first state whose name OR
/// description matches `needle` (case-insensitive substring). BFS over
/// transitions. The authoring agent uses this to ground a generated journey in
/// the app's REAL navigation (discovered by `reproit map`) instead of
/// hallucinated taps. Returns (target_state_name, ordered action strings); the
/// path is empty when the entry state itself matches.
#[allow(dead_code)]
pub(crate) fn path_to_label(map: &AppMap, needle: &str) -> Option<(String, Vec<String>)> {
    let start = entry_state(map)?;
    let needle = needle.to_lowercase();
    let matches = |name: &str| -> bool {
        name.to_lowercase().contains(&needle)
            || map
                .states
                .get(name)
                .map(|s| s.description.to_lowercase().contains(&needle))
                .unwrap_or(false)
    };
    let mut adj: BTreeMap<&str, Vec<(String, &str)>> = BTreeMap::new();
    for t in &map.transitions {
        adj.entry(t.from.as_str())
            .or_default()
            .push((action_str(&t.action), t.to.as_str()));
    }
    let mut q = std::collections::VecDeque::new();
    let mut prev: BTreeMap<&str, (&str, String)> = BTreeMap::new();
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    q.push_back(start.as_str());
    seen.insert(start.as_str());
    let mut goal: Option<&str> = matches(&start).then_some(start.as_str());
    while goal.is_none() {
        let Some(cur) = q.pop_front() else { break };
        for (act, to) in adj.get(cur).into_iter().flatten() {
            if seen.insert(to) {
                prev.insert(to, (cur, act.clone()));
                if matches(to) {
                    goal = Some(to);
                    break;
                }
                q.push_back(to);
            }
        }
    }
    let goal = goal?;
    let mut path = Vec::new();
    let mut node = goal;
    while let Some((parent, act)) = prev.get(node) {
        path.push(act.clone());
        node = parent;
    }
    path.reverse();
    Some((goal.to_string(), path))
}

/// Compact "From --action--> To" edge list, for grounding the authoring prompt
/// in the app's real transitions.
#[allow(dead_code)]
pub(crate) fn edges_summary(map: &AppMap) -> Vec<String> {
    map.transitions
        .iter()
        .map(|t| format!("{} --{}--> {}", t.from, action_str(&t.action), t.to))
        .collect()
}

/// Visit counts keyed by sig + the start state. Rename-proof.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct Visits {
    pub start: Option<String>,
    pub counts: BTreeMap<String, u64>,
    /// Per-edge traversal counts keyed "fromSig|action" (action e.g.
    /// "tap:Beacons"/"back"). Feeds inverse-visit-count action scoring.
    #[serde(default)]
    pub edge_counts: BTreeMap<String, u64>,
}

/// Cap on the destination visit count used for edge weighting. The pick weight
/// is `1/(1+count)`, so an uncapped count lets a frequently-visited HUB action
/// (e.g. "add to cart", "open cart", actions you MUST repeat to reach deep
/// states) decay toward zero weight and the walk learns to avoid it, starving
/// the very paths that gate depth. Capping the count floors the weight at
/// `1/(1+CAP)`, preserving the inverse-visit bias (new states still strongly
/// preferred) while keeping hub actions reachable.
const VISIT_WEIGHT_CAP: u64 = 8;

impl Visits {
    /// edgeWeights[fromSig][action] = DESTINATION-state visit count (capped at
    /// [`VISIT_WEIGHT_CAP`]), for the explorer's pick (weight ~ 1/(1+count)).
    /// Weighting by where an edge LEADS (reward edges to rarely-seen states)
    /// rather than by how often the edge was traversed (which penalized the
    /// productive deep "Next" edges and fought depth, per the A/B). Unknown
    /// edges aren't listed, so the explorer treats them as count 0 = max weight
    /// = worth trying. Needs the map to resolve action targets.
    pub fn edge_weights(&self, map: &AppMap) -> BTreeMap<String, BTreeMap<String, u64>> {
        let sig_of: BTreeMap<&str, &str> = map
            .states
            .iter()
            .filter_map(|(id, s)| {
                s.signature
                    .semantics_hash
                    .as_deref()
                    .map(|sig| (id.as_str(), sig))
            })
            .collect();
        let mut out: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for t in &map.transitions {
            let (Some(&from_sig), Some(&to_sig)) =
                (sig_of.get(t.from.as_str()), sig_of.get(t.to.as_str()))
            else {
                continue;
            };
            let dest_visits = self
                .counts
                .get(to_sig)
                .copied()
                .unwrap_or(0)
                .min(VISIT_WEIGHT_CAP);
            out.entry(from_sig.to_string())
                .or_default()
                .insert(action_str(&t.action), dest_visits);
        }
        out
    }
}

pub(crate) fn load_visits(root: &Path) -> Visits {
    std::fs::read_to_string(root.join(".reproit/visits.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_visits(root: &Path, v: &Visits) -> Result<()> {
    std::fs::write(
        root.join(".reproit/visits.json"),
        serde_json::to_string_pretty(v)?,
    )?;
    Ok(())
}

/// Merge one run's observations into an IN-MEMORY map + visits, returning the
/// parsed observations. Does no I/O, so callers that must stay pure (notably
/// `fuzz`, which reports discoveries but never mutates the committed graph) can
/// accrue cross-seed/cross-batch coverage guidance within a single invocation
/// without touching `.reproit/appmap.json` / `.reproit/visits.json`.
pub(crate) fn absorb_run_inmem(map: &mut AppMap, visits: &mut Visits, log: &str) -> RunObs {
    let obs = parse_run(log);
    if obs.states.is_empty() {
        return obs;
    }
    merge(map, &obs);
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
    obs
}

/// Merge one run's observations into both live files and persist them. This is
/// `map`'s commit path: `map` is what folds discovered coverage into the
/// committed graph. `fuzz` must NOT call this (it would make a fixed seed drift
/// across invocations as visit counts accumulate); it uses [`absorb_run_inmem`].
pub(crate) fn absorb_run(root: &Path, cfg: &Config, log: &str) -> Result<RunObs> {
    let mut map = load_map(root, cfg);
    let mut visits = load_visits(root);
    let obs = absorb_run_inmem(&mut map, &mut visits, log);
    if !obs.states.is_empty() {
        save_map(root, &map)?;
        save_visits(root, &visits)?;
    }
    Ok(obs)
}

/// BFS shortest action-path from the start state to the least-visited
/// reachable state (ties: prefer deeper, to push the frontier outward).
pub(crate) fn frontier_path(map: &AppMap, visits: &Visits) -> Option<(String, Vec<String>)> {
    let index = sig_index(map);
    let start_sig = visits.start.clone()?;
    let start_id = index.get(&start_sig)?.clone();

    let mut adj: HashMap<&str, Vec<(&Transition, &str)>> = HashMap::new();
    for t in &map.transitions {
        adj.entry(t.from.as_str())
            .or_default()
            .push((t, t.to.as_str()));
    }
    let sig_of: HashMap<&str, &str> = map
        .states
        .iter()
        .filter_map(|(id, s)| {
            s.signature
                .semantics_hash
                .as_deref()
                .map(|sig| (id.as_str(), sig))
        })
        .collect();

    let mut paths: HashMap<String, Vec<String>> = HashMap::new();
    paths.insert(start_id.clone(), vec![]);
    let mut queue = VecDeque::from([start_id.clone()]);
    while let Some(id) = queue.pop_front() {
        let path = paths[&id].clone();
        for (t, to) in adj.get(id.as_str()).cloned().unwrap_or_default() {
            if paths.contains_key(to) {
                continue;
            }
            let mut p = path.clone();
            p.push(action_str(&t.action));
            paths.insert(to.to_string(), p);
            queue.push_back(to.to_string());
        }
    }

    paths
        .iter()
        .filter(|(id, _)| **id != start_id)
        .min_by_key(|(id, path)| {
            let sig = sig_of.get(id.as_str()).copied().unwrap_or("");
            let count = visits.counts.get(sig).copied().unwrap_or(0);
            (count, usize::MAX - path.len())
        })
        .map(|(id, path)| (id.clone(), path.clone()))
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

pub async fn build_map(
    cfg: &Config,
    root: &Path,
    journey: &str,
    label: bool,
    from_run: Option<&Path>,
) -> Result<()> {
    let run_dir = match from_run {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => root.join(p),
        None => {
            let outcome = orchestrator::run_journey(
                cfg,
                root,
                journey,
                &orchestrator::RunOpts {
                    devices: 1,
                    ..Default::default()
                },
            )
            .await?;
            if !outcome.passed {
                println!("  note: exploration run did not pass cleanly; mapping what was observed");
            }
            outcome.run_dir
        }
    };
    // Fold in EVERY device's log, not just device a: a multi-actor scenario run
    // has each actor traverse different (often deeper) screens, and a scenario
    // now emits the same EXPLORE records the crawl does, so the dual-user
    // journeys double as the mapper for screens a single actor can't reach.
    let log = read_all_device_logs(&run_dir)?;
    let obs = absorb_run(root, cfg, &log)?;
    if obs.states.is_empty() {
        anyhow::bail!(
            "no EXPLORE:STATE records in {} (is the explorer journey installed? see templates/explorer.dart)",
            run_dir.display()
        );
    }

    if label {
        let mut map = load_map(root, cfg);
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
                let index = sig_index(&map);
                let mut renames: Vec<(String, String)> = Vec::new();
                for (sig, name) in names {
                    if let Some(old_id) = index.get(&sig) {
                        if old_id != &name && !map.states.contains_key(&name) {
                            renames.push((old_id.clone(), name));
                        }
                    }
                }
                for (old, new) in renames {
                    if let Some(state) = map.states.remove(&old) {
                        map.states.insert(new.clone(), state);
                        for t in &mut map.transitions {
                            if t.from == old {
                                t.from = new.clone();
                            }
                            if t.to == old {
                                t.to = new.clone();
                            }
                        }
                    }
                }
                save_map(root, &map)?;
            }
            Err(e) => println!("  warn: labeling pass failed ({e}); keeping current names"),
        }
    }

    let map = load_map(root, cfg);
    println!(
        "  map: {} states, {} transitions -> {}",
        map.states.len(),
        map.transitions.len(),
        root.join(".reproit/appmap.json").display()
    );
    println!("  view: reproit map show --format html --out map.html");
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
        "These are screens of a mobile app, identified by signature, with the visible \
semantic labels observed on each. Give each a short snake_case name (login, meet_feed, \
profile, settings, ...). Reply with ONLY a JSON object mapping signature to name, no \
commentary, no code fences.\n\n{listing}"
    );
    let response = provider.complete(&llm::Task::new(prompt)).await?;
    let json_str = response
        .find('{')
        .and_then(|s| response.rfind('}').map(|e| &response[s..=e]))
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
mod tests {
    use super::*;

    fn st(desc: &str) -> State {
        State {
            description: desc.to_string(),
            signature: StateSignature {
                screenshot_phash: None,
                semantics_hash: None,
                route: None,
            },
            parameters: vec![],
            unlabeled_tappables: 0,
            operability_gaps: Default::default(),
        }
    }
    fn tap(from: &str, label: &str, to: &str) -> Transition {
        Transition {
            from: from.to_string(),
            to: to.to_string(),
            action: Action::Tap {
                finder: format!("label:{label}"),
            },
            guards: vec![],
            reversibility: Reversibility::ProposedReversible,
            expected: None,
        }
    }
    fn sample() -> AppMap {
        let mut states = BTreeMap::new();
        states.insert("Home".to_string(), st("home screen"));
        states.insert("Settings".to_string(), st("settings screen"));
        states.insert("About".to_string(), st("about / version info"));
        AppMap {
            app: "demo".to_string(),
            version: 1,
            states,
            transitions: vec![
                tap("Home", "Settings", "Settings"),
                tap("Settings", "About", "About"),
            ],
            invariants: vec![],
            interrupts: vec![],
        }
    }

    #[test]
    fn entry_is_the_state_without_incoming_edges() {
        assert_eq!(entry_state(&sample()).as_deref(), Some("Home"));
    }

    #[test]
    fn path_to_label_finds_shortest_action_sequence() {
        let m = sample();
        let (target, path) = path_to_label(&m, "about").expect("About is reachable");
        assert_eq!(target, "About");
        assert_eq!(
            path,
            vec!["tap:Settings".to_string(), "tap:About".to_string()]
        );
        // the entry state itself matching yields an empty path.
        let (t0, p0) = path_to_label(&m, "home").unwrap();
        assert_eq!(t0, "Home");
        assert!(p0.is_empty());
        // an unreachable/unknown label yields None.
        assert!(path_to_label(&m, "nonexistent-screen").is_none());
    }

    #[test]
    fn edges_summary_lists_real_transitions() {
        assert!(edges_summary(&sample())
            .iter()
            .any(|e| e == "Home --tap:Settings--> Settings"));
    }

    #[test]
    fn edge_weights_caps_the_visit_count_so_hub_actions_keep_a_floor() {
        // A hub destination visited far more than the cap must not decay the
        // edge weight toward zero: the count feeding 1/(1+count) is clamped to
        // VISIT_WEIGHT_CAP, so the walk can still reach it.
        let sig_state = |sig: &str| {
            let mut s = st("x");
            s.signature.semantics_hash = Some(sig.to_string());
            s
        };
        let mut states = BTreeMap::new();
        states.insert("A".to_string(), sig_state("sigA"));
        states.insert("B".to_string(), sig_state("sigB"));
        let map = AppMap {
            app: "demo".to_string(),
            version: 1,
            states,
            transitions: vec![tap("A", "go", "B")],
            invariants: vec![],
            interrupts: vec![],
        };
        let mut visits = Visits::default();
        visits.counts.insert("sigB".to_string(), 1000); // wildly over-visited hub
        let ew = visits.edge_weights(&map);
        let count = *ew
            .get("sigA")
            .and_then(|m| m.values().next())
            .expect("an edge from sigA");
        assert_eq!(
            count, VISIT_WEIGHT_CAP,
            "the weighting count must be capped, not the raw 1000"
        );
    }

    #[test]
    fn merge_captures_route_from_explore_state() {
        // A runner that reports a route (Flutter anchor, web URL path, ...) lands
        // it on the verified state, so the candidate map can reconcile by route.
        let log = r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":["Home"],"unlabeled":0}"#;
        let obs = parse_run(log);
        assert_eq!(obs.routes.get("abc").map(String::as_str), Some("/home"));
        let mut map = AppMap {
            app: "t".into(),
            version: 1,
            states: BTreeMap::new(),
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        };
        merge(&mut map, &obs);
        let state = map.states.values().next().expect("a merged state");
        assert_eq!(state.signature.route.as_deref(), Some("/home"));
    }

    #[test]
    fn groundtruth_marker_yields_operability_gaps() {
        // The motivating case: a control operable by pointer but not keyboard-
        // reachable and exposing no role (the finding-div in the dashboard).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"abc","labels":[],"unlabeled":0}"#,
            "\n",
            r#"EXPLORE:GROUNDTRUTH {"sig":"abc","focusTrap":false,"elements":[{"id":"role:option#0","operable":true,"a11y":{"inTabOrder":false,"keyboardActivatable":false,"rolePresent":false}},{"id":"key:id:nav","operable":true,"a11y":{"inTabOrder":true,"keyboardActivatable":true,"rolePresent":true}},{"id":"decoration","operable":false,"a11y":{"inTabOrder":false}}]}"#,
        );
        let obs = parse_run(log);
        let g = obs.gaps.get("abc").expect("gaps for abc");
        assert_eq!(
            g.pointer_only, 1,
            "one operable element not keyboard-activatable"
        );
        assert_eq!(
            g.keyboard_unreachable, 1,
            "one operable element not in tab order"
        );
        assert_eq!(g.no_role, 1, "one operable element with no role");
        assert!(!g.focus_trap);
        // The grounded per-element detail: exactly the one failing element, by
        // selector, tagged with every dimension it fails. This is what the
        // accessibility view/MCP tool serves, so it must be present, not a count.
        assert_eq!(g.items.len(), 1, "only the one failing element is recorded");
        assert_eq!(g.items[0].selector, "role:option#0");
        assert_eq!(
            g.items[0].kinds,
            vec!["pointer_only", "keyboard_unreachable", "no_role"],
            "the failing element is tagged with all three dimensions it fails"
        );
        // The non-operable decoration is never a gap; the healthy nav is not either.
        let mut map = AppMap {
            app: "t".into(),
            version: 1,
            states: BTreeMap::new(),
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        };
        merge(&mut map, &obs);
        let state = map.states.values().next().expect("a merged state");
        assert_eq!(state.operability_gaps.pointer_only, 1);
        assert_eq!(state.operability_gaps.keyboard_unreachable, 1);
    }

    #[test]
    fn appkit_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process AppKit operability agent
        // (runners/native/appkit-agent/main.swift). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line that the built+run Swift agent emits for a
        // window holding a real NSButton, a "fake button" (custom NSView with a
        // click gesture + handler and no a11y role), and a correctly-built
        // accessible custom control. The engine must score exactly one gap row
        // (the fake button), failing all three a11y dimensions.
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"3854aea0","labels":["Real Button","Accessible Custom Button"]}"#,
            "\n",
            r#"EXPLORE:GROUNDTRUTH {"focusTrap":false,"sig":"3854aea0","elements":[{"a11y":{"keyboardActivatable":true,"namePresent":true,"focusable":true,"rolePresent":true,"inTabOrder":true},"operable":true,"gestureKind":"button","id":"key:realButton"},{"operable":true,"a11y":{"focusable":false,"inTabOrder":false,"keyboardActivatable":false,"rolePresent":false,"namePresent":false},"id":"key:fakeButton","gestureKind":"button"},{"id":"key:goodCustom","operable":true,"gestureKind":"button","a11y":{"rolePresent":true,"namePresent":true,"keyboardActivatable":true,"focusable":true,"inTabOrder":true}}]}"#,
        );
        let obs = parse_run(log);
        let g = obs.gaps.get("3854aea0").expect("gaps for the agent state");
        // The fake button alone is an operable-but-inaccessible element.
        assert_eq!(g.no_role, 1, "fake button has no a11y role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button is not in the key-view loop"
        );
        assert_eq!(g.pointer_only, 1, "fake button is pointer-only (gesture)");
        assert!(!g.focus_trap);
    }

    #[test]
    fn wpf_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process WPF operability agent
        // (runners/native/wpf-agent/Program.cs). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line that the built+run agent emits on the Windows
        // VM for a window holding a real <Button> and a "fake button" (a
        // clickable <Border>/<TextBlock> with a MouseLeftButtonUp handler and no
        // Button role / no AutomationProperties). Graph 1 (visual tree + handler
        // reflection) and graph 2 (UIElementAutomationPeer) are joined by object
        // identity. The engine must score exactly one gap row (the fake button),
        // failing all three a11y dimensions; the real Button is clean.
        let log = r#"EXPLORE:GROUNDTRUTH {"sig":"7f0cd305","focusTrap":false,"elements":[{"id":"SaveButton","operable":true,"gestureKind":"button","a11y":{"rolePresent":true,"namePresent":true,"focusable":true,"inTabOrder":true,"keyboardActivatable":true}},{"id":"DeleteFakeButton","operable":true,"gestureKind":"delegated","a11y":{"rolePresent":false,"namePresent":false,"focusable":false,"inTabOrder":false,"keyboardActivatable":false}}]}"#;
        let obs = parse_run(log);
        let g = obs
            .gaps
            .get("7f0cd305")
            .expect("gaps for the wpf agent state");
        assert_eq!(g.no_role, 1, "fake button has no Button role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button is not in the tab order"
        );
        assert_eq!(
            g.pointer_only, 1,
            "fake button is pointer-only (mouse handler)"
        );
        assert!(!g.focus_trap);
    }

    #[test]
    fn qt_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process Qt operability agent
        // (runners/native/qt-agent/qt_agent.cpp). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line the built+run agent emits in a Linux
        // container (Debian, Qt 6.8.2, `QT_QPA_PLATFORM=offscreen`) for a window
        // holding a real QPushButton, a "fake button" (custom QWidget with a
        // mousePressEvent handler and no QAccessible role), and a correctly-built
        // accessible control. Graph 1 (QObject tree + wired signals / custom
        // subclass) joins graph 2 (QAccessibleInterface) by object identity. The
        // engine must score exactly one gap row (the fake button), failing all
        // three a11y dimensions; the real button is clean. The signature matches
        // the AppKit agent's (3854aea0): same three-control structural descriptor.
        let log = r#"EXPLORE:GROUNDTRUTH {"elements":[{"a11y":{"focusable":true,"inTabOrder":true,"keyboardActivatable":true,"namePresent":true,"rolePresent":true},"gestureKind":"button","id":"key:realButton","operable":true},{"a11y":{"focusable":false,"inTabOrder":false,"keyboardActivatable":false,"namePresent":false,"rolePresent":false},"gestureKind":"button","id":"key:fakeButton","operable":true},{"a11y":{"focusable":true,"inTabOrder":true,"keyboardActivatable":true,"namePresent":true,"rolePresent":true},"gestureKind":"button","id":"key:goodCustom","operable":true}],"focusTrap":false,"sig":"3854aea0"}"#;
        let obs = parse_run(log);
        let g = obs
            .gaps
            .get("3854aea0")
            .expect("gaps for the qt agent state");
        assert_eq!(g.no_role, 1, "fake button has no QAccessible role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button is not in the tab order"
        );
        assert_eq!(
            g.pointer_only, 1,
            "fake button is pointer-only (mousePressEvent)"
        );
        assert!(!g.focus_trap);
    }

    #[test]
    fn gtk_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process GTK operability agent
        // (runners/native/gtk-agent/gtk_agent.c). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line the built+run agent emits in a Linux container
        // (Debian, GTK 4.18.6, under `xvfb-run`) for a window holding a real
        // GtkButton, a "fake button" (a GtkBox carrying a GtkGestureClick +
        // handler with no button role / not focusable), and a correctly-built
        // accessible GtkButton. Graph 1 (GtkWidget tree + wired signals / click
        // gestures) joins graph 2 (GtkAccessible role/state) by object identity.
        // The fake button is the motivating finding: operable yet rolePresent
        // false and keyboard-unreachable. GTK4 also surfaces the window's
        // built-in click gesture (role:group#0, a focusless operable element) and
        // the buttons' inner GtkLabel children (operable:false, never gaps); the
        // engine counts every operable-but-inaccessible element, so no_role==1
        // (the fake button alone has no role) while the two focusless operable
        // elements (window + fake button) drive keyboard_unreachable/pointer_only.
        let log = r#"EXPLORE:GROUNDTRUTH {"sig":"44602d5a","focusTrap":false,"elements":[{"id":"role:group#0","operable":true,"gestureKind":"button","a11y":{"rolePresent":true,"namePresent":false,"focusable":false,"inTabOrder":false,"keyboardActivatable":false}},{"id":"key:realButton","operable":true,"gestureKind":"button","a11y":{"rolePresent":true,"namePresent":true,"focusable":true,"inTabOrder":true,"keyboardActivatable":true}},{"id":"role:text#1","operable":false,"gestureKind":"","a11y":{"rolePresent":true,"namePresent":false,"focusable":false,"inTabOrder":false,"keyboardActivatable":false}},{"id":"key:fakeButton","operable":true,"gestureKind":"button","a11y":{"rolePresent":false,"namePresent":false,"focusable":false,"inTabOrder":false,"keyboardActivatable":false}},{"id":"key:goodCustom","operable":true,"gestureKind":"button","a11y":{"rolePresent":true,"namePresent":true,"focusable":true,"inTabOrder":true,"keyboardActivatable":true}},{"id":"role:text#2","operable":false,"gestureKind":"","a11y":{"rolePresent":true,"namePresent":false,"focusable":false,"inTabOrder":false,"keyboardActivatable":false}}]}"#;
        let obs = parse_run(log);
        let g = obs
            .gaps
            .get("44602d5a")
            .expect("gaps for the gtk agent state");
        // The fake button is the only operable element with no accessible role.
        assert_eq!(g.no_role, 1, "fake button alone has no GtkAccessible role");
        // Two operable elements lack focus/keyboard reachability: the fake button
        // and GTK4's window-level click gesture; the real + good buttons are clean.
        assert_eq!(g.keyboard_unreachable, 2);
        assert_eq!(g.pointer_only, 2);
        assert!(!g.focus_trap);
    }

    #[test]
    fn merge_backfills_route_on_a_known_state() {
        // First run had no route; a later run that reports one backfills it.
        let mut map = AppMap {
            app: "t".into(),
            version: 1,
            states: BTreeMap::new(),
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        };
        merge(
            &mut map,
            &parse_run(r#"EXPLORE:STATE {"sig":"abc","labels":[],"unlabeled":0}"#),
        );
        assert!(map
            .states
            .values()
            .next()
            .unwrap()
            .signature
            .route
            .is_none());
        merge(
            &mut map,
            &parse_run(r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":[],"unlabeled":0}"#),
        );
        assert_eq!(
            map.states
                .values()
                .next()
                .unwrap()
                .signature
                .route
                .as_deref(),
            Some("/home")
        );
    }

    #[test]
    fn read_all_device_logs_unions_every_actor() {
        let dir = std::env::temp_dir().join(format!("reproit-maplogs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("drive-a.log"), "EXPLORE:STATE a-line").unwrap();
        std::fs::write(dir.join("drive-b.log"), "EXPLORE:STATE b-line").unwrap();
        // A non-device file must be ignored.
        std::fs::write(dir.join("other.log"), "ignore me").unwrap();
        let joined = read_all_device_logs(&dir).unwrap();
        assert!(joined.contains("a-line"), "device a's log is included");
        assert!(joined.contains("b-line"), "device b's log is included");
        assert!(
            !joined.contains("ignore me"),
            "non-device logs are excluded"
        );
        // Sorted by name: a before b.
        assert!(joined.find("a-line").unwrap() < joined.find("b-line").unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
