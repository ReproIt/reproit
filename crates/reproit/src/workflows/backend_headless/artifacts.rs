use super::*;

/// Rewrite an absolute request URL to origin-relative (path + query) and
/// record the shared origin once. The first origin seen wins; a step against
/// a DIFFERENT origin keeps its absolute URL rather than being guessed onto
/// the wrong base (the replay resolver treats non-`/` URLs as absolute).
fn relativize_request(request: &mut RequestArtifact, origin: &mut Option<String>) {
    let Ok(parsed) = request.url.parse::<reqwest::Url>() else {
        return;
    };
    let base = parsed.origin().ascii_serialization();
    match origin {
        None => *origin = Some(base),
        Some(existing) if *existing != base => return,
        _ => {}
    }
    let mut relative = parsed.path().to_string();
    if let Some(query) = parsed.query() {
        relative.push('?');
        relative.push_str(query);
    }
    request.url = relative;
}

/// The schema path as stored in a version-3 artifact: project-relative when
/// the file lives under the root, absolute (flagged) when it does not.
fn portable_schema(root: &Path, schema: &Path) -> (String, bool) {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canonical_schema = schema
        .canonicalize()
        .unwrap_or_else(|_| schema.to_path_buf());
    match canonical_schema.strip_prefix(&canonical_root) {
        Ok(relative) => (relative.to_string_lossy().into_owned(), false),
        Err(_) => (canonical_schema.to_string_lossy().into_owned(), true),
    }
}

pub(super) fn persist_findings(
    root: &Path,
    schema: &Path,
    schema_sha256: &str,
    seed: u64,
    findings: Vec<FindingCase>,
    reset: &crate::domain::backend::BackendReset,
) -> Result<Vec<Value>> {
    let mut persisted = Vec::new();
    let mut seen = BTreeSet::new();
    for (endpoint, request, mut setup, mut finding) in findings {
        let fingerprint = finding
            .get("fingerprint")
            .and_then(Value::as_str)
            .context("backend finding has no fingerprint")?;
        if !seen.insert(fingerprint.to_string()) {
            continue;
        }
        // The id is derived from the ABSOLUTE discovering URL exactly as
        // before version 3, so portability changes storage, never identity.
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
        let mut request = request;
        let mut origin = None;
        for step in &mut setup {
            relativize_request(&mut step.request, &mut origin);
        }
        relativize_request(&mut request, &mut origin);
        let (schema_stored, schema_outside_root) = portable_schema(root, schema);
        let artifact = BackendFindingArtifact {
            format: "reproit-backend-finding".into(),
            version: 3,
            schema: schema_stored,
            schema_sha256: schema_sha256.into(),
            origin,
            schema_outside_root,
            reset_url: std::env::var("REPROIT_BACKEND_RESET_URL").ok(),
            reset: reset.clone(),
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
            "oracle": "contract",
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

/// Bound the rows printed to a terminal; `--json` always carries all of them.
const MAX_REPORTED_ROWS: usize = 20;

/// Render the coverage rows a run recorded. Reads them back off the report so
/// the text a user sees and the JSON an agent parses cannot disagree.
fn coverage_lines(report: &Value) -> Vec<String> {
    let Some(rows) = report["coverage"].as_array() else {
        return Vec::new();
    };
    let evaluated = rows.iter().filter(|row| row["evaluated"] == true).count();
    if rows.is_empty() || evaluated == rows.len() {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "  coverage: {evaluated}/{} declared operation(s) evaluated",
        rows.len()
    )];
    for (label, reached) in [("never sent", false), ("no success to evaluate", true)] {
        let group: Vec<&Value> = rows
            .iter()
            .filter(|row| row["evaluated"] != true && row["reached"] == reached)
            .collect();
        if group.is_empty() {
            continue;
        }
        lines.push(format!("    {label} ({}):", group.len()));
        for row in group.iter().take(MAX_REPORTED_ROWS) {
            lines.push(format!(
                "      {} {}{}",
                row["method"].as_str().unwrap_or(""),
                row["operation"].as_str().unwrap_or(""),
                coverage_detail(row)
            ));
        }
        if group.len() > MAX_REPORTED_ROWS {
            lines.push(format!(
                "      ... and {} more (use --json for the full table)",
                group.len() - MAX_REPORTED_ROWS
            ));
        }
    }
    lines
}

/// The one-line why: the reason it was skipped, or the status it kept returning
/// plus what the service said about it.
fn coverage_detail(row: &Value) -> String {
    if let Some(reason) = row["notSentReason"].as_str() {
        return format!(": {reason}");
    }
    let attempts = row["attempts"].as_u64().unwrap_or(0);
    if attempts == 0 {
        return ": every attempt failed to send".to_string();
    }
    let counts = if row["rateLimited"].as_u64() == Some(attempts) {
        format!("{attempts} attempt(s), all rate limited")
    } else {
        format!(
            "{attempts} attempt(s), last {}",
            row["lastStatus"].as_u64().unwrap_or(0)
        )
    };
    match row["lastBody"].as_str() {
        Some(body) => format!(": {counts} - {body}"),
        None => format!(": {counts}"),
    }
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
    if let Some(tier) = super::transport::adapter_tier_line() {
        ctx.say(format!("  {tier}"));
    }
    // What the run did NOT reach, before what it found. A findings count alone
    // reads as coverage, and an operation that 400'd every attempt evaluated no
    // contract at all.
    for line in coverage_lines(report) {
        ctx.say(line);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::backend_headless::coverage::Coverage;

    fn endpoint(id: &str, method: &str) -> Endpoint {
        let mut endpoint = super::super::openapi_endpoints(&json!({
            "openapi": "3.0.3",
            "paths": {"/x": {"get": {"operationId": id, "responses": {}}}}
        }))
        .pop()
        .expect("one endpoint");
        endpoint.method = method.to_string();
        endpoint
    }

    #[test]
    fn a_fully_evaluated_run_prints_no_coverage_warning() {
        let mut coverage = Coverage::new(&[endpoint("getUser", "GET")]);
        coverage.record("getUser", 200, &json!({"id": 1}));
        let report = json!({ "coverage": coverage.report() });
        assert!(
            coverage_lines(&report).is_empty(),
            "an honest aggregate needs no warning"
        );
    }

    #[test]
    fn the_summary_names_what_the_run_never_reached_or_evaluated() {
        // The reported case: a sweep that looks clean because every mutation
        // 400'd on a schema the service disagrees with, plus an operation scan
        // never sends at all.
        let mut coverage = Coverage::new(&[
            endpoint("getUser", "GET"),
            endpoint("blockUser", "POST"),
            endpoint("deletePost", "DELETE"),
        ]);
        coverage.record("getUser", 200, &json!({"id": 1}));
        coverage.record(
            "blockUser",
            400,
            &json!({"error": "blocked_type must be one of user, sponsor"}),
        );
        coverage.not_sent("deletePost", "scan executes read-only GET operations only");
        let lines = coverage_lines(&json!({ "coverage": coverage.report() })).join("\n");

        assert!(
            lines.contains("1/3 declared operation(s) evaluated"),
            "{lines}"
        );
        assert!(lines.contains("never sent (1)"), "{lines}");
        assert!(
            lines.contains("DELETE deletePost: scan executes read-only"),
            "{lines}"
        );
        assert!(lines.contains("no success to evaluate (1)"), "{lines}");
        // The body snippet is the whole point: it names the field the schema
        // got wrong, which is what the aggregate could never say.
        assert!(
            lines.contains("POST blockUser: 1 attempt(s), last 400")
                && lines.contains("blocked_type must be one of user, sponsor"),
            "{lines}"
        );
        assert!(
            !lines.contains("getUser"),
            "an evaluated operation is not noise: {lines}"
        );
    }

    #[test]
    fn an_all_429_operation_is_reported_as_rate_limited_not_as_a_plain_failure() {
        let mut coverage = Coverage::new(&[endpoint("listNearby", "GET")]);
        coverage.record("listNearby", 429, &Value::Null);
        coverage.record("listNearby", 429, &Value::Null);
        let lines = coverage_lines(&json!({ "coverage": coverage.report() })).join("\n");
        assert!(lines.contains("2 attempt(s), all rate limited"), "{lines}");
    }
}
