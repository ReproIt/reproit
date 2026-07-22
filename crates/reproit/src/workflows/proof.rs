//! Read-only projection of a finding's immutable proof graph.

use crate::adapters::config;
use crate::domain::repro;
use crate::interface::cli::context::Ctx;
use crate::runtime::project_layout as layout;
use anyhow::{Context, Result};
use reproit_protocol::{
    ArtifactKind, EvidenceGraph, PromotionBlocker, PromotionStatus, ProofLedger, ReasonCode,
};
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_EVIDENCE_GRAPH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CANDIDATE_ENTRIES: usize = 4_096;

pub(super) fn show_proof(ctx: &Ctx, loaded: &config::Loaded, reference: &str) -> Result<()> {
    let (public_id, graph_path) = resolve_graph_path(loaded, reference)?;
    let graph = load_graph(&graph_path)?;
    let ledger = graph.proof_ledger()?.ok_or_else(|| {
        anyhow::anyhow!(
            "{} has no proof ledger root; regenerate the finding with the current protocol",
            graph_path.display()
        )
    })?;
    let capsule = load_capsule(loaded, reference)?;
    let legacy = requires_reverification(&graph, &ledger);
    if ctx.json {
        let next_evidence = if legacy {
            serde_json::json!([{
                "blocker": "legacy-evidence-requires-reverification",
                "action": "rerun the originating fuzz campaign with the current Reproit CLI",
            }])
        } else {
            serde_json::to_value(additional_evidence(&ledger))?
        };
        let effective_promotion = if legacy {
            "legacy".to_string()
        } else {
            serialized_name(&ledger.promotion)
        };
        let mut output = serde_json::json!({
            "command": "proof",
            "id": public_id,
            "runId": graph.run_id,
            "graphRoot": graph.root,
            "ledger": ledger,
            "nextEvidence": next_evidence,
            "evidenceGeneration": if legacy { "legacy" } else { "current" },
            "requiresReverification": legacy,
            "effectivePromotion": effective_promotion,
        });
        if let Some(capsule) = &capsule {
            let object = output.as_object_mut().expect("proof output is an object");
            object.insert(
                "causalGraph".into(),
                serde_json::to_value(&capsule.causal_graph)?,
            );
            object.insert(
                "environmentEnvelope".into(),
                serde_json::to_value(&capsule.environment_envelope)?,
            );
        }
        ctx.emit(&output);
        return Ok(());
    }
    print_ledger(ctx, &public_id, &graph, &ledger, legacy);
    if let Some(capsule) = capsule {
        print_capsule_proof(ctx, &capsule);
    }
    Ok(())
}

fn load_capsule(
    loaded: &config::Loaded,
    reference: &str,
) -> Result<Option<crate::domain::capsule::Capsule>> {
    let link = if let Some(meta) = repro::resolve(&loaded.root, reference) {
        layout::repro_dir(&loaded.root, &meta.id).join("capsule-id")
    } else {
        let raw = repro::raw_finding_id(reference).unwrap_or(reference);
        layout::finding_dir(&loaded.root, raw).join("capsule-id")
    };
    let id = match std::fs::read_to_string(&link) {
        Ok(id) => id,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", link.display())),
    };
    Ok(Some(crate::domain::capsule::Capsule::load(
        &loaded.root,
        id.trim(),
    )?))
}

fn print_capsule_proof(ctx: &Ctx, capsule: &crate::domain::capsule::Capsule) {
    ctx.say(format!(
        "  causal graph: v{}, {} nodes, {} edges",
        capsule.causal_graph.version,
        capsule.causal_graph.nodes.len(),
        capsule.causal_graph.edges.len()
    ));
    let envelope = &capsule.environment_envelope;
    ctx.say(format!(
        "  environment: {} ({} replay attempts)",
        if envelope.complete {
            "final minimized replay confirmed"
        } else {
            "ABSTAIN"
        },
        envelope.replay_attempts
    ));
    if !envelope.relaxed_dimensions.is_empty() {
        ctx.say(format!(
            "    portable without: {}",
            envelope
                .relaxed_dimensions
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for trial in &envelope.trials {
        if trial.outcome == crate::domain::capsule::EnvironmentOutcome::Reproduces {
            continue;
        }
        ctx.say(format!(
            "    {}: {} ({})",
            trial.dimension,
            serialized_name(&trial.outcome),
            trial.reason
        ));
    }
}

pub(super) fn list_candidates(ctx: &Ctx, loaded: &config::Loaded) -> Result<()> {
    let findings_dir = layout::findings_dir(&loaded.root);
    let entries = match std::fs::read_dir(&findings_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if ctx.json {
                ctx.emit(&serde_json::json!({
                    "command": "candidates",
                    "candidates": [],
                }));
            } else {
                ctx.say("no blocked candidates");
            }
            return Ok(());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading candidate inbox {}", findings_dir.display()));
        }
    };
    let entries = entries
        .take(MAX_CANDIDATE_ENTRIES + 1)
        .collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() > MAX_CANDIDATE_ENTRIES {
        anyhow::bail!(
            "candidate inbox contains more than {} entries",
            MAX_CANDIDATE_ENTRIES
        );
    }
    let mut directories = entries
        .into_iter()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    directories.sort();
    let mut candidates = Vec::new();
    for directory in directories {
        let graph_path = directory.join("run-evidence.json");
        if !graph_path.exists() {
            continue;
        }
        let graph = load_graph(&graph_path)?;
        let Some(ledger) = graph.proof_ledger()? else {
            continue;
        };
        let legacy = requires_reverification(&graph, &ledger);
        if ledger.promotion != PromotionStatus::Candidate && !legacy {
            continue;
        }
        let id = directory
            .file_name()
            .and_then(|name| name.to_str())
            .map(repro::display_finding_id)
            .context("candidate directory has no valid UTF-8 identity")?;
        candidates.push((id, graph, ledger, legacy));
    }
    if ctx.json {
        let candidates = candidates
            .into_iter()
            .map(|(id, graph, ledger, legacy)| {
                let blockers = if legacy {
                    serde_json::json!(["legacy-evidence-requires-reverification"])
                } else {
                    serde_json::to_value(&ledger.blockers).expect("blockers serialize")
                };
                let next_evidence = if legacy {
                    serde_json::json!([{
                        "blocker": "legacy-evidence-requires-reverification",
                        "action": "rerun the originating fuzz campaign with the current Reproit CLI",
                    }])
                } else {
                    serde_json::to_value(additional_evidence(&ledger))
                        .expect("evidence requests serialize")
                };
                serde_json::json!({
                    "id": id,
                    "graphRoot": graph.root,
                    "blockers": blockers,
                    "nextEvidence": next_evidence,
                    "evidenceGeneration": if legacy { "legacy" } else { "current" },
                    "ledger": ledger,
                })
            })
            .collect::<Vec<_>>();
        ctx.emit(&serde_json::json!({
            "command": "candidates",
            "candidates": candidates,
        }));
        return Ok(());
    }
    if candidates.is_empty() {
        ctx.say("no blocked candidates");
        return Ok(());
    }
    ctx.say(format!("candidate inbox ({} blocked)", candidates.len()));
    for (id, _, ledger, legacy) in candidates {
        if legacy {
            ctx.say(format!(
                "  {id}: legacy evidence requires clean reverification"
            ));
            ctx.say("    next: rerun the originating fuzz campaign with the current CLI");
            continue;
        }
        ctx.say(format!(
            "  {id}: {}",
            ledger
                .blockers
                .iter()
                .map(|blocker| blocker.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for request in additional_evidence(&ledger) {
            ctx.say(format!("    next: {}", request.action));
        }
    }
    Ok(())
}

fn resolve_graph_path(loaded: &config::Loaded, reference: &str) -> Result<(String, PathBuf)> {
    if let Some(meta) = repro::resolve(&loaded.root, reference) {
        return Ok((
            repro::display_repro_id(&meta.id),
            layout::repro_dir(&loaded.root, &meta.id).join("run-evidence.json"),
        ));
    }
    let raw = repro::raw_finding_id(reference).unwrap_or(reference);
    let canonical = layout::canonical_finding_id(&loaded.root, raw);
    let path = layout::finding_dir(&loaded.root, &canonical).join("run-evidence.json");
    if path.exists() {
        return Ok((repro::display_finding_id(&canonical), path));
    }
    anyhow::bail!("no finding or saved repro `{reference}` with an immutable proof graph")
}

fn load_graph(path: &Path) -> Result<EvidenceGraph> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_EVIDENCE_GRAPH_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() as u64 > MAX_EVIDENCE_GRAPH_BYTES {
        anyhow::bail!(
            "proof graph {} exceeds the {} byte limit",
            path.display(),
            MAX_EVIDENCE_GRAPH_BYTES
        );
    }
    let graph: EvidenceGraph =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    graph
        .validate()
        .with_context(|| format!("validating {}", path.display()))?;
    Ok(graph)
}

fn print_ledger(
    ctx: &Ctx,
    public_id: &str,
    graph: &EvidenceGraph,
    ledger: &ProofLedger,
    legacy: bool,
) {
    ctx.say(format!("proof {public_id}"));
    ctx.say(format!(
        "  promotion: {}",
        if legacy {
            "legacy (clean reverification required)".to_string()
        } else {
            serialized_name(&ledger.promotion)
        }
    ));
    let authority = ledger
        .authority
        .iter()
        .map(serialized_name)
        .collect::<Vec<_>>();
    ctx.say(format!(
        "  authority: {}",
        if authority.is_empty() {
            "none".to_string()
        } else {
            authority.join(", ")
        }
    ));
    ctx.say(format!(
        "  evaluation: {}",
        serialized_name(&ledger.evaluation)
    ));
    ctx.say(format!(
        "  confirmation: {} (identity {})",
        serialized_name(&ledger.confirmation),
        if ledger.replay_identity_matched {
            "matched"
        } else {
            "did not match"
        }
    ));
    ctx.say(format!(
        "  minimization: {}",
        serialized_name(&ledger.minimization)
    ));
    if ledger.blockers.is_empty() {
        ctx.say("  blockers: none");
    } else {
        ctx.say(format!(
            "  blockers: {}",
            ledger
                .blockers
                .iter()
                .map(|blocker| blocker.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for request in additional_evidence(ledger) {
            ctx.say(format!("  next evidence: {}", request.action));
        }
    }
    ctx.say(format!("  graph: {} ({})", graph.root, graph.run_id));
}

fn requires_reverification(graph: &EvidenceGraph, ledger: &ProofLedger) -> bool {
    if ledger.promotion != PromotionStatus::Confirmed {
        return false;
    }
    let has_causal_graph = graph
        .nodes
        .iter()
        .any(|node| node.kind == ArtifactKind::CausalGraph);
    let has_environment = graph
        .nodes
        .iter()
        .any(|node| node.kind == ArtifactKind::EnvironmentEnvelope);
    !has_causal_graph || !has_environment
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceRequest {
    blocker: &'static str,
    action: &'static str,
}

fn additional_evidence(ledger: &ProofLedger) -> Vec<EvidenceRequest> {
    ledger
        .blockers
        .iter()
        .map(|blocker| EvidenceRequest {
            blocker: blocker.as_str(),
            action: evidence_action(*blocker, &ledger.evaluation_reasons),
        })
        .collect()
}

fn evidence_action(blocker: PromotionBlocker, reasons: &[ReasonCode]) -> &'static str {
    match blocker {
        PromotionBlocker::MissingAuthority => {
            "attach an authored contract, approved baseline, published standard, or runtime diagnosis"
        }
        PromotionBlocker::NoViolation => {
            "no contradiction was observed; keep this result clean unless a new candidate identity appears"
        }
        PromotionBlocker::EvaluationAbstained if reasons.contains(&ReasonCode::FrameTooLarge) => {
            "capture a bounded frame with the same explicit contract scope"
        }
        PromotionBlocker::EvaluationAbstained
            if reasons.contains(&ReasonCode::AuthorityUnavailable) =>
        {
            "provide the exact authority referenced by the evaluation"
        }
        PromotionBlocker::EvaluationAbstained
            if reasons.contains(&ReasonCode::NoObservations) =>
        {
            "capture at least one complete normalized observation for the evaluated scope"
        }
        PromotionBlocker::EvaluationAbstained => {
            "recapture a complete, ordered, supported, and bounded evidence stream"
        }
        PromotionBlocker::ReplayNotReproduced => {
            "run the exact candidate again in a clean reset environment"
        }
        PromotionBlocker::ReplayIdentityMismatch => {
            "replay until the original canonical finding identity is observed"
        }
        PromotionBlocker::MinimizationNotPreserved => {
            "minimize again and retain only a trace that reproduces the exact identity"
        }
    }
}

fn serialized_name(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reproit_protocol::{
        AuthoritySource, ConfirmationStatus, EvaluationStatus, MinimizationStatus,
    };

    #[test]
    fn planner_requests_the_smallest_evidence_for_a_scoped_oversized_frame() {
        let ledger = ProofLedger::from_stages(
            vec!["candidate".into()],
            vec![AuthoritySource::AuthoredContract],
            EvaluationStatus::Abstain,
            vec![ReasonCode::FrameTooLarge],
            ConfirmationStatus::NotAttempted,
            false,
            MinimizationStatus::NotAttempted,
        )
        .unwrap();
        let requests = additional_evidence(&ledger);
        assert_eq!(requests[0].blocker, "evaluation-abstained");
        assert!(requests[0].action.contains("bounded frame"));
    }

    #[test]
    fn planner_never_suggests_overriding_a_clean_evaluation() {
        let ledger = ProofLedger::from_stages(
            vec!["candidate".into()],
            vec![AuthoritySource::ApprovedBaseline],
            EvaluationStatus::Satisfied,
            vec![],
            ConfirmationStatus::NotReproduced,
            false,
            MinimizationStatus::NotAttempted,
        )
        .unwrap();
        let requests = additional_evidence(&ledger);
        assert_eq!(requests[0].blocker, "no-violation");
        assert!(requests[0].action.contains("keep this result clean"));
    }

    #[test]
    fn confirmed_graph_without_causal_and_environment_proof_is_legacy() {
        let ledger = ProofLedger::from_stages(
            vec!["fnd_legacy000001".into()],
            vec![AuthoritySource::RuntimeDiagnosis],
            EvaluationStatus::Violation,
            vec![],
            ConfirmationStatus::Reproduced,
            true,
            MinimizationStatus::Preserved,
        )
        .unwrap();
        let graph = EvidenceGraph {
            run_id: "run-legacy".into(),
            root: "sha256:legacy".into(),
            nodes: vec![],
        };
        assert!(requires_reverification(&graph, &ledger));
    }
}
