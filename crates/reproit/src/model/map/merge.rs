//! Union of parsed observations into the persistent app map.

use super::RunObs;
use crate::model::appmap::{Action, AppMap, Reversibility, State, StateSignature, Transition};
use std::collections::{HashMap, HashSet};

/// sig -> existing state id (states are keyed by id; sig lives in the
/// signature, so labeling renames never break identity).
pub(super) fn sig_index(map: &AppMap) -> HashMap<String, String> {
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
pub(crate) fn merge(map: &mut AppMap, obs: &RunObs) -> bool {
    let mut changed = false;
    let mut index = sig_index(map);
    for (sig, labels) in &obs.states {
        match index.get(sig) {
            Some(id) => {
                // Known state: refresh grounded operability data and backfill
                // the route if a later run reported one we didn't have.
                if let Some(state) = map.states.get_mut(id) {
                    if let Some(g) = obs.gaps.get(sig) {
                        if state.operability_gaps != *g {
                            state.operability_gaps = g.clone();
                            changed = true;
                        }
                    }
                    if state.elements.is_empty() {
                        if let Some(elements) = obs.elements.get(sig) {
                            state.elements = elements.clone();
                            changed = true;
                        }
                    }
                    if state.texts.is_empty() {
                        if let Some(texts) = obs.texts.get(sig) {
                            state.texts = texts.clone();
                            changed = true;
                        }
                    }
                    if state.signature.route.is_none() {
                        if let Some(r) = obs.routes.get(sig) {
                            state.signature.route = Some(r.clone());
                            changed = true;
                        }
                    }
                }
            }
            None => {
                let id = format!("s_{sig}");
                map.states.insert(
                    id.clone(),
                    State {
                        name: None,
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
                        elements: obs.elements.get(sig).cloned().unwrap_or_default(),
                        texts: obs.texts.get(sig).cloned().unwrap_or_default(),
                        operability_gaps: obs.gaps.get(sig).cloned().unwrap_or_default(),
                    },
                );
                index.insert(sig.clone(), id);
                changed = true;
            }
        }
    }
    let mut existing: HashSet<(String, String, String)> = map
        .transitions
        .iter()
        .map(|t| (t.from.clone(), action_str(&t.action), t.to.clone()))
        .collect();
    for (from, action, to) in &obs.edges {
        let (Some(f), Some(t)) = (index.get(from), index.get(to)) else {
            continue;
        };
        let Some(parsed) = parse_action(action) else {
            continue;
        };
        let key = (f.clone(), action_str(&parsed), t.clone());
        if !existing.insert(key) {
            continue;
        }
        map.transitions.push(Transition {
            from: f.clone(),
            to: t.clone(),
            action: parsed,
            guards: vec![],
            reversibility: Reversibility::ProposedReversible,
            expected: None,
        });
        changed = true;
    }
    changed
}

pub(crate) fn action_str(a: &Action) -> String {
    match a {
        Action::Tap { finder } => format!("tap:{finder}"),
        Action::Back => "back".to_string(),
        Action::Type { finder, .. } => format!("type:{finder}"),
        Action::Scroll { finder, .. } => format!("scroll:{finder}"),
        Action::Key { key } => format!("key:{key}"),
        Action::System { event } => format!("system:{event}"),
    }
}

/// Inverse of [`action_str`]: parse an `EXPLORE:EDGE` action string back into
/// an `Action`. `type:`/`scroll:`/`system:` MUST be parsed into their real
/// variants (not collapsed to `Back`) or a form-driven transition lands in the
/// persisted map as a meaningless `back` edge -- losing the finder/value, so
/// the screen behind a typed input becomes unreplayable and frontier guidance
/// over the map is wrong wherever a state is only reachable through typed
/// input.
pub(super) fn parse_action(s: &str) -> Option<Action> {
    if let Some(finder) = s.strip_prefix("tap:") {
        return (!finder.is_empty()).then(|| Action::Tap {
            finder: finder.to_string(),
        });
    }
    if let Some(rest) = s.strip_prefix("type:") {
        // Raw typed values are runtime evidence, not graph identity. Keep only
        // the structural finder so secrets and personal data cannot enter the
        // reviewable app map.
        let finder = rest
            .split_once('=')
            .map(|(finder, _)| finder)
            .unwrap_or(rest);
        let finder = finder.to_string();
        let text = String::new();
        return (!finder.is_empty()).then_some(Action::Type { finder, text });
    }
    if let Some(rest) = s.strip_prefix("scroll:") {
        // `scroll:<finder>` or `scroll:<finder>=<dy>` (dy optional/recoverable).
        let (finder, dy) = match rest.rsplit_once('=') {
            Some((f, d)) => (f.to_string(), d.parse().ok()?),
            None => (rest.to_string(), 0),
        };
        return (!finder.is_empty()).then_some(Action::Scroll { finder, dy });
    }
    if let Some(key) = s.strip_prefix("key:") {
        return (!key.is_empty()).then(|| Action::Key {
            key: key.to_string(),
        });
    }
    if let Some(ev) = s.strip_prefix("system:") {
        return (!ev.is_empty()).then(|| Action::System {
            event: ev.to_string(),
        });
    }
    (s == "back").then_some(Action::Back)
}
