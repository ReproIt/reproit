//! Bounded graph analysis used only to prioritize exploration.

use super::index::GraphIndex;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

const MAX_SCC_STATES: usize = 20_000;
const MAX_SCC_EDGES: usize = 100_000;
const MAX_DOMINATOR_STATES: usize = 1_024;
const MAX_DOMINATOR_EDGES: usize = 16_384;

#[derive(Default)]
pub(crate) struct GraphGuidance<'a> {
    component_by_state: HashMap<&'a str, usize>,
    components: Vec<Vec<&'a str>>,
    dominated: HashMap<&'a str, usize>,
}

impl<'a> GraphGuidance<'a> {
    pub(crate) fn analyze(graph: &GraphIndex<'a>, start: &'a str) -> Self {
        let reachable = reachable_states(graph, start);
        let edge_count = reachable
            .iter()
            .map(|state| graph.outgoing(state).len())
            .sum::<usize>();
        if reachable.len() > MAX_SCC_STATES || edge_count > MAX_SCC_EDGES {
            return Self::default();
        }
        let (component_by_state, components) = strongly_connected(graph, &reachable);
        let dominated =
            if reachable.len() <= MAX_DOMINATOR_STATES && edge_count <= MAX_DOMINATOR_EDGES {
                dominated_counts(graph, start, &reachable)
            } else {
                HashMap::new()
            };
        Self {
            component_by_state,
            components,
            dominated,
        }
    }

    pub(crate) fn component_members(&self, state: &str) -> &[&'a str] {
        self.component_by_state
            .get(state)
            .and_then(|index| self.components.get(*index))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn dominated_count(&self, state: &str) -> usize {
        self.dominated.get(state).copied().unwrap_or(0)
    }

    pub(crate) fn component_totals(
        &self,
        mut score: impl FnMut(&str) -> u64,
    ) -> HashMap<&'a str, u64> {
        let mut totals = HashMap::with_capacity(self.component_by_state.len());
        for members in &self.components {
            let total = members
                .iter()
                .map(|state| score(state))
                .fold(0_u64, u64::saturating_add);
            for &state in members {
                totals.insert(state, total);
            }
        }
        totals
    }
}

fn reachable_states<'a>(graph: &GraphIndex<'a>, start: &'a str) -> BTreeSet<&'a str> {
    let mut reachable = BTreeSet::from([start]);
    let mut queue = VecDeque::from([start]);
    while let Some(state) = queue.pop_front() {
        for transition in graph.outgoing(state) {
            let to = transition.to.as_str();
            if reachable.insert(to) {
                queue.push_back(to);
            }
        }
    }
    reachable
}

fn strongly_connected<'a>(
    graph: &GraphIndex<'a>,
    reachable: &BTreeSet<&'a str>,
) -> (HashMap<&'a str, usize>, Vec<Vec<&'a str>>) {
    let mut visited = HashSet::new();
    let mut order = Vec::with_capacity(reachable.len());
    for &root in reachable {
        if !visited.insert(root) {
            continue;
        }
        let mut stack = vec![(root, 0_usize)];
        while !stack.is_empty() {
            let last = stack.len() - 1;
            let (state, edge_index) = stack[last];
            let edges = graph.outgoing(state);
            if edge_index < edges.len() {
                stack[last].1 += 1;
                let to = edges[edge_index].to.as_str();
                if reachable.contains(to) && visited.insert(to) {
                    stack.push((to, 0));
                }
            } else {
                order.push(state);
                stack.pop();
            }
        }
    }

    let mut reverse = HashMap::<&str, Vec<&str>>::new();
    for &state in reachable {
        for transition in graph.outgoing(state) {
            let to = transition.to.as_str();
            if reachable.contains(to) {
                reverse.entry(to).or_default().push(state);
            }
        }
    }
    let mut component_by_state = HashMap::new();
    let mut components = Vec::new();
    for &root in order.iter().rev() {
        if component_by_state.contains_key(root) {
            continue;
        }
        let component_index = components.len();
        let mut members = Vec::new();
        let mut stack = vec![root];
        component_by_state.insert(root, component_index);
        while let Some(state) = stack.pop() {
            members.push(state);
            for &previous in reverse.get(state).map(Vec::as_slice).unwrap_or(&[]) {
                if component_by_state
                    .insert(previous, component_index)
                    .is_none()
                {
                    stack.push(previous);
                }
            }
        }
        members.sort_unstable();
        components.push(members);
    }
    (component_by_state, components)
}

fn dominated_counts<'a>(
    graph: &GraphIndex<'a>,
    start: &'a str,
    reachable: &BTreeSet<&'a str>,
) -> HashMap<&'a str, usize> {
    let mut predecessors = BTreeMap::<&str, BTreeSet<&str>>::new();
    for &state in reachable {
        for transition in graph.outgoing(state) {
            let to = transition.to.as_str();
            if reachable.contains(to) {
                predecessors.entry(to).or_default().insert(state);
            }
        }
    }
    let mut dominators = BTreeMap::new();
    for &state in reachable {
        let initial = if state == start {
            BTreeSet::from([start])
        } else {
            reachable.clone()
        };
        dominators.insert(state, initial);
    }
    for _ in 0..reachable.len() {
        let mut changed = false;
        for &state in reachable {
            if state == start {
                continue;
            }
            let Some(preds) = predecessors.get(state) else {
                continue;
            };
            let mut intersection = reachable.clone();
            for predecessor in preds {
                intersection = intersection
                    .intersection(&dominators[predecessor])
                    .copied()
                    .collect();
            }
            intersection.insert(state);
            if dominators.get(state) != Some(&intersection) {
                dominators.insert(state, intersection);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut counts = HashMap::new();
    for (&state, state_dominators) in &dominators {
        for &dominator in state_dominators {
            if dominator != state {
                *counts.entry(dominator).or_insert(0) += 1;
            }
        }
    }
    counts
}
