//! Portable temporal contracts over normalized Reproit observations.

use crate::observation::Observation;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

    pub fn reproduces(&self, observations: &[Observation]) -> bool {
        evaluate_all(&self.contracts, observations)
            .iter()
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
    contracts
        .iter()
        .flat_map(|contract| contract.evaluate(trace))
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

pub fn write_evidence(
    path: &std::path::Path,
    contracts: &[ContractSpec],
    observations: &[Observation],
    violations: &[ContractViolation],
) -> std::io::Result<()> {
    if violations.is_empty() {
        return Ok(());
    }
    let payload = serde_json::json!({
        "version": 1,
        "contracts": contracts,
        "observations": observations,
        "violations": violations,
    });
    std::fs::write(path, serde_json::to_vec_pretty(&payload)?)
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
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
}
