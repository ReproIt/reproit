//! Versioned, bounded dependency graph for one executable causal capsule.

use super::Capsule;
use crate::domain::backend::BackendEventKind;
use anyhow::{bail, Result};
use reproit_protocol::{
    CausalEdge, CausalEdgeKind, CausalGraph, CausalNode, CausalNodeKind, CausalTarget,
    CAUSAL_GRAPH_VERSION,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_CAUSAL_LABEL_BYTES: usize = 16 * 1024;

pub fn build(capsule: &Capsule) -> Result<CausalGraph> {
    let mut builder = GraphBuilder::default();
    let finding = builder.node(CausalNodeKind::Finding, "exact finding", None, None)?;
    let mut actor_nodes = BTreeMap::new();
    let mut action_nodes = BTreeMap::new();
    let mut last_action = BTreeMap::new();

    for actor in capsule_actors(capsule) {
        let node = builder.node(
            CausalNodeKind::ActorEvent,
            &format!("actor {actor}"),
            Some(actor.clone()),
            None,
        )?;
        actor_nodes.insert(actor, node);
    }

    let mut environment_nodes = Vec::new();
    for (key, value) in &capsule.environment {
        let kind = if key == "status_bar_time" {
            CausalNodeKind::Timer
        } else if key.starts_with("permission:") {
            CausalNodeKind::Permission
        } else {
            CausalNodeKind::Environment
        };
        let node = builder.node(
            kind,
            &format!("{key}={value}"),
            None,
            Some(CausalTarget::Environment { key: key.clone() }),
        )?;
        environment_nodes.push(node);
    }

    for action in &capsule.actions {
        let node = builder.node(
            CausalNodeKind::Action,
            &action.action,
            Some(action.actor.clone()),
            Some(CausalTarget::Action {
                actor: action.actor.clone(),
                index: action.index,
            }),
        )?;
        if let Some(owner) = actor_nodes.get(&action.actor) {
            builder.edge(owner, &node, CausalEdgeKind::ActorOwnership);
        }
        if let Some(previous) = last_action.insert(action.actor.clone(), node.clone()) {
            builder.edge(&previous, &node, CausalEdgeKind::HappensBefore);
        }
        action_nodes.insert((action.actor.clone(), action.index), node.clone());
        if let Some(to_sig) = &action.to_sig {
            let state = builder.node(
                CausalNodeKind::StateWrite,
                &format!("state {to_sig}"),
                Some(action.actor.clone()),
                None,
            )?;
            builder.edge(&node, &state, CausalEdgeKind::DataDependency);
            builder.edge(&state, &finding, CausalEdgeKind::ContractScope);
        }
    }

    for environment in &environment_nodes {
        if action_nodes.is_empty() {
            builder.edge(environment, &finding, CausalEdgeKind::StatePrerequisite);
        } else {
            for first in first_actions(&capsule.actions, &action_nodes) {
                builder.edge(environment, first, CausalEdgeKind::StatePrerequisite);
            }
        }
    }

    for exchange in &capsule.exchanges {
        let target = CausalTarget::Exchange {
            actor: exchange.actor.clone(),
            action_index: exchange.action_index,
            ordinal: exchange.ordinal,
            exchange_id: exchange.id.clone(),
        };
        let node = builder.node(
            CausalNodeKind::Response,
            &format!("{} {}", exchange.method, exchange.url),
            Some(exchange.actor.clone()),
            Some(target),
        )?;
        if let Some(owner) = actor_nodes.get(&exchange.actor) {
            builder.edge(owner, &node, CausalEdgeKind::ActorOwnership);
        }
        if let Some(action) = action_nodes.get(&(exchange.actor.clone(), exchange.action_index)) {
            builder.edge(action, &node, CausalEdgeKind::HappensBefore);
            builder.edge(action, &node, CausalEdgeKind::DataDependency);
        }
        builder.edge(&node, &finding, CausalEdgeKind::ContractScope);
    }

    let mut backend_nodes = BTreeMap::new();
    for event in &capsule.backend_events {
        let actor = event.actor.clone().unwrap_or_else(|| "a".to_string());
        let kind = match event.event {
            BackendEventKind::Effect { .. } => CausalNodeKind::StateWrite,
            BackendEventKind::Start { .. } | BackendEventKind::Return { .. } => {
                CausalNodeKind::Callback
            }
            BackendEventKind::Protocol { .. } => CausalNodeKind::BackendEvent,
        };
        let node = builder.node(
            kind,
            &event.operation,
            Some(actor.clone()),
            Some(CausalTarget::BackendEvent {
                sequence: event.sequence,
                trace_id: event.trace_id.clone(),
                span_id: event.span_id.clone(),
            }),
        )?;
        if let Some(owner) = actor_nodes.get(&actor) {
            builder.edge(owner, &node, CausalEdgeKind::ActorOwnership);
        }
        if let Some(action) = action_nodes.get(&(actor, event.action_index)) {
            builder.edge(action, &node, CausalEdgeKind::DataDependency);
        }
        backend_nodes.insert(
            (event.trace_id.clone(), event.span_id.clone()),
            node.clone(),
        );
        builder.edge(&node, &finding, CausalEdgeKind::ContractScope);
    }
    for event in &capsule.backend_events {
        let Some(parent) = event
            .parent_span_id
            .as_ref()
            .and_then(|parent| backend_nodes.get(&(event.trace_id.clone(), parent.clone())))
        else {
            continue;
        };
        if let Some(child) = backend_nodes.get(&(event.trace_id.clone(), event.span_id.clone())) {
            builder.edge(parent, child, CausalEdgeKind::DataDependency);
        }
    }
    for last in last_action.values() {
        builder.edge(last, &finding, CausalEdgeKind::HappensBefore);
    }

    builder.finish()
}

impl Capsule {
    /// Return a candidate with the requested causal nodes and every hard
    /// dependent removed atomically. Action-local clocks are then compacted so
    /// transport and backend evidence still refer to the same actor action.
    pub fn reduced_without_nodes(&self, requested: &BTreeSet<String>) -> Result<Self> {
        let removed = self.causal_graph.removal_closure(requested);
        let targets = self
            .causal_graph
            .nodes
            .iter()
            .filter(|node| removed.contains(&node.id))
            .filter_map(|node| node.target.clone())
            .collect::<BTreeSet<_>>();
        let mut candidate = self.clone();
        candidate.actions.retain(|action| {
            !targets.contains(&CausalTarget::Action {
                actor: action.actor.clone(),
                index: action.index,
            })
        });
        candidate.exchanges.retain(|exchange| {
            !targets.contains(&CausalTarget::Exchange {
                actor: exchange.actor.clone(),
                action_index: exchange.action_index,
                ordinal: exchange.ordinal,
                exchange_id: exchange.id.clone(),
            })
        });
        candidate.backend_events.retain(|event| {
            !targets.contains(&CausalTarget::BackendEvent {
                sequence: event.sequence,
                trace_id: event.trace_id.clone(),
                span_id: event.span_id.clone(),
            })
        });
        candidate.compact_action_clocks();
        candidate.refresh_causal_graph()?;
        Ok(candidate)
    }

    pub fn replay_actions(&self) -> Vec<String> {
        self.actions
            .iter()
            .map(|action| action.action.clone())
            .collect()
    }

    pub fn refresh_causal_graph(&mut self) -> Result<()> {
        self.causal_graph = build(self)?;
        Ok(())
    }

    fn compact_action_clocks(&mut self) {
        let mut removed_before = BTreeMap::<String, Vec<u32>>::new();
        let retained = self
            .actions
            .iter()
            .map(|action| (action.actor.clone(), action.index))
            .collect::<BTreeSet<_>>();
        for action in &self.actions {
            removed_before.entry(action.actor.clone()).or_default();
        }
        for node in &self.causal_graph.nodes {
            let Some(CausalTarget::Action { actor, index }) = &node.target else {
                continue;
            };
            if !retained.contains(&(actor.clone(), *index)) {
                removed_before
                    .entry(actor.clone())
                    .or_default()
                    .push(*index);
            }
        }
        let compact = |actor: &str, index: u32| {
            let removed = removed_before
                .get(actor)
                .map(|indices| indices.iter().filter(|removed| **removed < index).count())
                .unwrap_or(0);
            index.saturating_sub(removed as u32)
        };
        for action in &mut self.actions {
            action.index = compact(&action.actor, action.index);
        }
        for exchange in &mut self.exchanges {
            exchange.action_index = compact(&exchange.actor, exchange.action_index);
        }
        for event in &mut self.backend_events {
            let actor = event.actor.as_deref().unwrap_or("a");
            event.action_index = compact(actor, event.action_index);
        }
    }
}

#[derive(Default)]
struct GraphBuilder {
    nodes: Vec<CausalNode>,
    edges: BTreeSet<CausalEdge>,
}

impl GraphBuilder {
    fn node(
        &mut self,
        kind: CausalNodeKind,
        label: &str,
        actor: Option<String>,
        target: Option<CausalTarget>,
    ) -> Result<String> {
        if label.len() > MAX_CAUSAL_LABEL_BYTES {
            bail!("causal node label exceeds {MAX_CAUSAL_LABEL_BYTES} bytes");
        }
        let encoded = serde_json::to_vec(&(kind, label, &actor, &target))?;
        let digest = Sha256::digest(encoded);
        let id = format!(
            "cause_{}",
            digest[..10]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        if self.nodes.iter().all(|node| node.id != id) {
            self.nodes.push(CausalNode {
                id: id.clone(),
                kind,
                label: label.to_string(),
                actor,
                target,
            });
        }
        Ok(id)
    }

    fn edge(&mut self, from: &str, to: &str, kind: CausalEdgeKind) {
        if from != to {
            self.edges.insert(CausalEdge {
                from: from.to_string(),
                to: to.to_string(),
                kind,
            });
        }
    }

    fn finish(self) -> Result<CausalGraph> {
        let graph = CausalGraph {
            version: CAUSAL_GRAPH_VERSION,
            nodes: self.nodes,
            edges: self.edges.into_iter().collect(),
        };
        graph.validate()?;
        Ok(graph)
    }
}

fn capsule_actors(capsule: &Capsule) -> BTreeSet<String> {
    capsule
        .actions
        .iter()
        .map(|action| action.actor.clone())
        .chain(
            capsule
                .exchanges
                .iter()
                .map(|exchange| exchange.actor.clone()),
        )
        .chain(
            capsule
                .backend_events
                .iter()
                .map(|event| event.actor.clone().unwrap_or_else(|| "a".to_string())),
        )
        .chain(["a".to_string()])
        .collect()
}

fn first_actions<'a>(
    actions: &'a [super::Action],
    nodes: &'a BTreeMap<(String, u32), String>,
) -> Vec<&'a String> {
    let mut first = BTreeMap::<&str, u32>::new();
    for action in actions {
        first
            .entry(&action.actor)
            .and_modify(|index| *index = (*index).min(action.index))
            .or_insert(action.index);
    }
    first
        .into_iter()
        .filter_map(|(actor, index)| nodes.get(&(actor.to_string(), index)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend::{BackendEvent, BackendEventKind};
    use crate::domain::capsule::{Action, Exchange, FindingIdentity};
    use serde_json::Value;

    fn capsule() -> Capsule {
        let mut capsule = Capsule::new(
            "app",
            FindingIdentity {
                oracle: "crash".into(),
                invariant: "no-exception".into(),
                kind: "exception".into(),
                message: "boom".into(),
                frame: "main:1".into(),
                trigger: "tap:key:save".into(),
                boundary: None,
            },
        );
        capsule
            .environment
            .insert("status_bar_time".into(), "09:41".into());
        capsule
            .environment
            .insert("permission:camera".into(), "denied".into());
        capsule.actions = vec![
            Action {
                index: 1,
                actor: "a".into(),
                action: "tap:key:open".into(),
                from_sig: Some("home".into()),
                to_sig: Some("form".into()),
            },
            Action {
                index: 2,
                actor: "a".into(),
                action: "tap:key:save".into(),
                from_sig: Some("form".into()),
                to_sig: None,
            },
        ];
        for (id, action_index) in [("open", 1), ("save", 2)] {
            capsule.exchanges.push(Exchange {
                id: id.into(),
                actor: "a".into(),
                action_index,
                ordinal: 0,
                protocol: "https".into(),
                method: "GET".into(),
                url: format!("https://example.test/{id}"),
                request_headers: BTreeMap::new(),
                request_body: None,
                status: 200,
                response_headers: BTreeMap::new(),
                response_body: None,
                required: true,
            });
        }
        capsule.backend_events = vec![
            BackendEvent {
                sequence: 1,
                trace_id: "trace".into(),
                span_id: "start".into(),
                action_index: 2,
                parent_span_id: None,
                operation: "save".into(),
                build: None,
                config_contract: None,
                actor: Some("a".into()),
                tenant: None,
                idempotency_key: None,
                selections: Vec::new(),
                event: BackendEventKind::Start { input: Value::Null },
            },
            BackendEvent {
                sequence: 2,
                trace_id: "trace".into(),
                span_id: "effect".into(),
                action_index: 2,
                parent_span_id: Some("start".into()),
                operation: "save".into(),
                build: None,
                config_contract: None,
                actor: Some("a".into()),
                tenant: None,
                idempotency_key: None,
                selections: Vec::new(),
                event: BackendEventKind::Effect {
                    effect: crate::domain::backend::EffectKind::Write,
                    resource: None,
                    key: None,
                    tenant: None,
                    event: None,
                    before: None,
                    after: None,
                    payload: None,
                },
            },
        ];
        capsule.finalize_id().unwrap();
        capsule
    }

    #[test]
    fn graph_contains_all_captured_dependency_classes_and_edge_kinds() {
        let graph = &capsule().causal_graph;
        for kind in [
            CausalNodeKind::Action,
            CausalNodeKind::Response,
            CausalNodeKind::Timer,
            CausalNodeKind::Permission,
            CausalNodeKind::StateWrite,
            CausalNodeKind::Callback,
            CausalNodeKind::ActorEvent,
            CausalNodeKind::Finding,
        ] {
            assert!(graph.nodes.iter().any(|node| node.kind == kind));
        }
        for kind in [
            CausalEdgeKind::HappensBefore,
            CausalEdgeKind::DataDependency,
            CausalEdgeKind::StatePrerequisite,
            CausalEdgeKind::ActorOwnership,
            CausalEdgeKind::ContractScope,
        ] {
            assert!(graph.edges.iter().any(|edge| edge.kind == kind));
        }
        graph.validate().unwrap();
    }

    #[test]
    fn action_reduction_removes_hard_dependents_and_compacts_actor_clock() {
        let capsule = capsule();
        let first_action = capsule
            .causal_graph
            .nodes
            .iter()
            .find(|node| {
                node.target
                    == Some(CausalTarget::Action {
                        actor: "a".into(),
                        index: 1,
                    })
            })
            .unwrap()
            .id
            .clone();
        let reduced = capsule
            .reduced_without_nodes(&BTreeSet::from([first_action]))
            .unwrap();

        assert_eq!(reduced.replay_actions(), vec!["tap:key:save"]);
        assert_eq!(reduced.actions[0].index, 1);
        assert_eq!(reduced.exchanges.len(), 1);
        assert_eq!(reduced.exchanges[0].id, "save");
        assert_eq!(reduced.exchanges[0].action_index, 1);
        reduced.causal_graph.validate().unwrap();
    }

    #[test]
    fn validation_rejects_a_cycle() {
        let mut graph = capsule().causal_graph;
        let edge = graph.edges[0].clone();
        graph.edges.push(CausalEdge {
            from: edge.to,
            to: edge.from,
            kind: CausalEdgeKind::HappensBefore,
        });
        assert!(graph.validate().is_err());
    }
}
