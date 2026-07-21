//! Bounded browser execution for declared route-access contracts.

use crate::adapters::{config, platform};
use crate::domain::evidence::EvidenceStatus;
use crate::domain::route_access::{
    self, RouteAccessEvaluation, RouteAccessExpectation, RouteAccessObservation,
};
use crate::interface::cli::context::Ctx;
use crate::runtime::project_layout as layout;
use crate::workflows::{fuzz, journey};
use anyhow::{Context, Result};
use reproit_protocol::ReasonCode;
use serde::Serialize;
use serde_json::json;

const MARKER: &str = "REPROIT:ROUTE_ACCESS ";
const MAX_MARKER_BYTES: usize = 4096;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteAccessSummary {
    pub complete: bool,
    pub checked: usize,
    pub violations: usize,
    pub abstentions: usize,
    pub results: Vec<RouteAccessEvaluation>,
}

pub async fn run(ctx: &Ctx, loaded: &config::Loaded) -> Result<RouteAccessSummary> {
    let resolved = platform::resolve(&loaded.config.app.platform)
        .expect("config loader validates the platform");
    if resolved.backend != platform::Backend::WebCdp {
        anyhow::bail!("routeAccess currently requires a web, Electron, or Tauri target");
    }
    if loaded.config.route_access.is_empty() {
        anyhow::bail!("`scan --only route-access` needs at least one routeAccess entry");
    }

    let mut results = Vec::new();
    for spec in &loaded.config.route_access {
        for (principal, expected) in &spec.access {
            ctx.say(format!("  route-access: {principal} -> {}", spec.route));
            let setup = if principal == "anonymous" {
                Ok(Vec::new())
            } else {
                journey::account_setup_actions(loaded, principal)
            };
            let mut evaluation = match setup {
                Ok(mut actions) => {
                    actions.push(format!("visit:{}", spec.route));
                    probe(loaded, &spec.route, principal, expected, &actions).await?
                }
                Err(error) => route_access::evaluate(&spec.route, principal, expected, None, false)
                    .with_reason(format!("principal authority unavailable: {error:#}")),
            };
            if evaluation.status == EvidenceStatus::Violation {
                let setup = if principal == "anonymous" {
                    Ok(Vec::new())
                } else {
                    journey::account_setup_actions(loaded, principal)
                };
                let confirmed = match setup {
                    Ok(mut actions) => {
                        actions.push(format!("visit:{}", spec.route));
                        let replay =
                            probe(loaded, &spec.route, principal, expected, &actions).await?;
                        replay.status == EvidenceStatus::Violation
                            && replay.observation == evaluation.observation
                    }
                    Err(_) => false,
                };
                if !confirmed {
                    evaluation.status = EvidenceStatus::Abstain;
                    evaluation.reason_code = Some(ReasonCode::IncompleteStream);
                    evaluation.reason = Some(
                        "the access violation did not reproduce with identical route evidence"
                            .into(),
                    );
                }
            }
            results.push(evaluation);
        }
    }

    let violations = results
        .iter()
        .filter(|result| result.status == EvidenceStatus::Violation)
        .count();
    let abstentions = results
        .iter()
        .filter(|result| result.status == EvidenceStatus::Abstain)
        .count();
    let summary = RouteAccessSummary {
        complete: abstentions == 0,
        checked: results.len(),
        violations,
        abstentions,
        results,
    };
    persist_summary(&loaded.root, &summary)?;
    if ctx.json {
        ctx.emit(&json!({
            "command": "scan",
            "only": "route-access",
            "complete": summary.complete,
            "checked": summary.checked,
            "issues": summary.violations,
            "abstentions": summary.abstentions,
            "results": summary.results,
        }));
    } else {
        print_summary(ctx, &summary);
    }
    Ok(summary)
}

async fn probe(
    loaded: &config::Loaded,
    route: &str,
    principal: &str,
    expected: &RouteAccessExpectation,
    actions: &[String],
) -> Result<RouteAccessEvaluation> {
    let config_path = layout::fuzz_config_path(&loaded.root);
    std::fs::create_dir_all(
        config_path
            .parent()
            .expect("fuzz config always has a parent"),
    )?;
    std::fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "seed": 1,
            "budget": actions.len().max(1),
            "replay": actions,
        }))?,
    )?;
    let defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        config_path.to_string_lossy().into_owned(),
    )];
    let outcome = fuzz::run_explorer(
        &loaded.config,
        &loaded.root,
        "explore",
        false,
        &defines,
        false,
        false,
        false,
    )
    .await
    .with_context(|| format!("probing route {route:?} as {principal:?}"))?;
    let log = std::fs::read_to_string(outcome.run_dir.join("drive-a.log"))
        .context("reading route-access runner log")?;
    let authority_available = principal == "anonymous"
        || (!log.contains("FUZZ:MISS ") && !log.contains("FUZZ:ASSERT fail"));
    if !authority_available {
        return Ok(route_access::evaluate(
            route, principal, expected, None, false,
        ));
    }
    Ok(match observation_for(&log, route) {
        RouteObservation::Observed(observation) => {
            route_access::evaluate(route, principal, expected, observation, true)
        }
        RouteObservation::Defect(reason_code, reason) => {
            route_access::abstain_for_defect(route, principal, expected, reason_code, reason)
        }
    })
}

#[derive(Debug, PartialEq, Eq)]
enum RouteObservation {
    Observed(Option<RouteAccessObservation>),
    Defect(ReasonCode, &'static str),
}

fn observation_for(log: &str, route: &str) -> RouteObservation {
    let mut observed = None;
    for line in log.lines() {
        let Some(payload) = line.strip_prefix(MARKER) else {
            continue;
        };
        if payload.len() > MAX_MARKER_BYTES {
            return RouteObservation::Defect(
                ReasonCode::FrameTooLarge,
                "the attributed route observation exceeded its byte limit",
            );
        }
        let Ok(observation) = serde_json::from_str::<RouteAccessObservation>(payload) else {
            return RouteObservation::Defect(
                ReasonCode::MalformedFrame,
                "the attributed route observation was malformed",
            );
        };
        if observation.requested == route {
            observed = Some(observation);
        }
    }
    RouteObservation::Observed(observed)
}

fn persist_summary(root: &std::path::Path, summary: &RouteAccessSummary) -> Result<()> {
    let path = layout::tmp_dir(root).join("route-access.json");
    std::fs::create_dir_all(path.parent().expect("summary path has a parent"))?;
    std::fs::write(path, serde_json::to_vec_pretty(summary)?)?;
    Ok(())
}

fn print_summary(ctx: &Ctx, summary: &RouteAccessSummary) {
    ctx.say(format!(
        "\nroute-access: {} checked, {} violation(s), {} abstention(s)",
        summary.checked, summary.violations, summary.abstentions
    ));
    for result in &summary.results {
        let label = match result.status {
            EvidenceStatus::Satisfied => "pass",
            EvidenceStatus::Violation => "FAIL",
            EvidenceStatus::Abstain => "ABSTAIN",
        };
        ctx.say(format!(
            "  {label:7} {:12} {}{}",
            result.principal,
            result.route,
            result
                .reason
                .as_ref()
                .map(|reason| format!("  {reason}"))
                .unwrap_or_default()
        ));
    }
}

trait EvaluationReason {
    fn with_reason(self, reason: String) -> Self;
}

impl EvaluationReason for RouteAccessEvaluation {
    fn with_reason(mut self, reason: String) -> Self {
        self.reason = Some(reason);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_observation_is_exact_bounded_and_last_write_wins() {
        let log = concat!(
            "REPROIT:ROUTE_ACCESS {\"requested\":\"/a\",\"finalRoute\":\"/old\",",
            "\"status\":200,\"settled\":true}\n",
            "REPROIT:ROUTE_ACCESS {\"requested\":\"/a\",\"finalRoute\":\"/new\",",
            "\"status\":200,\"settled\":true}\n",
        );
        let RouteObservation::Observed(Some(observation)) = observation_for(log, "/a") else {
            panic!("expected an attributed observation");
        };
        assert_eq!(observation.final_route, "/new");
        assert_eq!(
            observation_for(log, "/missing"),
            RouteObservation::Observed(None)
        );
    }

    #[test]
    fn oversized_and_malformed_observations_are_explicit_stream_defects() {
        let oversized = format!("{MARKER}{}", "x".repeat(MAX_MARKER_BYTES + 1));
        assert_eq!(
            observation_for(&oversized, "/a"),
            RouteObservation::Defect(
                ReasonCode::FrameTooLarge,
                "the attributed route observation exceeded its byte limit"
            )
        );
        assert_eq!(
            observation_for(&format!("{MARKER}not-json"), "/a"),
            RouteObservation::Defect(
                ReasonCode::MalformedFrame,
                "the attributed route observation was malformed"
            )
        );
    }
}
