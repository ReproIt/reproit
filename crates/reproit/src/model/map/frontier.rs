//! Visit weighting and deterministic path/frontier selection.

use super::merge::{action_str, sig_index};
use crate::model::appmap::{AppMap, Transition};
use std::collections::{BTreeMap, HashMap, VecDeque};

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
/// the app's REAL navigation (discovered by the internal model crawl) instead
/// of hallucinated taps. Returns (target_state_name, ordered action strings);
/// the path is empty when the entry state itself matches.
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
pub(super) const VISIT_WEIGHT_CAP: u64 = 8;

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
        // Deterministic frontier choice: least-visited, then deepest path, then a
        // STABLE tie-break on the structural signature (and id). Without the last
        // two keys a tie resolved on `HashMap` iteration order, which is randomized
        // per run -- so `fuzz --frontier` picked a different target (and replayed a
        // different prefix for every seed) run-to-run, breaking reproducibility.
        .min_by_key(|(id, path)| {
            let sig = sig_of.get(id.as_str()).copied().unwrap_or("");
            let count = visits.counts.get(sig).copied().unwrap_or(0);
            (count, usize::MAX - path.len(), sig, id.as_str())
        })
        .map(|(id, path)| (id.clone(), path.clone()))
}
