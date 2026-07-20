//! Evaluation of user-declared state, edge, and graph predicates.

use super::finding::finding;
use super::graph::screen_hint;
use super::Observations;
use crate::adapters::config::{CustomInvariant, InvariantScope};
use serde_json::Value;

/// Evaluate one custom invariant against the run.
pub(super) fn eval_custom(obs: &Observations, c: &CustomInvariant) -> Vec<Value> {
    let mut out = Vec::new();
    match &c.scope {
        InvariantScope::State => {
            for (sig, labels) in &obs.obs.states {
                // labels-match: every state's labels must contain a match.
                if let Some(re) = &c.labels_match {
                    if !labels.iter().any(|l| re.is_match(l)) {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "state {sig} violates {}: no label matches /{}/{}",
                                c.id,
                                re.as_str(),
                                screen_hint(labels)
                            ),
                            Some(sig),
                        ));
                    }
                }
                // labels-absent: no label may match.
                if let Some(re) = &c.labels_absent {
                    if let Some(hit) = labels.iter().find(|l| re.is_match(l)) {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "state {sig} violates {}: label {hit:?} matches forbidden /{}/",
                                c.id,
                                re.as_str()
                            ),
                            Some(sig),
                        ));
                    }
                }
            }
        }
        InvariantScope::Edge => {
            // Custom edge invariant: forbid an action (by regex) anywhere, e.g.
            // "no destructive tap reachable". Start simple: a forbidden-action
            // regex flags any edge whose action string matches.
            if let Some(re) = &c.action_absent {
                for (from, action, to) in &obs.obs.edges {
                    if re.is_match(action) {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "edge {from} --{action}--> {to} violates {}: forbidden action /{}/",
                                c.id,
                                re.as_str()
                            ),
                            Some(from),
                        ));
                    }
                }
            }
        }
        InvariantScope::Graph => {
            // Custom graph invariant: a label that MUST be reachable.
            if let Some(re) = &c.must_reach {
                let reached = obs
                    .obs
                    .states
                    .values()
                    .any(|labels| labels.iter().any(|l| re.is_match(l)));
                if !reached {
                    out.push(finding(
                        &c.id,
                        "INVARIANT",
                        format!(
                            "invariant {} violated: no observed state has a label matching \
                             required /{}/",
                            c.id,
                            re.as_str()
                        ),
                        None,
                    ));
                }
            }
        }
    }
    out
}
