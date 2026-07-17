//! Borrowed indexes over the deterministic, reviewable map representation.

use crate::model::appmap::{AppMap, Transition};
use std::collections::{HashMap, HashSet};

/// Query-oriented view built once for a graph operation. It is deliberately
/// not serialized: JSON remains a small `BTreeMap` plus transition list.
pub(crate) struct GraphIndex<'a> {
    by_signature: HashMap<&'a str, &'a str>,
    signature_by_state: HashMap<&'a str, &'a str>,
    outgoing: HashMap<&'a str, Vec<&'a Transition>>,
    incoming: HashSet<&'a str>,
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
        for transition in &map.transitions {
            outgoing
                .entry(transition.from.as_str())
                .or_default()
                .push(transition);
            incoming.insert(transition.to.as_str());
        }
        Self {
            by_signature,
            signature_by_state,
            outgoing,
            incoming,
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
}
