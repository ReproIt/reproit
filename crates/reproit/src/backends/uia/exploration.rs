//! Fuzz configuration, deterministic randomness, and frontier bookkeeping.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::ACTION_BUDGET;

pub(super) struct Fuzz {
    pub(super) seed: u32,
    pub(super) budget: u32,
    pub(super) configured: bool,
    pub(super) replay: Option<Vec<String>>,
    pub(super) prefix: Option<Vec<String>>,
    pub(super) edge_weights: BTreeMap<String, BTreeMap<String, u64>>,
    pub(super) clip_sel: Option<String>,
    pub(super) clip_label: Option<String>,
    pub(super) clip_oracle: Option<String>,
}

pub(super) fn load_fuzz() -> Fuzz {
    let mut fuzz = Fuzz {
        seed: 0,
        budget: ACTION_BUDGET,
        configured: false,
        replay: None,
        prefix: None,
        edge_weights: BTreeMap::new(),
        clip_sel: None,
        clip_label: None,
        clip_oracle: None,
    };
    if let Ok(raw) = std::env::var("REPROIT_FUZZ_BUDGET") {
        if let Ok(budget) = raw.parse::<u32>() {
            fuzz.budget = budget;
            fuzz.configured = true;
        }
    }
    let Ok(path) = std::env::var("REPROIT_FUZZ_CONFIG") else {
        return fuzz;
    };
    fuzz.configured = true;
    let Ok(raw) = std::fs::read_to_string(path) else {
        return fuzz;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return fuzz;
    };
    if let Some(seed) = json.get("seed").and_then(serde_json::Value::as_u64) {
        fuzz.seed = seed as u32;
    }
    if let Some(budget) = json.get("budget").and_then(serde_json::Value::as_u64) {
        fuzz.budget = budget as u32;
    }
    fuzz.replay = string_array(&json, "replay");
    fuzz.prefix = string_array(&json, "prefix");
    if let Some(clip) = json.get("clip").and_then(serde_json::Value::as_object) {
        let string_field = |key: &str| {
            clip.get(key)
                .and_then(serde_json::Value::as_str)
                .map(String::from)
        };
        fuzz.clip_sel = string_field("sel");
        fuzz.clip_label = string_field("label");
        fuzz.clip_oracle = string_field("oracle");
    }
    if let Some(weights) = json
        .get("edgeWeights")
        .and_then(serde_json::Value::as_object)
    {
        for (sig, weights) in weights {
            if let Some(weights) = weights.as_object() {
                fuzz.edge_weights.insert(
                    sig.clone(),
                    weights
                        .iter()
                        .filter_map(|(key, value)| value.as_u64().map(|n| (key.clone(), n)))
                        .collect(),
                );
            }
        }
    }
    fuzz
}

fn string_array(json: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    json.get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect()
        })
}

pub(super) struct Rng {
    state: u32,
}

impl Rng {
    pub(super) fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn step(&mut self) -> u32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 17;
        self.state ^= self.state << 5;
        self.state
    }

    pub(super) fn unit(&mut self) -> f64 {
        (self.step() & 0x7fff_ffff) as f64 / (0x8000_0000u32 as f64)
    }
}

pub(super) fn edge_key(sig: &str, action: &str) -> String {
    format!("{sig}|{action}")
}

pub(super) fn remember_actions(
    actions_by_state: &mut BTreeMap<String, Vec<String>>,
    sig: &str,
    actions: Vec<String>,
) {
    let known = actions_by_state.entry(sig.to_string()).or_default();
    for action in actions {
        if !known.contains(&action) {
            known.push(action);
        }
    }
}

pub(super) fn first_untried_action(
    actions_by_state: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
    sig: &str,
) -> Option<String> {
    actions_by_state.get(sig).and_then(|actions| {
        actions
            .iter()
            .find(|action| !tried.contains(&edge_key(sig, action)))
            .cloned()
    })
}

pub(super) fn has_frontier(
    actions_by_state: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
) -> bool {
    actions_by_state
        .keys()
        .any(|sig| first_untried_action(actions_by_state, tried, sig).is_some())
}

pub(super) fn remember_edge(
    graph: &mut BTreeMap<String, Vec<(String, String)>>,
    from: &str,
    action: &str,
    to: &str,
) {
    let edges = graph.entry(from.to_string()).or_default();
    if !edges.iter().any(|(a, target)| a == action && target == to) {
        edges.push((action.to_string(), to.to_string()));
    }
}

pub(super) fn path_to_frontier(
    graph: &BTreeMap<String, Vec<(String, String)>>,
    actions_by_state: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
    from: &str,
) -> Option<Vec<String>> {
    if first_untried_action(actions_by_state, tried, from).is_some() {
        return Some(Vec::new());
    }
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    seen.insert(from.to_string());
    queue.push_back((from.to_string(), Vec::<String>::new()));
    while let Some((sig, path)) = queue.pop_front() {
        if let Some(edges) = graph.get(&sig) {
            for (action, target) in edges {
                if !seen.insert(target.clone()) {
                    continue;
                }
                let mut next_path = path.clone();
                next_path.push(action.clone());
                if first_untried_action(actions_by_state, tried, target).is_some() {
                    return Some(next_path);
                }
                queue.push_back((target.clone(), next_path));
            }
        }
    }
    None
}
