//! Borrowed indexes over the deterministic, reviewable map representation.

use crate::model::appmap::{Action, AppMap, Transition};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EdgeSummary {
    pub(crate) outgoing: usize,
    pub(crate) distinct_actions: usize,
}

/// Query-oriented view built once for a graph operation. It is deliberately
/// not serialized: JSON remains a small `BTreeMap` plus transition list.
pub(crate) struct GraphIndex<'a> {
    by_signature: HashMap<&'a str, &'a str>,
    signature_by_state: HashMap<&'a str, &'a str>,
    outgoing: HashMap<&'a str, Vec<&'a Transition>>,
    incoming: HashSet<&'a str>,
    by_action: HashMap<&'a str, HashMap<&'a Action, Vec<&'a Transition>>>,
    summaries: HashMap<&'a str, EdgeSummary>,
}

impl<'a> GraphIndex<'a> {
    pub(crate) fn new(map: &'a AppMap) -> Self {
        let mut by_signature = HashMap::new();
        let mut signature_by_state = HashMap::new();
        for (id, state) in &map.states {
            if let Some(signature) = state.signature.semantics_hash.as_deref() {
                by_signature.insert(signature, id.as_str());
                signature_by_state.insert(id.as_str(), signature);
            }
        }
        let mut outgoing: HashMap<&str, Vec<&Transition>> = HashMap::new();
        let mut incoming = HashSet::new();
        let mut by_action: HashMap<&str, HashMap<&Action, Vec<&Transition>>> = HashMap::new();
        let mut actions_by_state: HashMap<&str, HashSet<&Action>> = HashMap::new();
        for transition in &map.transitions {
            outgoing
                .entry(transition.from.as_str())
                .or_default()
                .push(transition);
            incoming.insert(transition.to.as_str());
            by_action
                .entry(transition.from.as_str())
                .or_default()
                .entry(&transition.action)
                .or_default()
                .push(transition);
            actions_by_state
                .entry(transition.from.as_str())
                .or_default()
                .insert(&transition.action);
        }
        let summaries = outgoing
            .iter()
            .map(|(state, edges)| {
                let distinct_actions = actions_by_state.get(state).map(HashSet::len).unwrap_or(0);
                (
                    *state,
                    EdgeSummary {
                        outgoing: edges.len(),
                        distinct_actions,
                    },
                )
            })
            .collect();
        Self {
            by_signature,
            signature_by_state,
            outgoing,
            incoming,
            by_action,
            summaries,
        }
    }

    pub(crate) fn state_for_signature(&self, signature: &str) -> Option<&'a str> {
        self.by_signature.get(signature).copied()
    }

    pub(crate) fn signature_for_state(&self, state: &str) -> Option<&'a str> {
        self.signature_by_state.get(state).copied()
    }

    pub(crate) fn outgoing(&self, state: &str) -> &[&'a Transition] {
        self.outgoing.get(state).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn has_incoming(&self, state: &str) -> bool {
        self.incoming.contains(state)
    }

    #[allow(dead_code)] // Used by replay/benchmark consumers, not every CLI build.
    pub(crate) fn transitions_for_action(&self, state: &str, action: &Action) -> &[&'a Transition] {
        self.by_action
            .get(state)
            .and_then(|actions| actions.get(action))
            .map(std::vec::Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn summary(&self, state: &str) -> EdgeSummary {
        self.summaries.get(state).copied().unwrap_or_default()
    }
}
