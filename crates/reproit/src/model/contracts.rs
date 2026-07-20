//! Portable temporal contracts over normalized Reproit observations.

use crate::model::observation::Observation;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContractSpec {
    pub id: String,
    #[serde(default)]
    pub scope: ContractScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Predicate>,
    pub must: Formula,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ContractScope {
    State,
    #[default]
    Trace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Predicate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<CountPredicate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CountPredicate {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_least: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_most: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Formula {
    Is { is: Box<Predicate> },
    Always { always: Box<Formula> },
    Eventually { eventually: Eventually },
    Next { next: Box<Formula> },
    Implies { implies: Implication },
    All { all: Vec<Formula> },
    Any { any: Vec<Formula> },
    Not { not: Box<Formula> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Eventually {
    pub condition: Box<Formula>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within_steps: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Implication {
    pub if_condition: Box<Formula>,
    pub then: Box<Formula>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContractViolation {
    pub contract_id: String,
    pub contract_hash: String,
    pub fingerprint: String,
    pub reason: String,
    pub trigger_index: usize,
    pub boundary_index: usize,
    pub scope: ContractScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContractEvaluation {
    pub contract_id: String,
    pub contract_hash: String,
    pub status: crate::model::evidence::EvidenceStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_codes: Vec<reproit_protocol::ReasonCode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<ContractViolation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FrozenContractGuard {
    pub contracts: Vec<ContractSpec>,
    pub fingerprints: BTreeSet<String>,
}

impl FrozenContractGuard {
    pub fn from_findings(
        contracts: &[ContractSpec],
        findings: &[serde_json::Value],
    ) -> Option<Self> {
        let fingerprints = findings
            .iter()
            .filter(|finding| {
                finding.get("oracle").and_then(serde_json::Value::as_str) == Some("contract")
            })
            .filter_map(|finding| {
                finding
                    .get("fingerprint")
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        if fingerprints.is_empty() {
            return None;
        }
        let ids = findings
            .iter()
            .filter(|finding| {
                finding.get("oracle").and_then(serde_json::Value::as_str) == Some("contract")
            })
            .filter_map(|finding| finding.get("invariant").and_then(serde_json::Value::as_str))
            .collect::<BTreeSet<_>>();
        let contracts = contracts
            .iter()
            .filter(|contract| ids.contains(contract.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        (!contracts.is_empty()).then_some(Self {
            contracts,
            fingerprints,
        })
    }

    pub(crate) fn reproduces(
        &self,
        observations: &[Observation],
        defects: &[crate::model::runner::StreamDefect],
    ) -> bool {
        evaluate_stream(&self.contracts, observations, defects)
            .iter()
            .flat_map(|evaluation| &evaluation.violations)
            .any(|violation| self.fingerprints.contains(&violation.fingerprint))
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }

    pub fn load(path: &std::path::Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

impl ContractSpec {
    pub fn stable_hash(&self) -> String {
        hash_bytes(&serde_json::to_vec(self).expect("contract serialization cannot fail"))[..16]
            .to_string()
    }

    pub fn evaluate(&self, trace: &[Observation]) -> Vec<ContractViolation> {
        if trace.is_empty() {
            return Vec::new();
        }
        let triggers = match (&self.when, self.scope) {
            (None, ContractScope::State) => (0..trace.len()).collect::<Vec<_>>(),
            (Some(predicate), _) => trace
                .iter()
                .enumerate()
                .filter_map(|(index, observation)| predicate.matches(observation).then_some(index))
                .collect::<Vec<_>>(),
            (None, ContractScope::Trace) => vec![0],
        };
        let contract_hash = self.stable_hash();
        triggers
            .into_iter()
            .filter_map(|trigger_index| {
                evaluate_formula(&self.must, trace, trigger_index)
                    .err()
                    .map(|(boundary_index, reason)| {
                        let actor = trace.get(trigger_index).and_then(|o| o.actor.clone());
                        // Trace positions are evidence, not identity. Shrinking
                        // is allowed to move the trigger earlier while it must
                        // preserve the contract, failed formula, and actor.
                        let material = format!(
                            "{}:{}:{}:{}",
                            self.id,
                            contract_hash,
                            reason,
                            actor.as_deref().unwrap_or("")
                        );
                        ContractViolation {
                            contract_id: self.id.clone(),
                            contract_hash: contract_hash.clone(),
                            fingerprint: hash_bytes(material.as_bytes())[..20].to_string(),
                            reason,
                            trigger_index,
                            boundary_index,
                            scope: self.scope,
                            actor,
                        }
                    })
            })
            .collect()
    }

    pub fn action_hints(&self) -> BTreeSet<String> {
        let mut actions = BTreeSet::new();
        if let Some(action) = self
            .when
            .as_ref()
            .and_then(|predicate| predicate.action.as_ref())
        {
            actions.insert(action.clone());
        }
        collect_action_hints(&self.must, &mut actions);
        actions
    }
}

impl Predicate {
    fn matches(&self, observation: &Observation) -> bool {
        self.actor
            .as_ref()
            .is_none_or(|v| observation.actor.as_ref() == Some(v))
            && self
                .state
                .as_ref()
                .is_none_or(|v| observation.state.as_ref() == Some(v))
            && self
                .route
                .as_ref()
                .is_none_or(|v| observation.route.as_ref() == Some(v))
            && self
                .action
                .as_ref()
                .is_none_or(|v| observation.action.as_ref() == Some(v))
            && self
                .text
                .as_ref()
                .is_none_or(|v| observation.visible_text.iter().any(|t| t == v))
            && self
                .oracle
                .as_ref()
                .is_none_or(|v| observation.oracle_signals.iter().any(|s| s == v))
            && self
                .network_status
                .is_none_or(|status| observation.network_statuses.contains(&status))
            && self.response_shape.as_ref().is_none_or(|shape| {
                observation
                    .response_shapes
                    .iter()
                    .any(|value| value == shape)
            })
            && self
                .count
                .as_ref()
                .is_none_or(|count| count.matches(observation))
    }
}

impl CountPredicate {
    fn matches(&self, observation: &Observation) -> bool {
        let value = observation.counts.get(&self.key).copied().unwrap_or(0);
        self.equals.is_none_or(|limit| value == limit)
            && self.at_least.is_none_or(|limit| value >= limit)
            && self.at_most.is_none_or(|limit| value <= limit)
    }
}

fn evaluate_formula(
    formula: &Formula,
    trace: &[Observation],
    index: usize,
) -> Result<(), (usize, String)> {
    let current = trace.get(index).ok_or((index, "trace ended".to_string()))?;
    match formula {
        Formula::Is { is: predicate } => predicate
            .matches(current)
            .then_some(())
            .ok_or((index, "predicate did not match".to_string())),
        Formula::Always { always: inner } => {
            for cursor in index..trace.len() {
                evaluate_formula(inner, trace, cursor)?;
            }
            Ok(())
        }
        Formula::Eventually { eventually } => {
            let end = eventually
                .within_steps
                .map(|steps| index.saturating_add(steps as usize))
                .unwrap_or(trace.len().saturating_sub(1))
                .min(trace.len().saturating_sub(1));
            for cursor in index..=end {
                if evaluate_formula(&eventually.condition, trace, cursor).is_ok() {
                    return Ok(());
                }
            }
            Err((end, "eventual condition was not observed".to_string()))
        }
        Formula::Next { next: inner } => evaluate_formula(inner, trace, index + 1),
        Formula::Implies {
            implies: implication,
        } => {
            if evaluate_formula(&implication.if_condition, trace, index).is_ok() {
                evaluate_formula(&implication.then, trace, index)
            } else {
                Ok(())
            }
        }
        Formula::All { all: items } => {
            for item in items {
                evaluate_formula(item, trace, index)?;
            }
            Ok(())
        }
        Formula::Any { any: items } => {
            if items
                .iter()
                .any(|item| evaluate_formula(item, trace, index).is_ok())
            {
                Ok(())
            } else {
                Err((index, "no alternative matched".to_string()))
            }
        }
        Formula::Not { not: inner } => {
            if evaluate_formula(inner, trace, index).is_err() {
                Ok(())
            } else {
                Err((index, "negated condition matched".to_string()))
            }
        }
    }
}

pub fn evaluate_all(contracts: &[ContractSpec], trace: &[Observation]) -> Vec<ContractViolation> {
    evaluate_stream(contracts, trace, &[])
        .iter()
        .flat_map(|evaluation| evaluation.violations.iter().cloned())
        .collect()
}

pub(crate) fn evaluate_stream(
    contracts: &[ContractSpec],
    trace: &[Observation],
    defects: &[crate::model::runner::StreamDefect],
) -> Vec<ContractEvaluation> {
    contracts
        .iter()
        .map(|contract| {
            let contract_hash = contract.stable_hash();
            let reason_codes = defects
                .iter()
                .filter(|defect| defect.scope.affects_contract(&contract_hash))
                .map(|defect| defect.reason)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if !reason_codes.is_empty() {
                return ContractEvaluation {
                    contract_id: contract.id.clone(),
                    contract_hash,
                    status: crate::model::evidence::EvidenceStatus::Abstain,
                    reason_codes,
                    violations: Vec::new(),
                };
            }
            if trace.is_empty() {
                return ContractEvaluation {
                    contract_id: contract.id.clone(),
                    contract_hash,
                    status: crate::model::evidence::EvidenceStatus::Abstain,
                    reason_codes: vec![reproit_protocol::ReasonCode::NoObservations],
                    violations: Vec::new(),
                };
            }
            let violations = contract.evaluate(trace);
            let status = if violations.is_empty() {
                crate::model::evidence::EvidenceStatus::Satisfied
            } else {
                crate::model::evidence::EvidenceStatus::Violation
            };
            ContractEvaluation {
                contract_id: contract.id.clone(),
                contract_hash,
                status,
                reason_codes: Vec::new(),
                violations,
            }
        })
        .collect()
}

pub fn finding(violation: &ContractViolation) -> serde_json::Value {
    serde_json::json!({
        "oracle": "contract",
        "invariant": violation.contract_id,
        "kind": "temporal-contract",
        "message": violation.reason,
        "contract_hash": violation.contract_hash,
        "scope": violation.scope,
        "fingerprint": violation.fingerprint,
        "trigger": violation.fingerprint,
        "trigger_index": violation.trigger_index,
        "boundary_index": violation.boundary_index,
        "actor": violation.actor,
        "frames": [format!("contract:{}", violation.contract_hash)],
    })
}

pub fn action_hints(contracts: &[ContractSpec]) -> Vec<String> {
    contracts
        .iter()
        .flat_map(ContractSpec::action_hints)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn write_evidence(
    path: &std::path::Path,
    contracts: &[ContractSpec],
    observations: &[Observation],
    evaluations: &[ContractEvaluation],
    defects: &[crate::model::runner::StreamDefect],
) -> std::io::Result<()> {
    if contracts.is_empty() {
        return Ok(());
    }
    let graph = contract_evidence_graph(contracts, observations, evaluations, defects)?;
    std::fs::write(path, serde_json::to_vec_pretty(&graph)?)
}

fn contract_evidence_graph(
    contracts: &[ContractSpec],
    observations: &[Observation],
    evaluations: &[ContractEvaluation],
    defects: &[crate::model::runner::StreamDefect],
) -> std::io::Result<reproit_protocol::EvidenceGraph> {
    let violations = evaluations
        .iter()
        .flat_map(|evaluation| &evaluation.violations)
        .collect::<Vec<_>>();
    let normalized_payload = serde_json::json!({
        "contracts": contracts,
        "observations": observations,
        "streamDefects": defects,
    });
    let normalized = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::NormalizedTrace,
        vec![],
        normalized_payload,
    )
    .map_err(protocol_io)?;
    let evaluation = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::Evaluation,
        vec![normalized.id.clone()],
        serde_json::json!({
            "outcomes": evaluations,
            "violations": violations,
        }),
    )
    .map_err(protocol_io)?;
    let run_hash =
        hash_bytes(&serde_json::to_vec(observations).expect("normalized observations serialize"));
    let graph = reproit_protocol::EvidenceGraph {
        run_id: format!("contract-{}", &run_hash[..16]),
        root: evaluation.id.clone(),
        nodes: vec![normalized, evaluation],
    };
    graph.validate().map_err(protocol_io)?;
    Ok(graph)
}

fn protocol_io(error: reproit_protocol::ProtocolError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
}

fn collect_action_hints(formula: &Formula, actions: &mut BTreeSet<String>) {
    match formula {
        Formula::Is { is } => {
            if let Some(action) = &is.action {
                actions.insert(action.clone());
            }
        }
        Formula::Always { always }
        | Formula::Next { next: always }
        | Formula::Not { not: always } => collect_action_hints(always, actions),
        Formula::Eventually { eventually } => collect_action_hints(&eventually.condition, actions),
        Formula::Implies { implies } => {
            collect_action_hints(&implies.if_condition, actions);
            collect_action_hints(&implies.then, actions);
        }
        Formula::All { all } | Formula::Any { any: all } => {
            for item in all {
                collect_action_hints(item, actions);
            }
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    crate::infra::sha256_hex(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(sequence: u64, actor: &str, state: &str) -> Observation {
        Observation {
            sequence,
            elapsed_ms: sequence,
            actor: Some(actor.to_string()),
            state: Some(state.to_string()),
            ..Observation::default()
        }
    }

    fn is_state(state: &str) -> Formula {
        Formula::Is {
            is: Box::new(Predicate {
                actor: None,
                state: Some(state.to_string()),
                route: None,
                action: None,
                text: None,
                oracle: None,
                network_status: None,
                response_shape: None,
                count: None,
            }),
        }
    }

    #[test]
    fn eventual_contract_passes_across_actors() {
        let contract = ContractSpec {
            id: "message-visible".to_string(),
            scope: ContractScope::Trace,
            when: Some(Predicate {
                actor: Some("alice".to_string()),
                state: Some("sent".to_string()),
                route: None,
                action: None,
                text: None,
                oracle: None,
                network_status: None,
                response_shape: None,
                count: None,
            }),
            must: Formula::Eventually {
                eventually: Eventually {
                    condition: Box::new(Formula::All {
                        all: vec![
                            Formula::Is {
                                is: Box::new(Predicate {
                                    actor: Some("bob".to_string()),
                                    state: None,
                                    route: None,
                                    action: None,
                                    text: None,
                                    oracle: None,
                                    network_status: None,
                                    response_shape: None,
                                    count: None,
                                }),
                            },
                            is_state("received"),
                        ],
                    }),
                    within_steps: Some(2),
                },
            },
        };
        let trace = vec![
            observation(1, "alice", "sent"),
            observation(2, "alice", "waiting"),
            observation(3, "bob", "received"),
        ];
        assert!(contract.evaluate(&trace).is_empty());
    }

    #[test]
    fn violation_identity_is_stable() {
        let contract = ContractSpec {
            id: "must-finish".to_string(),
            scope: ContractScope::Trace,
            when: None,
            must: Formula::Eventually {
                eventually: Eventually {
                    condition: Box::new(is_state("done")),
                    within_steps: Some(1),
                },
            },
        };
        let trace = vec![observation(1, "alice", "start")];
        assert_eq!(contract.evaluate(&trace), contract.evaluate(&trace));
        assert_eq!(contract.evaluate(&trace).len(), 1);
    }

    #[test]
    fn shrink_may_move_positions_without_changing_violation_identity() {
        let contract = ContractSpec {
            id: "must-finish".to_string(),
            scope: ContractScope::Trace,
            when: Some(Predicate {
                actor: Some("alice".to_string()),
                state: Some("start".to_string()),
                route: None,
                action: None,
                text: None,
                oracle: None,
                network_status: None,
                response_shape: None,
                count: None,
            }),
            must: Formula::Eventually {
                eventually: Eventually {
                    condition: Box::new(is_state("done")),
                    within_steps: Some(1),
                },
            },
        };
        let short = vec![observation(1, "alice", "start")];
        let long = vec![
            observation(1, "bob", "idle"),
            observation(2, "alice", "start"),
        ];
        assert_eq!(
            contract.evaluate(&short)[0].fingerprint,
            contract.evaluate(&long)[0].fingerprint
        );
    }

    #[test]
    fn action_hints_are_sorted_and_deduplicated() {
        let contract = ContractSpec {
            id: "send".to_string(),
            scope: ContractScope::Trace,
            when: Some(Predicate {
                actor: None,
                state: None,
                route: None,
                action: Some("tap:key:testid:send".to_string()),
                text: None,
                oracle: None,
                network_status: None,
                response_shape: None,
                count: None,
            }),
            must: Formula::Next {
                next: Box::new(Formula::Is {
                    is: Box::new(Predicate {
                        actor: None,
                        state: None,
                        route: None,
                        action: Some("tap:key:testid:send".to_string()),
                        text: None,
                        oracle: None,
                        network_status: None,
                        response_shape: None,
                        count: None,
                    }),
                }),
            },
        };
        assert_eq!(
            action_hints(&[contract]),
            vec!["tap:key:testid:send".to_string()]
        );
    }

    #[test]
    fn yaml_is_structural_and_language_independent() {
        let yaml = r#"
id: no-crash
must:
  always:
    not:
      is:
        oracle: crash
"#;
        let contract: ContractSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(contract.id, "no-crash");
    }

    #[test]
    fn unscoped_stream_defect_makes_every_contract_abstain() {
        let contracts = ["first", "second"].map(|id| ContractSpec {
            id: id.to_string(),
            scope: ContractScope::Trace,
            when: None,
            must: is_state("expected"),
        });
        let defects = [crate::model::runner::StreamDefect {
            reason: crate::model::runner::StreamDefectReason::FrameTooLarge,
            scope: crate::model::runner::EvidenceScope::Contract {
                contract_hash: None,
            },
            sequence: None,
        }];
        let outcomes = evaluate_stream(
            &contracts,
            &[observation(1, "actor", "unexpected")],
            &defects,
        );
        assert!(outcomes.iter().all(|outcome| {
            outcome.status == crate::model::evidence::EvidenceStatus::Abstain
                && outcome.reason_codes == [reproit_protocol::ReasonCode::FrameTooLarge]
                && outcome.violations.is_empty()
        }));
    }

    #[test]
    fn attributed_stream_defect_only_abstains_the_matching_contract() {
        let contracts = ["first", "second"].map(|id| ContractSpec {
            id: id.to_string(),
            scope: ContractScope::Trace,
            when: None,
            must: is_state("expected"),
        });
        let defects = [crate::model::runner::StreamDefect {
            reason: crate::model::runner::StreamDefectReason::FrameTooLarge,
            scope: crate::model::runner::EvidenceScope::Contract {
                contract_hash: Some(contracts[0].stable_hash()),
            },
            sequence: None,
        }];
        let outcomes = evaluate_stream(
            &contracts,
            &[observation(1, "actor", "unexpected")],
            &defects,
        );
        assert_eq!(
            outcomes[0].status,
            crate::model::evidence::EvidenceStatus::Abstain
        );
        assert!(outcomes[0].violations.is_empty());
        assert_eq!(
            outcomes[1].status,
            crate::model::evidence::EvidenceStatus::Violation
        );
        assert_eq!(outcomes[1].violations.len(), 1);
    }

    #[test]
    fn evidence_artifact_persists_tri_state_outcome_and_stream_defect() {
        let contracts = [ContractSpec {
            id: "bounded".to_string(),
            scope: ContractScope::Trace,
            when: None,
            must: is_state("expected"),
        }];
        let defects = [crate::model::runner::StreamDefect {
            reason: crate::model::runner::StreamDefectReason::FrameTooLarge,
            scope: crate::model::runner::EvidenceScope::Contract {
                contract_hash: None,
            },
            sequence: None,
        }];
        let outcomes = evaluate_stream(&contracts, &[], &defects);
        let graph = contract_evidence_graph(&contracts, &[], &outcomes, &defects).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(
            graph.nodes[0].kind,
            reproit_protocol::ArtifactKind::NormalizedTrace
        );
        assert_eq!(
            graph.nodes[0].payload["streamDefects"][0]["reason"],
            "frame-too-large"
        );
        assert_eq!(
            graph.nodes[1].kind,
            reproit_protocol::ArtifactKind::Evaluation
        );
        assert_eq!(graph.nodes[1].payload["outcomes"][0]["status"], "abstain");
        assert_eq!(
            graph.nodes[1].payload["outcomes"][0]["reasonCodes"][0],
            "frame-too-large"
        );
        assert_eq!(graph.nodes[1].payload["violations"], serde_json::json!([]));
    }
}
