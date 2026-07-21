//! Read-only, non-authoritative projections of the verified app map.

use super::{action_str, Visits};
use crate::domain::appmap::{AppMap, Reversibility};
use crate::domain::authority::ContractAuthority;
use crate::domain::contracts::{ContractScope, ContractSpec, Eventually, Formula, Predicate};
use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_MODEL_STATES: usize = 20_000;
const MAX_MODEL_TRANSITIONS: usize = 100_000;
const MAX_MODEL_ACTIONS: usize = 200_000;
const MAX_CONTRACT_DRAFTS: usize = 4_096;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShadowModel {
    classification: &'static str,
    revision: u64,
    states: Vec<String>,
    transitions: Vec<ModelTransition>,
    unknown_actions: Vec<UnknownAction>,
}

#[derive(Serialize)]
struct ModelTransition {
    from: String,
    action: String,
    to: String,
}

#[derive(Serialize)]
struct UnknownAction {
    state: String,
    action: String,
}

pub(crate) fn shadow_model(map: &AppMap) -> Result<ShadowModel> {
    if map.states.len() > MAX_MODEL_STATES || map.transitions.len() > MAX_MODEL_TRANSITIONS {
        anyhow::bail!("app model exceeds the bounded shadow-model projection");
    }
    let known = map
        .transitions
        .iter()
        .map(|transition| (transition.from.as_str(), action_str(&transition.action)))
        .collect::<BTreeSet<_>>();
    let mut unknown_actions = Vec::new();
    for (state_id, state) in &map.states {
        for element in &state.elements {
            let action = format!("tap:{}", element.sel);
            if !known.contains(&(state_id.as_str(), action.clone())) {
                unknown_actions.push(UnknownAction {
                    state: state_id.clone(),
                    action,
                });
                if unknown_actions.len() > MAX_MODEL_ACTIONS {
                    anyhow::bail!("app model exceeds the bounded unknown-action projection");
                }
            }
        }
    }
    unknown_actions
        .sort_by(|left, right| (&left.state, &left.action).cmp(&(&right.state, &right.action)));
    Ok(ShadowModel {
        classification: "incomplete-non-authoritative",
        revision: map.revision,
        states: map.states.keys().cloned().collect(),
        transitions: map
            .transitions
            .iter()
            .map(|transition| ModelTransition {
                from: transition.from.clone(),
                action: action_str(&transition.action),
                to: transition.to.clone(),
            })
            .collect(),
        unknown_actions,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BudgetAdvice {
    classification: &'static str,
    states: usize,
    transitions: usize,
    traversed_transitions: usize,
    saturation_percent: u8,
    recommended_actions: u32,
    recommendation: &'static str,
}

pub(crate) fn budget_advice(map: &AppMap, visits: &Visits, base: u32) -> BudgetAdvice {
    let traversed = map
        .transitions
        .iter()
        .filter(|transition| {
            let Some(signature) = map
                .states
                .get(&transition.from)
                .and_then(|state| state.signature.semantics_hash.as_deref())
            else {
                return false;
            };
            let key = format!("{signature}|{}", action_str(&transition.action));
            visits.edge_counts.get(&key).copied().unwrap_or(0) > 0
        })
        .count();
    let total = map.transitions.len();
    let saturation = traversed
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100) as u8;
    let (multiplier, recommendation) = match saturation {
        0..=39 => (4, "continue: the observed graph is still sparse"),
        40..=74 => (2, "continue: prioritize rare transitions and frontiers"),
        75..=94 => (1, "focus: spend budget on remaining frontiers"),
        _ => (
            0,
            "reallocate: this graph is saturated under current actions",
        ),
    };
    BudgetAdvice {
        classification: "guidance-only",
        states: map.states.len(),
        transitions: total,
        traversed_transitions: traversed,
        saturation_percent: saturation,
        recommended_actions: base.saturating_mul(multiplier).min(base.saturating_mul(4)),
        recommendation,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContractDrafts {
    classification: &'static str,
    source_revision: u64,
    contracts: Vec<ContractSpec>,
}

pub(crate) fn contract_drafts(map: &AppMap) -> Result<ContractDrafts> {
    let contracts = map
        .transitions
        .iter()
        .filter(|transition| matches!(transition.reversibility, Reversibility::VerifiedReversible))
        .map(|transition| {
            transition_contract(
                transition.from.as_str(),
                transition.to.as_str(),
                &action_str(&transition.action),
            )
        })
        .take(MAX_CONTRACT_DRAFTS + 1)
        .collect::<Vec<_>>();
    if contracts.len() > MAX_CONTRACT_DRAFTS {
        anyhow::bail!("verified transitions exceed the {MAX_CONTRACT_DRAFTS} draft limit");
    }
    Ok(ContractDrafts {
        classification: "draft-non-authoritative",
        source_revision: map.revision,
        contracts,
    })
}

fn transition_contract(from: &str, to: &str, action: &str) -> ContractSpec {
    let digest = Sha256::digest(format!("{from}\n{action}\n{to}").as_bytes());
    let suffix = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ContractSpec {
        id: format!("draft-transition-{suffix}"),
        authority: ContractAuthority::Suggested,
        scope: ContractScope::Trace,
        when: Some(Predicate {
            state: Some(from.to_string()),
            action: Some(action.to_string()),
            ..empty_predicate()
        }),
        must: Formula::Eventually {
            eventually: Eventually {
                condition: Box::new(Formula::Is {
                    is: Box::new(Predicate {
                        state: Some(to.to_string()),
                        ..empty_predicate()
                    }),
                }),
                within_steps: Some(1),
            },
        },
    }
}

fn empty_predicate() -> Predicate {
    Predicate {
        actor: None,
        state: None,
        route: None,
        action: None,
        text: None,
        oracle: None,
        network_status: None,
        response_shape: None,
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::appmap::{Action, State, StateElement, StateSignature, Transition};
    use std::collections::BTreeMap;

    fn map() -> AppMap {
        let state = |hash: &str, selector: &str| State {
            name: None,
            description: String::new(),
            signature: StateSignature {
                screenshot_phash: None,
                semantics_hash: Some(hash.into()),
                route: None,
            },
            elements: vec![StateElement {
                sel: selector.into(),
                ..Default::default()
            }],
            texts: vec![],
            parameters: vec![],
            operability_gaps: Default::default(),
        };
        AppMap {
            app: "fixture".into(),
            schema_version: 3,
            revision: 7,
            states: BTreeMap::from([
                ("home".into(), state("h", "key:open")),
                ("details".into(), state("d", "key:close")),
            ]),
            transitions: vec![Transition {
                from: "home".into(),
                to: "details".into(),
                action: Action::Tap {
                    finder: "key:open".into(),
                },
                guards: vec![],
                reversibility: Reversibility::VerifiedReversible,
                expected: None,
            }],
            invariants: vec![],
            interrupts: vec![],
        }
    }

    #[test]
    fn shadow_model_marks_unknown_actions_as_non_authoritative() {
        let model = shadow_model(&map()).unwrap();
        assert_eq!(model.classification, "incomplete-non-authoritative");
        assert_eq!(model.unknown_actions.len(), 1);
        assert_eq!(model.unknown_actions[0].state, "details");
    }

    #[test]
    fn budget_advice_never_becomes_a_verdict() {
        let advice = budget_advice(&map(), &Visits::default(), 100);
        assert_eq!(advice.classification, "guidance-only");
        assert_eq!(advice.recommended_actions, 400);
    }

    #[test]
    fn contract_suggestions_remain_explicit_drafts() {
        let drafts = contract_drafts(&map()).unwrap();
        assert_eq!(drafts.classification, "draft-non-authoritative");
        assert_eq!(drafts.contracts.len(), 1);
        assert_eq!(drafts.contracts[0].authority, ContractAuthority::Suggested);
    }
}
