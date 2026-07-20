use super::{BackendConfig, BackendEvent, BackendEventKind, EffectKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CausalContractGraph {
    pub nodes: BTreeMap<String, GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphNode {
    pub id: String,
    pub kind: GraphNodeKind,
    #[serde(default)]
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GraphNodeKind {
    Operation,
    Function,
    ValueDomain,
    Resource,
    Event,
    Actor,
    Tenant,
    Contract,
    RuntimeObservation,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub relation: GraphRelation,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GraphRelation {
    Implements,
    Calls,
    Consumes,
    Produces,
    Reads,
    Writes,
    Deletes,
    Emits,
    Requires,
    Ensures,
    ObservedAs,
    ActsAs,
    BelongsTo,
    HappensBefore,
}

pub fn build_graph(config: &BackendConfig, events: &[BackendEvent]) -> CausalContractGraph {
    let mut graph = CausalContractGraph::default();
    for contract in &config.operations {
        let operation = format!("operation:{}", contract.id);
        graph.nodes.insert(
            operation.clone(),
            GraphNode {
                id: operation.clone(),
                kind: GraphNodeKind::Operation,
                attributes: BTreeMap::new(),
            },
        );
        let contract_id = format!("contract:{}:{}", contract.id, contract.authority as u8);
        graph.nodes.insert(
            contract_id.clone(),
            GraphNode {
                id: contract_id.clone(),
                kind: GraphNodeKind::Contract,
                attributes: BTreeMap::new(),
            },
        );
        graph.edges.push(GraphEdge {
            from: contract_id,
            to: operation,
            relation: GraphRelation::Ensures,
        });
    }
    for program in &config.programs {
        for function in &program.functions {
            let function_id = format!("function:{}", function.id);
            graph.nodes.insert(
                function_id.clone(),
                GraphNode {
                    id: function_id.clone(),
                    kind: GraphNodeKind::Function,
                    attributes: BTreeMap::from([
                        ("name".into(), Value::String(function.name.clone())),
                        ("language".into(), Value::String(program.language.clone())),
                    ]),
                },
            );
            if let Some(operation) = &function.operation {
                let operation = format!("operation:{operation}");
                graph.nodes.entry(operation.clone()).or_insert(GraphNode {
                    id: operation.clone(),
                    kind: GraphNodeKind::Operation,
                    attributes: BTreeMap::new(),
                });
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: operation,
                    relation: GraphRelation::Implements,
                });
            }
            for call in &function.calls {
                let callee = format!("function:{call}");
                graph.nodes.entry(callee.clone()).or_insert(GraphNode {
                    id: callee.clone(),
                    kind: GraphNodeKind::Function,
                    attributes: BTreeMap::new(),
                });
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: callee,
                    relation: GraphRelation::Calls,
                });
            }
            for input in &function.inputs {
                let domain = format!("domain:{}:input:{}", function.id, input.name);
                graph.nodes.insert(
                    domain.clone(),
                    GraphNode {
                        id: domain.clone(),
                        kind: GraphNodeKind::ValueDomain,
                        attributes: BTreeMap::from([(
                            "shape".into(),
                            serde_json::to_value(&input.domain).unwrap_or(Value::Null),
                        )]),
                    },
                );
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: domain,
                    relation: GraphRelation::Consumes,
                });
            }
            if let Some(output) = &function.output {
                let domain = format!("domain:{}:output", function.id);
                graph.nodes.insert(
                    domain.clone(),
                    GraphNode {
                        id: domain.clone(),
                        kind: GraphNodeKind::ValueDomain,
                        attributes: BTreeMap::from([(
                            "shape".into(),
                            serde_json::to_value(output).unwrap_or(Value::Null),
                        )]),
                    },
                );
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: domain,
                    relation: GraphRelation::Produces,
                });
            }
            for effect in &function.effects {
                let (name, kind, relation) = match effect.kind {
                    EffectKind::Read => (
                        effect.resource.as_deref().unwrap_or("unknown"),
                        GraphNodeKind::Resource,
                        GraphRelation::Reads,
                    ),
                    EffectKind::Write => (
                        effect.resource.as_deref().unwrap_or("unknown"),
                        GraphNodeKind::Resource,
                        GraphRelation::Writes,
                    ),
                    EffectKind::Delete => (
                        effect.resource.as_deref().unwrap_or("unknown"),
                        GraphNodeKind::Resource,
                        GraphRelation::Deletes,
                    ),
                    EffectKind::Emit => (
                        effect.event.as_deref().unwrap_or("unknown"),
                        GraphNodeKind::Event,
                        GraphRelation::Emits,
                    ),
                    EffectKind::Call => continue,
                };
                let target = format!(
                    "{}:{name}",
                    if kind == GraphNodeKind::Event {
                        "event"
                    } else {
                        "resource"
                    }
                );
                graph.nodes.entry(target.clone()).or_insert(GraphNode {
                    id: target.clone(),
                    kind,
                    attributes: BTreeMap::new(),
                });
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: target,
                    relation,
                });
            }
        }
    }
    let mut previous = BTreeMap::<String, String>::new();
    for event in events {
        let observation = format!(
            "observation:{}:{}:{}",
            event.trace_id, event.span_id, event.sequence
        );
        let operation = format!("operation:{}", event.operation);
        graph.nodes.entry(operation.clone()).or_insert(GraphNode {
            id: operation.clone(),
            kind: GraphNodeKind::Operation,
            attributes: BTreeMap::new(),
        });
        graph.nodes.insert(
            observation.clone(),
            GraphNode {
                id: observation.clone(),
                kind: GraphNodeKind::RuntimeObservation,
                attributes: BTreeMap::new(),
            },
        );
        graph.edges.push(GraphEdge {
            from: operation.clone(),
            to: observation.clone(),
            relation: GraphRelation::ObservedAs,
        });
        if let Some(prior) = previous.insert(event.trace_id.clone(), observation.clone()) {
            graph.edges.push(GraphEdge {
                from: prior,
                to: observation.clone(),
                relation: GraphRelation::HappensBefore,
            });
        }
        if let BackendEventKind::Effect {
            effect,
            resource,
            event: emitted,
            ..
        } = &event.event
        {
            let (target, kind, relation) = if *effect == EffectKind::Emit {
                (
                    emitted.as_deref().unwrap_or("unknown"),
                    GraphNodeKind::Event,
                    GraphRelation::Emits,
                )
            } else {
                let relation = match effect {
                    EffectKind::Read => GraphRelation::Reads,
                    EffectKind::Write => GraphRelation::Writes,
                    EffectKind::Delete => GraphRelation::Deletes,
                    EffectKind::Call => GraphRelation::Calls,
                    EffectKind::Emit => GraphRelation::Emits,
                };
                (
                    resource.as_deref().unwrap_or("unknown"),
                    GraphNodeKind::Resource,
                    relation,
                )
            };
            let target = format!(
                "{}:{target}",
                match kind {
                    GraphNodeKind::Event => "event",
                    _ => "resource",
                }
            );
            graph.nodes.entry(target.clone()).or_insert(GraphNode {
                id: target.clone(),
                kind,
                attributes: BTreeMap::new(),
            });
            graph.edges.push(GraphEdge {
                from: observation,
                to: target,
                relation,
            });
        }
    }
    graph
}
