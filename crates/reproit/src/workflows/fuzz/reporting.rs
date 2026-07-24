use super::*;

pub(super) fn persist_causal_capsule(
    cfg: &Config,
    root: &Path,
    run_dir: &Path,
    finding: &Value,
    actions: &[String],
    runtime_defines: &[(String, String)],
    seed: u64,
) -> Result<crate::domain::capsule::Capsule> {
    let first = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| finding.get(*key).and_then(Value::as_str))
            .unwrap_or("")
            .to_string()
    };
    let frame = finding
        .get("frames")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let boundary = ["operation", "endpoint", "url", "request", "event"]
        .iter()
        .find_map(|key| finding.get(*key).and_then(Value::as_str))
        .map(str::to_string);
    let raw_oracle = finding.get("oracle").and_then(Value::as_str).unwrap_or("");
    let oracle = if crate::domain::backend::is_backend_oracle(raw_oracle) {
        raw_oracle.to_string()
    } else {
        crate::domain::oracle::classify(finding)
            .as_str()
            .to_string()
    };
    let crash = oracle == "crash";
    let identity = crate::domain::capsule::FindingIdentity {
        oracle,
        invariant: if crash {
            "no-exception".into()
        } else {
            first(&["invariant"])
        },
        kind: if crash {
            "exception".into()
        } else {
            first(&["kind"])
        },
        message: normalize_message(&first(&["message"])),
        // Browser production telemetry cannot reliably preserve a source-mapped
        // frame or the runner-only trigger marker. The normalized crash message
        // remains the structural cause coordinate; non-crash oracles retain the
        // richer frame and trigger dimensions.
        frame: if crash { String::new() } else { frame },
        trigger: if crash {
            String::new()
        } else {
            first(&["root_trigger", "trigger", "element", "selector", "sig"])
        },
        boundary,
    };
    let mut capsule = crate::domain::capsule::Capsule::new(target_identity(cfg), identity);
    capsule.capabilities.insert(
        "ui_actions".into(),
        crate::domain::capsule::Capability {
            status: crate::domain::capsule::CaptureStatus::Captured,
            detail: None,
        },
    );
    capsule
        .environment
        .insert("platform".into(), cfg.app.platform.clone());
    capsule.environment.insert("seed".into(), seed.to_string());
    capsule.environment.insert(
        "status_bar_time".into(),
        cfg.devices.determinism.status_bar_time.clone(),
    );
    if let Some([lat, lon]) = cfg.devices.determinism.location {
        capsule
            .environment
            .insert("location".into(), format!("{lat},{lon}"));
    }
    let mut flag_count = 0usize;
    for (key, value) in &cfg.app.defines {
        if ["secret", "token", "password", "cookie", "authorization"]
            .iter()
            .any(|needle| key.to_ascii_lowercase().contains(needle))
        {
            capsule.redactions.push(format!("define:{key}"));
        } else {
            capsule
                .environment
                .insert(format!("define:{key}"), value.clone());
            flag_count += 1;
        }
    }
    for (key, value) in runtime_defines {
        if key == crate::domain::locale::LOCALE_ENV && !value.is_empty() {
            capsule
                .environment
                .insert(format!("define:{key}"), value.clone());
        }
    }
    capsule.capabilities.insert(
        "feature_flags".into(),
        crate::domain::capsule::Capability {
            status: crate::domain::capsule::CaptureStatus::Captured,
            detail: Some(format!("{flag_count} configured define(s)")),
        },
    );
    capsule.capabilities.insert(
        "clock".into(),
        crate::domain::capsule::Capability {
            status: crate::domain::capsule::CaptureStatus::Captured,
            detail: Some("deterministic device status time".into()),
        },
    );
    capsule.capabilities.insert(
        "randomness".into(),
        crate::domain::capsule::Capability {
            status: crate::domain::capsule::CaptureStatus::Captured,
            detail: Some(format!("fuzz seed {seed}")),
        },
    );
    if let Some(url) = &cfg.app.url {
        capsule.environment.insert("url".into(), url.clone());
    }
    if let Ok(sha) = std::env::var("GIT_COMMIT").or_else(|_| std::env::var("GITHUB_SHA")) {
        capsule.builds.insert("client".into(), sha);
    }
    capsule.actions = actions
        .iter()
        .enumerate()
        .map(|(index, action)| crate::domain::capsule::Action {
            // Index 0 is reserved for bootstrap network traffic.
            index: index as u32 + 1,
            actor: "a".into(),
            action: action.clone(),
            from_sig: None,
            to_sig: None,
        })
        .collect();
    capsule.ingest_network_files(run_dir)?;
    capsule.ingest_backend_files(run_dir)?;
    crate::domain::capsule::redact_capsule(
        &mut capsule,
        &crate::domain::capsule::RedactionPolicy::default(),
    );
    capsule.finalize_id()?;
    if !capsule.confirmable() {
        let missing = capsule.missing_required_capabilities().join(", ");
        anyhow::bail!("finding cannot be confirmed as a causal capsule; missing: {missing}");
    }
    let missing_replay = capsule.missing_required_replay_capabilities();
    if !missing_replay.is_empty() {
        anyhow::bail!(
            "finding cannot be confirmed hermetically; missing replay capability: {}",
            missing_replay.join(", ")
        );
    }
    capsule.persist(root)?;
    Ok(capsule)
}

/// Keep pending finding ids resolvable independently of run retention and the
/// currently configured evidence directory. Run artifacts are useful evidence,
/// but they are not an identity store: a later scan may rotate or relocate
/// them.
pub(super) fn persist_finding_report(root: &Path, id: &str, report_dir: &Path) -> Result<()> {
    let dir = layout::finding_dir(root, id);
    std::fs::create_dir_all(&dir)?;
    let stored = dir.join("fuzz.md");
    if !stored.exists() {
        std::fs::copy(report_dir.join("fuzz.md"), stored)?;
    }
    let evidence = report_dir.join("run-evidence.json");
    if evidence.exists() {
        std::fs::copy(evidence, dir.join("run-evidence.json"))?;
    }
    Ok(())
}

const MAX_PROMOTED_ALIASES: usize = 200;
const PROMOTED_ALIAS_RETENTION: std::time::Duration = std::time::Duration::from_secs(30 * 86_400);

/// Make the confirmed finding canonical and reduce an earlier provisional
/// identity to a small compatibility alias. The canonical evidence is checked
/// before duplicate files are removed, so promotion never discards the only
/// copy of a finding.
pub(super) fn promote_finding(
    root: &Path,
    provisional_id: Option<&str>,
    confirmed_id: &str,
    report_dir: &Path,
) -> Result<()> {
    let confirmed = layout::finding_dir(root, confirmed_id);
    if !confirmed.join("fuzz.md").is_file() || !confirmed.join("run-evidence.json").is_file() {
        anyhow::bail!("confirmed finding is missing its durable report or evidence graph");
    }
    std::fs::write(
        confirmed.join("status.json"),
        serde_json::to_vec_pretty(&json!({
            "status": "confirmed",
            "canonicalId": confirmed_id,
        }))?,
    )?;
    std::fs::write(report_dir.join("canonical-finding-id"), confirmed_id)?;

    if let Some(provisional_id) = provisional_id.filter(|id| *id != confirmed_id) {
        let provisional = layout::finding_dir(root, provisional_id);
        std::fs::create_dir_all(&provisional)?;
        std::fs::write(provisional.join("promoted-to"), confirmed_id)?;
        for duplicate in [
            "fuzz.md",
            "run-evidence.json",
            "contract.json",
            "backend-contract.json",
            "capsule-id",
            "identity.json",
            "status.json",
        ] {
            let _ = std::fs::remove_file(provisional.join(duplicate));
        }
    }
    prune_promoted_aliases(root)
}

fn prune_promoted_aliases(root: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(layout::findings_dir(root)) else {
        return Ok(());
    };
    let mut aliases = Vec::new();
    for entry in entries.take(MAX_PROMOTED_ALIASES * 4) {
        let entry = entry?;
        if !entry.path().join("promoted-to").is_file() {
            continue;
        }
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        aliases.push((modified, entry.path()));
    }
    aliases.sort_by_key(|(modified, path)| (*modified, path.clone()));
    let excess = aliases.len().saturating_sub(MAX_PROMOTED_ALIASES);
    for (index, (modified, path)) in aliases.into_iter().enumerate() {
        let expired = modified
            .elapsed()
            .is_ok_and(|age| age > PROMOTED_ALIAS_RETENTION);
        if index < excess || expired {
            std::fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

pub(super) fn write_report(
    run_dir: &Path,
    finding_raw_id: &str,
    seed: u64,
    findings: &[Value],
    trace: &[String],
    shrunk: &[String],
    confirmation: reproit_protocol::ConfirmationStatus,
) -> Result<()> {
    let reproduced = confirmation == reproit_protocol::ConfirmationStatus::Reproduced;
    let result_name = if reproduced { "finding" } else { "candidate" };
    let mut md =
        format!("# fuzz {result_name} (seed {seed})\n\n<!-- finding-id: {finding_raw_id} -->\n\n");
    // Each finding carries an `invariant` id (the named property it violates),
    // so the report leads with the invariant summary, then the detail. Findings
    // without an invariant fall under "exception".
    let invariant_of = |f: &Value| {
        f.get("invariant")
            .and_then(Value::as_str)
            .unwrap_or("exception")
            .to_string()
    };
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for f in findings {
        *counts.entry(invariant_of(f)).or_default() += 1;
    }
    md.push_str("## invariants violated\n\n");
    for (inv, n) in &counts {
        md.push_str(&format!("- **{inv}** ({n})\n"));
    }
    // PRIMARY finding header: a machine-readable line `keep` parses to record the
    // finding's ORACLE category, its named INVARIANT, and (for graph invariants)
    // the offending STATE SIG, so `check` can re-confirm the SAME finding by its
    // oracle rather than only looking for exceptions. The primary finding is the
    // MOST-SEVERE one (a real bug over an incidental graph/label invariant on the
    // same trace), consistent with the shrink target.
    if let Some(primary) = primary_finding(findings) {
        let oracle = crate::domain::oracle::classify(primary).as_str();
        let inv = invariant_of(primary);
        let sig = primary.get("sig").and_then(Value::as_str).unwrap_or("");
        let selector = primary
            .get("selector")
            .and_then(Value::as_str)
            .unwrap_or("");
        let fingerprint = primary
            .get("fingerprint")
            .and_then(Value::as_str)
            .unwrap_or("");
        md.push_str(&format!(
            "\n## oracle\n\n- oracle: `{oracle}`\n- invariant: `{inv}`\n- sig: `{sig}`\n- \
             selector: `{selector}`\n- fingerprint: `{fingerprint}`\n"
        ));
    }
    md.push_str("\n## findings\n\n");
    for f in findings.iter().take(8) {
        md.push_str(&format!(
            "- `{}` **{}**: {}\n",
            invariant_of(f),
            f.get("kind").and_then(Value::as_str).unwrap_or("?"),
            f.get("message").and_then(Value::as_str).unwrap_or("")
        ));
        for frame in f
            .get("frames")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .take(2)
        {
            md.push_str(&format!("  - `{}`\n", frame.as_str().unwrap_or("")));
        }
    }
    let finding_id = crate::domain::repro::display_finding_id(finding_raw_id);
    let trace_name = if reproduced {
        "confirmed repro"
    } else {
        "unconfirmed candidate trace"
    };
    md.push_str(&format!(
        "\n## {trace_name} ({} actions{})\n\n```\n{}\n```\n",
        shrunk.len(),
        if shrunk.len() < trace.len() {
            format!(", shrunk from {}", trace.len())
        } else {
            String::new()
        },
        shrunk.join("\n"),
    ));
    if reproduced {
        md.push_str(&format!(
            "\nReproduce: `reproit {finding_id}`\nKeep: `reproit keep {finding_id} --as \
             <name>`\nAfter keeping, record an annotated video with `reproit \
             @<alias> --record-video`.\n"
        ));
    } else {
        md.push_str(&format!(
            "\nInspect blockers: `reproit proof {finding_id}`\nRun a clean replay: `reproit \
             {finding_id}`\n"
        ));
    }
    std::fs::write(run_dir.join("fuzz.md"), md).context("writing fuzz report")
}

pub(super) struct RunEvidence<'a> {
    pub capture_dir: &'a Path,
    pub finding_id: &'a str,
    pub trace: &'a [String],
    pub findings: &'a [Value],
    pub minimized: &'a [String],
    pub confirmation: reproit_protocol::ConfirmationStatus,
    pub capsule: Option<&'a crate::domain::capsule::Capsule>,
}

pub(super) fn write_run_evidence_graph(
    output_dir: &Path,
    evidence: RunEvidence<'_>,
) -> Result<reproit_protocol::ProofLedger> {
    let log = std::fs::read(evidence.capture_dir.join("drive-a.log")).unwrap_or_default();
    let raw = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::RawCapture,
        vec![],
        json!({
            "path": "drive-a.log",
            "bytes": log.len(),
            "sha256": crate::domain::hash::sha256_hex(&log),
        }),
    )?;
    let normalized = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::NormalizedTrace,
        vec![raw.id.clone()],
        json!({ "actions": evidence.trace }),
    )?;
    let evaluation = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::Evaluation,
        vec![normalized.id.clone()],
        json!({ "findings": evidence.findings }),
    )?;
    let replay_identity_matched =
        evidence.confirmation == reproit_protocol::ConfirmationStatus::Reproduced;
    let replay = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::Replay,
        vec![evaluation.id.clone()],
        json!({
            "confirmation": evidence.confirmation,
            "identityMatched": replay_identity_matched,
        }),
    )?;
    let minimized = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::MinimizedTrace,
        vec![replay.id.clone()],
        json!({ "actions": evidence.minimized }),
    )?;
    let authority = authority_for_findings(evidence.findings);
    let (evaluation_status, evaluation_reasons) = if authority.is_empty() {
        (
            reproit_protocol::EvaluationStatus::Abstain,
            vec![reproit_protocol::ReasonCode::AuthorityUnavailable],
        )
    } else {
        (reproit_protocol::EvaluationStatus::Violation, vec![])
    };
    let minimization = if replay_identity_matched {
        reproit_protocol::MinimizationStatus::Preserved
    } else if evidence.confirmation == reproit_protocol::ConfirmationStatus::NotAttempted {
        reproit_protocol::MinimizationStatus::NotAttempted
    } else {
        reproit_protocol::MinimizationStatus::CouldNotConfirm
    };
    let proof = reproit_protocol::ProofLedger::from_stages(
        vec![crate::domain::repro::display_finding_id(
            evidence.finding_id,
        )],
        authority,
        evaluation_status,
        evaluation_reasons,
        evidence.confirmation,
        replay_identity_matched,
        minimization,
    )?;
    let mut nodes = vec![raw, normalized, evaluation, replay, minimized];
    let mut proof_parent = nodes.last().expect("minimized node exists").id.clone();
    if let Some(capsule) = evidence.capsule {
        let causal = reproit_protocol::ArtifactNode::new(
            reproit_protocol::ArtifactKind::CausalGraph,
            vec![proof_parent],
            serde_json::to_value(&capsule.causal_graph)?,
        )?;
        proof_parent = causal.id.clone();
        nodes.push(causal);
        let environment = reproit_protocol::ArtifactNode::new(
            reproit_protocol::ArtifactKind::EnvironmentEnvelope,
            vec![proof_parent],
            serde_json::to_value(reproit_protocol::EnvironmentProof {
                captured: capsule.environment.clone(),
                envelope: capsule.environment_envelope.clone(),
            })?,
        )?;
        proof_parent = environment.id.clone();
        nodes.push(environment);
    }
    let ledger = reproit_protocol::ArtifactNode::new(
        reproit_protocol::ArtifactKind::ProofLedger,
        vec![proof_parent],
        serde_json::to_value(&proof)?,
    )?;
    let digest = ledger.id.trim_start_matches("sha256:");
    nodes.push(ledger.clone());
    let graph = reproit_protocol::EvidenceGraph {
        run_id: format!("run-{}", &digest[..16]),
        root: ledger.id.clone(),
        nodes,
    };
    graph.validate()?;
    std::fs::write(
        output_dir.join("run-evidence.json"),
        serde_json::to_vec_pretty(&graph)?,
    )?;
    Ok(proof)
}

fn authority_for_findings(findings: &[Value]) -> Vec<reproit_protocol::AuthoritySource> {
    let mut authority = std::collections::BTreeSet::new();
    for finding in findings {
        let Some(source) = finding
            .get("oracle")
            .and_then(Value::as_str)
            .and_then(authority_for_oracle)
        else {
            return Vec::new();
        };
        authority.insert(source);
    }
    authority.into_iter().collect()
}

fn authority_for_oracle(oracle: &str) -> Option<reproit_protocol::AuthoritySource> {
    use reproit_protocol::AuthoritySource;
    match oracle {
        "crash" => Some(AuthoritySource::RuntimeDiagnosis),
        "contract" | "invariant" | "detached-indicator" => Some(AuthoritySource::AuthoredContract),
        // The backend contract family (per-check "backend-*" ids and the
        // legacy "backend-contract" umbrella) is authored-contract evidence.
        oracle if crate::domain::backend::is_backend_oracle(oracle) => {
            Some(AuthoritySource::AuthoredContract)
        }
        _ => None,
    }
}

/// Run the find -> PR delivery pipeline for one finding: annotate + upload the
/// minimized-repro clip to the cloud, then emit the PR comment (dry-run unless
/// `post` and a GitHub repo/PR/token are resolvable). Reuses the `deliver`
/// module so `reproit publish` / `reproit comment` and the in-fuzz path share
/// one implementation.
#[allow(clippy::too_many_arguments)]
pub(super) async fn deliver_finding(
    cfg: &Config,
    root: &Path,
    run_dir: &Path,
    cloud: &str,
    app: &str,
    bucket: &str,
    post: bool,
    confirmed: bool,
    json: bool,
) -> Result<()> {
    let run_name = run_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned());
    say(
        json,
        format!("  deliver: publishing finding to {cloud} (app {app}, bucket {bucket})"),
    );
    crate::workflows::deliver::publish(
        cfg,
        root,
        app,
        bucket,
        run_name.as_deref(),
        None,
        Some(cloud.to_string()),
        None,
    )
    .await?;
    // Emit the PR comment. Dry-run unless --post-comment AND the GitHub env is
    // present (we never claim to post what we can't). `confirmed` flows through
    // the run dir's exceptions/manifest the comment formatter already reads.
    let _ = confirmed;
    crate::workflows::deliver::comment(
        cfg,
        root,
        app,
        bucket,
        run_name.as_deref(),
        !post, // dry_run when not explicitly posting
        None,
        None,
        None,
        Some(cloud.to_string()),
        None,
    )
    .await?;
    Ok(())
}
