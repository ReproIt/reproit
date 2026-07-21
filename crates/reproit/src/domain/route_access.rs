//! Declarative access policy for concrete web document routes.

use crate::adapters::config::Account;
use crate::domain::evidence::EvidenceStatus;
use anyhow::{bail, Result};
use reproit_protocol::ReasonCode;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_CONTRACTS: usize = 64;
const MAX_CELLS: usize = 128;
const MAX_ROUTE_BYTES: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteAccessSpec {
    pub route: String,
    pub access: BTreeMap<String, RouteAccessExpectation>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RouteAccessExpectation {
    Decision(RouteAccessDecision),
    Redirect { redirect: String },
    Status { status: u16 },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RouteAccessDecision {
    Allow,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteAccessObservation {
    pub requested: String,
    pub final_route: String,
    pub status: Option<u16>,
    pub settled: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RouteAccessEvaluation {
    pub route: String,
    pub principal: String,
    pub expected: RouteAccessExpectation,
    pub status: EvidenceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<ReasonCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<RouteAccessObservation>,
    pub fingerprint: String,
}

pub fn validate(specs: &[RouteAccessSpec], accounts: &[Account]) -> Result<()> {
    if specs.len() > MAX_CONTRACTS {
        bail!(
            "routeAccess has {} routes; maximum is {MAX_CONTRACTS}",
            specs.len()
        );
    }
    let account_names = accounts
        .iter()
        .map(|account| account.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut routes = BTreeSet::new();
    let mut cells = 0usize;
    for spec in specs {
        validate_route_path(&spec.route, "routeAccess.route")?;
        if !routes.insert(spec.route.as_str()) {
            bail!("routeAccess repeats route {:?}", spec.route);
        }
        if spec.access.is_empty() {
            bail!("routeAccess route {:?} has no access entries", spec.route);
        }
        cells = cells.saturating_add(spec.access.len());
        if cells > MAX_CELLS {
            bail!("routeAccess has more than {MAX_CELLS} route/principal entries");
        }
        for (principal, expectation) in &spec.access {
            if principal != "anonymous" && !account_names.contains(principal.as_str()) {
                bail!("routeAccess principal {principal:?} is not anonymous or an auth account");
            }
            match expectation {
                RouteAccessExpectation::Decision(RouteAccessDecision::Allow) => {}
                RouteAccessExpectation::Redirect { redirect } => {
                    validate_route_path(redirect, "routeAccess redirect")?;
                }
                RouteAccessExpectation::Status { status } if (100..=599).contains(status) => {}
                RouteAccessExpectation::Status { status } => {
                    bail!("routeAccess status {status} is outside 100..599");
                }
            }
        }
    }
    Ok(())
}

pub fn validate_route_path(route: &str, field: &str) -> Result<()> {
    if route.is_empty()
        || route.len() > MAX_ROUTE_BYTES
        || !route.starts_with('/')
        || route.starts_with("//")
        || route.contains(['?', '#'])
        || route.chars().any(char::is_whitespace)
    {
        bail!(
            "{field} must be a concrete same-origin path without query or fragment, got {route:?}"
        );
    }
    Ok(())
}

pub fn evaluate(
    route: &str,
    principal: &str,
    expected: &RouteAccessExpectation,
    observation: Option<RouteAccessObservation>,
    authority_available: bool,
) -> RouteAccessEvaluation {
    let fingerprint = fingerprint(route, principal, expected);
    if !authority_available {
        return abstain(
            route,
            principal,
            expected,
            observation,
            fingerprint,
            ReasonCode::AuthorityUnavailable,
            "principal authentication could not be proven",
        );
    }
    let Some(observation) = observation else {
        return abstain(
            route,
            principal,
            expected,
            None,
            fingerprint,
            ReasonCode::NoObservations,
            "the runner emitted no bounded route observation",
        );
    };
    if !observation.settled || observation.requested != route || observation.status.is_none() {
        return abstain(
            route,
            principal,
            expected,
            Some(observation),
            fingerprint,
            ReasonCode::IncompleteStream,
            "route navigation did not settle with an attributed document response",
        );
    }

    let observed_status = observation.status.expect("checked above");
    let satisfied = match expected {
        RouteAccessExpectation::Decision(RouteAccessDecision::Allow) => {
            observation.final_route == route && (200..=299).contains(&observed_status)
        }
        RouteAccessExpectation::Redirect { redirect } => observation.final_route == *redirect,
        RouteAccessExpectation::Status { status } => observed_status == *status,
    };
    let (status, reason) = if satisfied {
        (EvidenceStatus::Satisfied, None)
    } else {
        (
            EvidenceStatus::Violation,
            Some(format!(
                "expected {}, observed route {} with HTTP {}",
                expectation_label(expected),
                observation.final_route,
                observed_status
            )),
        )
    };
    RouteAccessEvaluation {
        route: route.to_string(),
        principal: principal.to_string(),
        expected: expected.clone(),
        status,
        reason,
        reason_code: None,
        observation: Some(observation),
        fingerprint,
    }
}

pub fn abstain_for_defect(
    route: &str,
    principal: &str,
    expected: &RouteAccessExpectation,
    reason_code: ReasonCode,
    reason: &str,
) -> RouteAccessEvaluation {
    abstain(
        route,
        principal,
        expected,
        None,
        fingerprint(route, principal, expected),
        reason_code,
        reason,
    )
}

fn abstain(
    route: &str,
    principal: &str,
    expected: &RouteAccessExpectation,
    observation: Option<RouteAccessObservation>,
    fingerprint: String,
    reason_code: ReasonCode,
    reason: &str,
) -> RouteAccessEvaluation {
    RouteAccessEvaluation {
        route: route.to_string(),
        principal: principal.to_string(),
        expected: expected.clone(),
        status: EvidenceStatus::Abstain,
        reason: Some(reason.to_string()),
        reason_code: Some(reason_code),
        observation,
        fingerprint,
    }
}

fn expectation_label(expectation: &RouteAccessExpectation) -> String {
    match expectation {
        RouteAccessExpectation::Decision(RouteAccessDecision::Allow) => {
            "the requested route to remain accessible".to_string()
        }
        RouteAccessExpectation::Redirect { redirect } => format!("redirect to {redirect}"),
        RouteAccessExpectation::Status { status } => format!("HTTP {status}"),
    }
}

fn fingerprint(route: &str, principal: &str, expectation: &RouteAccessExpectation) -> String {
    let encoded = serde_json::to_string(expectation).expect("route expectation serializes");
    let material = format!("route-access-v1\n{route}\n{principal}\n{encoded}");
    crate::domain::hash::sha256_hex(material.as_bytes())[..20].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(final_route: &str, status: u16) -> RouteAccessObservation {
        RouteAccessObservation {
            requested: "/admin".into(),
            final_route: final_route.into(),
            status: Some(status),
            settled: true,
        }
    }

    #[test]
    fn exact_redirect_and_status_contracts_are_tri_state() {
        let redirect = RouteAccessExpectation::Redirect {
            redirect: "/login".into(),
        };
        assert_eq!(
            evaluate(
                "/admin",
                "anonymous",
                &redirect,
                Some(observation("/login", 200)),
                true
            )
            .status,
            EvidenceStatus::Satisfied
        );
        assert_eq!(
            evaluate(
                "/admin",
                "anonymous",
                &redirect,
                Some(observation("/admin", 200)),
                true
            )
            .status,
            EvidenceStatus::Violation
        );
        assert_eq!(
            evaluate(
                "/admin",
                "member",
                &redirect,
                Some(observation("/login", 200)),
                false
            )
            .status,
            EvidenceStatus::Abstain
        );
    }

    #[test]
    fn incomplete_navigation_abstains_instead_of_becoming_a_violation() {
        let mut incomplete = observation("/admin", 200);
        incomplete.settled = false;
        let result = evaluate(
            "/admin",
            "anonymous",
            &RouteAccessExpectation::Decision(RouteAccessDecision::Allow),
            Some(incomplete),
            true,
        );
        assert_eq!(result.status, EvidenceStatus::Abstain);
        assert_eq!(result.reason_code, Some(ReasonCode::IncompleteStream));
    }
}
