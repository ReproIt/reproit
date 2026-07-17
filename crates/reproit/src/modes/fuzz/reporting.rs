use super::*;

pub(super) fn persist_causal_capsule(
    cfg: &Config,
    root: &Path,
    run_dir: &Path,
    finding: &Value,
    actions: &[String],
    seed: u64,
) -> Result<crate::capsule::Capsule> {
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
    let oracle = if finding.get("oracle").and_then(Value::as_str) == Some("backend-contract") {
        "backend-contract".into()
    } else {
        crate::crosscut::classify(finding).as_str().to_string()
    };
    let crash = oracle == "crash";
    let identity = crate::capsule::FindingIdentity {
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
    let mut capsule = crate::capsule::Capsule::new(target_identity(cfg), identity);
    capsule.capabilities.insert(
        "ui_actions".into(),
        crate::capsule::Capability {
            status: crate::capsule::CaptureStatus::Captured,
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
    capsule.capabilities.insert(
        "feature_flags".into(),
        crate::capsule::Capability {
            status: crate::capsule::CaptureStatus::Captured,
            detail: Some(format!("{flag_count} configured define(s)")),
        },
    );
    capsule.capabilities.insert(
        "clock".into(),
        crate::capsule::Capability {
            status: crate::capsule::CaptureStatus::Captured,
            detail: Some("deterministic device status time".into()),
        },
    );
    capsule.capabilities.insert(
        "randomness".into(),
        crate::capsule::Capability {
            status: crate::capsule::CaptureStatus::Captured,
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
        .map(|(index, action)| crate::capsule::Action {
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
    crate::capsule::redact_capsule(&mut capsule, &crate::capsule::RedactionPolicy::default());
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
    Ok(())
}

pub(super) fn write_report(
    run_dir: &Path,
    finding_raw_id: &str,
    seed: u64,
    findings: &[Value],
    trace: &[String],
    shrunk: &[String],
) -> Result<()> {
    let mut md =
        format!("# fuzz finding (seed {seed})\n\n<!-- finding-id: {finding_raw_id} -->\n\n");
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
        let oracle = crate::crosscut::classify(primary).as_str();
        let inv = invariant_of(primary);
        let sig = primary.get("sig").and_then(Value::as_str).unwrap_or("");
        let selector = primary
            .get("selector")
            .and_then(Value::as_str)
            .unwrap_or("");
        md.push_str(&format!(
            "\n## oracle\n\n- oracle: `{oracle}`\n- invariant: `{inv}`\n- sig: `{sig}`\n- \
             selector: `{selector}`\n"
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
    let finding_id = crate::model::repro::display_finding_id(finding_raw_id);
    md.push_str(&format!(
        "\n## confirmed repro ({} actions{})\n\n```\n{}\n```\n\nReproduce: `reproit \
         {finding_id}`\nKeep: `reproit keep {finding_id} --as <name>`\nAfter keeping, record an \
         annotated video with `reproit record <alias-or-rep-id>`.\n",
        shrunk.len(),
        if shrunk.len() < trace.len() {
            format!(", shrunk from {}", trace.len())
        } else {
            String::new()
        },
        shrunk.join("\n")
    ));
    std::fs::write(run_dir.join("fuzz.md"), md).context("writing fuzz report")
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
    crate::modes::deliver::publish(
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
    crate::modes::deliver::comment(
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
