use super::*;

pub(super) fn persist_findings(
    root: &Path,
    schema: &Path,
    schema_sha256: &str,
    seed: u64,
    findings: Vec<FindingCase>,
) -> Result<Vec<Value>> {
    let mut persisted = Vec::new();
    let mut seen = BTreeSet::new();
    for (endpoint, request, setup, mut finding) in findings {
        let fingerprint = finding
            .get("fingerprint")
            .and_then(Value::as_str)
            .context("backend finding has no fingerprint")?;
        if !seen.insert(fingerprint.to_string()) {
            continue;
        }
        let raw_id = repro::finding_id(
            schema_sha256,
            fingerprint,
            seed,
            &[format!("{} {}", request.method, request.url)],
        );
        let public_id = repro::display_finding_id(&raw_id);
        finding["id"] = Value::String(public_id.clone());
        finding["setupSteps"] = Value::from(setup.len());
        let directory = layout::finding_dir(root, &raw_id);
        std::fs::create_dir_all(&directory)?;
        let artifact = BackendFindingArtifact {
            format: "reproit-backend-finding".into(),
            version: 2,
            schema: schema.to_string_lossy().into_owned(),
            schema_sha256: schema_sha256.into(),
            reset_url: std::env::var("REPROIT_BACKEND_RESET_URL").ok(),
            setup,
            failing: ReplayStep {
                contract: endpoint.contract,
                request,
                policy: endpoint.policy,
            },
            finding: finding.clone(),
        };
        std::fs::write(
            directory.join("backend.json"),
            serde_json::to_vec_pretty(&artifact)?,
        )?;
        std::fs::write(
            directory.join("fuzz.md"),
            format!(
                "# Backend finding (seed {seed})\n\n<!-- finding-id: {raw_id} -->\n\n## confirmed \
                 repro (0 actions)\n\n```\n```\n\nReplay: `reproit {public_id}`\n"
            ),
        )?;
        persisted.push(finding);
    }
    Ok(persisted)
}

pub(super) fn persist_schema_findings(
    root: &Path,
    schema: &Path,
    schema_sha256: &str,
    violations: Vec<backend::BackendSchemaViolation>,
) -> Result<Vec<Value>> {
    let mut persisted = Vec::new();
    let mut seen = BTreeSet::new();
    for violation in violations {
        if !seen.insert(violation.fingerprint.clone()) {
            continue;
        }
        let raw_id = repro::finding_id(
            "backend-schema",
            &violation.fingerprint,
            0,
            std::slice::from_ref(&violation.pointer),
        );
        let public_id = repro::display_finding_id(&raw_id);
        let finding = json!({
            "id": public_id,
            "oracle": "backend-contract",
            "invariant": format!("backend:{}", violation.oracle),
            "kind": violation.oracle,
            "message": violation.reason,
            "operation": violation.operation,
            "contract_hash": &schema_sha256[..16],
            "fingerprint": violation.fingerprint,
            "trigger": violation.fingerprint,
            "frames": [format!("schema:{}", violation.pointer)],
        });
        let directory = layout::finding_dir(root, &raw_id);
        std::fs::create_dir_all(&directory)?;
        let artifact = BackendSchemaFindingArtifact {
            format: "reproit-backend-schema-finding".into(),
            version: 1,
            schema: schema.to_string_lossy().into_owned(),
            schema_sha256: schema_sha256.into(),
            violation,
            finding: finding.clone(),
        };
        std::fs::write(
            directory.join("backend-schema.json"),
            serde_json::to_vec_pretty(&artifact)?,
        )?;
        std::fs::write(
            directory.join("scan.md"),
            format!(
                "# Backend schema finding\n\n<!-- finding-id: {raw_id} -->\n\nReplay: `reproit \
                 {public_id}`\n"
            ),
        )?;
        persisted.push(finding);
    }
    Ok(persisted)
}

pub(super) fn persist_run_report(root: &Path, command: &str, report: &Value) -> Result<()> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let directory = root
        .join(".reproit/runs")
        .join(format!("backend-{command}-{stamp}"));
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("backend-report.json"),
        serde_json::to_vec_pretty(report)?,
    )?;
    Ok(())
}

pub(super) fn emit_report(ctx: &Ctx, command: &str, report: &Value) {
    if ctx.json {
        ctx.emit(report);
        return;
    }
    let findings = report["findings"].as_array().map_or(0, Vec::len);
    let candidates = report["candidates"].as_array().map_or(0, Vec::len);
    let errors = report["executionErrors"].as_array().map_or(0, Vec::len);
    ctx.say(format!(
        "backend {command}: {} operation(s) exercised, {findings} confirmed finding(s), \
         {candidates} candidate(s), {errors} execution error(s)",
        report["exercised"].as_u64().unwrap_or(0)
    ));
    if let Some(values) = report["findings"].as_array() {
        for finding in values {
            ctx.say(format!(
                "  {}  {}: {}",
                finding
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("fnd_unknown"),
                finding
                    .get("operation")
                    .and_then(Value::as_str)
                    .unwrap_or("operation"),
                finding.get("message").and_then(Value::as_str).unwrap_or("")
            ));
        }
    }
}
