use super::*;

/// The result of replaying one persisted finding artifact against the live
/// target: the three-state verdict plus the finding so a caller can label it.
/// `Fixed` is machine-checkable proof this defect is gone (the replay
/// re-exercises the exact failing request); `Inconclusive` means the run could
/// not evaluate the operation (unauthenticated, rate-limited, target down, or a
/// setup step that would not re-run) and must never be read as fixed.
pub(super) struct ReplayOutcome {
    pub verdict: ArtifactVerdict,
    pub finding: Value,
}

/// The finding id an artifact records, read without replaying it. Batch callers
/// filter on this first: naming an id must not re-send every other finding's
/// setup and failing request at the live target.
pub(super) fn artifact_finding_id(artifact_path: &Path) -> Result<String> {
    let document: Value = serde_json::from_slice(&std::fs::read(artifact_path)?)?;
    Ok(document
        .pointer("/finding/id")
        .and_then(Value::as_str)
        .unwrap_or("fnd_unknown")
        .to_string())
}

/// Replay a single finding artifact (backend.json or backend-schema.json) and
/// report its verdict. Shared by the direct `reproit <id>` form and the batch
/// `reproit verify` regression suite. The live-replay client picks up any
/// identity pool installed by the caller, so an auth-gated finding is replayed
/// authenticated rather than bouncing off a 401.
pub(super) async fn replay_artifact(
    artifact_path: &Path,
    current: &CurrentContracts,
) -> Result<ReplayOutcome> {
    if artifact_path.file_name().and_then(|value| value.to_str()) == Some("backend-schema.json") {
        let artifact: BackendSchemaFindingArtifact =
            serde_json::from_slice(&std::fs::read(artifact_path)?)?;
        if !(1..=2).contains(&artifact.version) {
            bail!(
                "unsupported backend schema artifact version {}; this reproit is older than the \
                 artifact",
                artifact.version
            );
        }
        let schema = resolve_artifact_schema(
            artifact_path,
            &artifact.schema,
            artifact.version >= 2 && !artifact.schema_outside_root,
        )?;
        let document = load_document(&schema)?;
        // A static schema check is deterministic: it either reproduces or not.
        // Retraction does not apply here, because the schema IS the subject:
        // editing it away is a real fix of the recorded defect, not a withdrawn
        // claim about something else.
        let verdict = if backend::validate_openapi_parameter_uniqueness(&document)
            .iter()
            .any(|value| value.fingerprint == artifact.violation.fingerprint)
        {
            ArtifactVerdict::Reproduced
        } else {
            ArtifactVerdict::Fixed
        };
        return Ok(ReplayOutcome {
            verdict,
            finding: artifact.finding,
        });
    }
    let mut artifact: BackendFindingArtifact =
        serde_json::from_slice(&std::fs::read(artifact_path)?)?;
    if !(1..=3).contains(&artifact.version) {
        bail!(
            "unsupported backend finding artifact version {}; this reproit is older than the \
             artifact",
            artifact.version
        );
    }
    if artifact.version >= 3 {
        // Version 3 stores origin-relative URLs plus the discovering origin,
        // so the artifact replays from any checkout location. The current
        // target wins (REPROIT_BACKEND_URL); the recorded origin is the
        // fallback; with neither, fail with the exact next input.
        let base = std::env::var("REPROIT_BACKEND_URL")
            .ok()
            .or_else(|| artifact.origin.clone());
        for step in artifact.setup.iter_mut() {
            resolve_relative_url(&mut step.request.url, base.as_deref())?;
        }
        resolve_relative_url(&mut artifact.failing.request.url, base.as_deref())?;
    } else {
        // Older artifacts record ABSOLUTE request URLs from the discovering
        // run. When the current run names a live target (REPROIT_BACKEND_URL,
        // which wins everywhere else too), rebase the recorded requests onto
        // it: a guard found against one ephemeral booted port must replay
        // against the port this run booted, not against a dead one.
        if let Ok(base) = std::env::var("REPROIT_BACKEND_URL") {
            if let Ok(base) = base.parse::<reqwest::Url>() {
                for step in artifact.setup.iter_mut() {
                    if let Some(url) = retarget_url(&step.request.url, &base) {
                        step.request.url = url;
                    }
                }
                if let Some(url) = retarget_url(&artifact.failing.request.url, &base) {
                    artifact.failing.request.url = url;
                }
            }
        }
    }
    let operation = artifact.failing.contract.id.clone();
    let status = current.status(&artifact.failing);
    // An operation the schema no longer declares cannot be replayed into either
    // a live defect or a proof of fix, so it is retracted without sending the
    // request at all.
    if matches!(status, ContractStatus::Absent) {
        return Ok(ReplayOutcome {
            verdict: ArtifactVerdict::Retracted(retraction::absent_reason(&operation)),
            finding: artifact.finding,
        });
    }
    // Re-establish the SAME preconditions the finding was found under before
    // replaying it. A finding reproduced from a different starting state is not
    // the same finding, and a fix "proven" from one is not proven.
    let client_for_reset = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    super::reset::run_reset_quiet(&client_for_reset, &artifact.reset).await?;
    let expected = artifact
        .finding
        .get("fingerprint")
        .and_then(Value::as_str)
        .context("backend artifact has no finding fingerprint")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let expected_oracle = artifact.finding.get("kind").and_then(Value::as_str);
    let endpoint = replay_endpoint(&artifact.failing);
    let verdict = replay_sequence(
        &client,
        &artifact.setup,
        &endpoint,
        &artifact.failing.request,
        expected,
        artifact.reset_url.as_deref(),
        expected_oracle,
    )
    .await?;
    // It still reproduces against the contract it was recorded under. If that
    // contract has since been edited, re-check against what the project asserts
    // now: the same response can stop being a violation because the claim moved
    // rather than because the server did.
    let recheck = match (verdict, status) {
        (ReplayVerdict::Reproduced, ContractStatus::Changed(claim)) => {
            let mut step = artifact.failing.clone();
            step.contract = claim.contract;
            step.policy = claim.policy;
            let current_endpoint = replay_endpoint(&step);
            Some(
                replay_sequence(
                    &client,
                    &artifact.setup,
                    &current_endpoint,
                    &artifact.failing.request,
                    expected,
                    artifact.reset_url.as_deref(),
                    expected_oracle,
                )
                .await?,
            )
        }
        _ => None,
    };
    let verdict = artifact_verdict(verdict, recheck, &operation);
    Ok(ReplayOutcome {
        verdict,
        finding: artifact.finding,
    })
}

/// Translate a replay verdict into the artifact verdict the guard surfaces.
///
/// `recheck` is the verdict of a second replay under the contract as the project
/// asserts it TODAY, and is present only when the first replay reproduced and the
/// contract has since changed. Only an evaluable non-reproduction there retracts:
/// a re-check that could not be evaluated leaves the blocking verdict standing,
/// so a flaky or unreachable run can never retract a live bug.
pub(crate) fn artifact_verdict(
    replay: ReplayVerdict,
    recheck: Option<ReplayVerdict>,
    operation: &str,
) -> ArtifactVerdict {
    match replay {
        ReplayVerdict::Fixed => ArtifactVerdict::Fixed,
        ReplayVerdict::Inconclusive => ArtifactVerdict::Inconclusive,
        ReplayVerdict::Reproduced => match recheck {
            Some(ReplayVerdict::Fixed) => {
                ArtifactVerdict::Retracted(retraction::changed_reason(operation))
            }
            _ => ArtifactVerdict::Reproduced,
        },
    }
}

pub async fn try_replay(ctx: &Ctx, id: &str) -> Result<Option<ExitCode>> {
    // A kept guard is addressed by its rpr_ id; the pending finding by fnd_.
    // Both name the same content hash, so both replay the same artifact.
    let Some(raw_id) = repro::raw_finding_id(id).or_else(|| repro::raw_repro_id(id)) else {
        return Ok(None);
    };
    let Some(artifact_path) = find_artifact(raw_id)? else {
        return Ok(None);
    };
    let outcome = replay_artifact(&artifact_path, &CurrentContracts::load(None)).await?;
    let (state, message) = match &outcome.verdict {
        ArtifactVerdict::Reproduced => ("reproduced", format!("{id}: reproduced exactly")),
        ArtifactVerdict::Fixed => ("fixed", format!("{id}: no longer reproduces")),
        ArtifactVerdict::Inconclusive => (
            "inconclusive",
            format!("{id}: could not verify (unauthenticated, rate-limited, or target down)"),
        ),
        ArtifactVerdict::Retracted(reason) => (
            "retracted",
            format!("{id}: retracted, {reason} (this is not a proof of fix)"),
        ),
    };
    let report = json!({
        "command": "backend replay",
        "id": id,
        "state": state,
        "reproduced": outcome.verdict == ArtifactVerdict::Reproduced,
        "finding": outcome.finding,
    });
    if ctx.json {
        ctx.emit(&report);
    } else {
        ctx.say(message);
    }
    // Only a proven Fixed is a pass; Reproduced and Inconclusive both fail closed,
    // so a replay that could not evaluate never reports success. Retracted does
    // not block: the claim was withdrawn by an explicit schema edit, and holding
    // the finding open would leave no way to close it but deleting it by hand.
    Ok(Some(if outcome.verdict.blocks() {
        Exit::Regression.code()
    } else {
        ExitCode::SUCCESS
    }))
}

/// Rewrite a recorded absolute URL onto the current target's origin, keeping
/// its path and query untouched. Returns None (leave the URL alone) when
/// either side does not parse.
/// Resolve one version-3 origin-relative URL against the effective base. An
/// absolute URL (a cross-origin step the writer left alone) passes through
/// untouched; a relative URL with no base at all is an error naming the exact
/// next input rather than a guess.
fn resolve_relative_url(url: &mut String, base: Option<&str>) -> Result<()> {
    if !url.starts_with('/') {
        return Ok(());
    }
    let Some(base) = base else {
        bail!(
            "this artifact stores origin-relative request URLs but records no origin; set \
             REPROIT_BACKEND_URL to the live target to replay it"
        );
    };
    *url = format!("{}{}", base.trim_end_matches('/'), url);
    Ok(())
}

/// Resolve a stored schema path for replay. A version-2 schema artifact keeps
/// the path PROJECT-RELATIVE, so it is resolved against the project root that
/// owns the artifact (the artifact sits under the findings directory, three
/// levels below the root), which is what lets a moved checkout replay. Version
/// 1, and any version-2 artifact whose schema genuinely lives outside the
/// root, stored an absolute path and is used as written.
fn resolve_artifact_schema(
    artifact_path: &Path,
    stored: &str,
    project_relative: bool,
) -> Result<PathBuf> {
    let stored_path = Path::new(stored);
    if !project_relative || stored_path.is_absolute() {
        return Ok(stored_path.to_path_buf());
    }
    let root = artifact_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .with_context(|| {
            format!(
                "{} is not inside a {}/<id>/ directory, so its project-relative schema path \
                 cannot be resolved",
                artifact_path.display(),
                layout::findings_dir_rel()
            )
        })?;
    Ok(root.join(stored_path))
}

fn retarget_url(recorded: &str, base: &reqwest::Url) -> Option<String> {
    let mut url: reqwest::Url = recorded.parse().ok()?;
    url.set_scheme(base.scheme()).ok()?;
    url.set_host(base.host_str()).ok()?;
    url.set_port(base.port()).ok()?;
    Some(url.into())
}

pub(super) fn replay_endpoint(step: &ReplayStep) -> Endpoint {
    let graphql = step
        .request
        .body
        .as_ref()
        .is_some_and(|body| body.get("query").is_some());
    Endpoint {
        method: step.request.method.clone(),
        path: String::new(),
        body_only: step.request.body.is_some(),
        content_type: step.request.content_type.clone(),
        response_field: graphql.then(|| step.contract.id.clone()),
        policy: step.policy.clone(),
        transport: if step.request.method == "GRPC" {
            Transport::Grpc
        } else {
            Transport::Http
        },
        schema_source: step.request.schema_source.clone(),
        client_streaming: step.request.client_streaming,
        server_streaming: step.request.server_streaming,
        contract: step.contract.clone(),
    }
}

pub(super) fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

/// Reset the target before a replay. `recorded` is the reset endpoint stored on
/// the artifact being replayed; the environment override still wins so an
/// operator can redirect a whole run, but with no override each artifact resets
/// against its own recorded endpoint instead of whichever one a batch happened
/// to read first.
pub(super) async fn maybe_reset_target(
    client: &reqwest::Client,
    failing_url: &str,
    recorded: Option<&str>,
) -> Result<()> {
    let (reset, source) = match std::env::var("REPROIT_BACKEND_RESET_URL") {
        Ok(value) => (value, "REPROIT_BACKEND_RESET_URL"),
        Err(_) => match recorded {
            Some(value) => (value.to_string(), "the artifact's recorded reset url"),
            None => {
                // A server this run booted itself: a full process restart is
                // the clean-state reset (there is no URL to demand for it).
                if crate::workflows::backend_learn::boot::process_reset_installed() {
                    return crate::workflows::backend_learn::boot::run_process_reset().await;
                }
                return Ok(());
            }
        },
    };
    validate_base_url(&reset).context(source)?;
    let failing = failing_url.parse::<reqwest::Url>()?;
    let reset_url = reset.parse::<reqwest::Url>()?;
    if failing.origin() != reset_url.origin() {
        bail!("{source} must use the same origin as the replay target");
    }
    let response = client.post(reset_url).send().await?;
    if !response.status().is_success() {
        bail!("backend reset returned {}", response.status());
    }
    Ok(())
}

pub(super) fn find_artifact(raw_id: &str) -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir()?;
    for root in cwd.ancestors() {
        // The committed guard store first (it survives a fresh checkout),
        // then the local findings store the fuzz run wrote.
        for directory in [
            crate::domain::repro::repro_dir(root, raw_id),
            layout::finding_dir(root, raw_id),
        ] {
            for name in ["backend.json", "backend-schema.json"] {
                let artifact = directory.join(name);
                if artifact.is_file() {
                    return Ok(Some(artifact));
                }
            }
        }
    }
    Ok(None)
}

/// Every kept hermetic capture guard under `.reproit/repros/`: a capture plus
/// the user-authored exec recipe (hermetic.json is repo config, like a
/// package.json script; the capture itself never supplies the command).
/// Sorted so batch reports are stable. Shared by the check gate and verify.
pub(super) fn hermetic_guards(root: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut guards = Vec::new();
    if let Ok(entries) = std::fs::read_dir(layout::repros_dir(root)) {
        for entry in entries.flatten() {
            let directory = entry.path();
            let capture = directory.join("capture.json");
            let recipe = directory.join("hermetic.json");
            if capture.is_file() && recipe.is_file() {
                guards.push((capture, recipe));
            }
        }
    }
    guards.sort();
    guards
}

/// Replay every KEPT backend guard (`.reproit/repros/<id>/backend*.json`)
/// against the live target: the committed regression suite behind `reproit
/// check` in a backend project. Returns None when no backend guards are kept.
/// A guard passes only on a proven Fixed (or an explicitly retracted claim);
/// Reproduced means the bug is back and Inconclusive fails closed.
pub async fn replay_kept_guards(ctx: &Ctx, root: &Path) -> Result<Option<ExitCode>> {
    let hermetic_guards = hermetic_guards(root);
    let mut artifacts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(layout::repros_dir(root)) {
        for entry in entries.flatten() {
            let directory = entry.path();
            // A hermetic guard directory replays through its recipe above,
            // never additionally as a live artifact.
            if directory.join("capture.json").is_file() && directory.join("hermetic.json").is_file()
            {
                continue;
            }
            for name in ["backend.json", "backend-schema.json"] {
                let artifact = directory.join(name);
                if artifact.is_file() {
                    artifacts.push(artifact);
                    break;
                }
            }
        }
    }
    if artifacts.is_empty() && hermetic_guards.is_empty() {
        return Ok(None);
    }
    artifacts.sort();
    let mut hermetic_failed = 0usize;
    let mut quarantined = 0usize;
    for (capture, recipe) in &hermetic_guards {
        let raw_id = capture
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        let label = repro::display_repro_id(raw_id);
        let Some(exec) = super::hermetic::guard_exec(recipe) else {
            hermetic_failed += 1;
            ctx.say(format!(
                "  guard {label}: hermetic.json has no `exec` command; fails closed"
            ));
            continue;
        };
        let outcome = super::hermetic::run_hermetic(capture, &exec).await?;
        match outcome.verdict {
            super::hermetic::HermeticVerdict::Fixed => {
                ctx.say(format!(
                    "  guard {label}: held (hermetic re-execution clean)"
                ));
            }
            super::hermetic::HermeticVerdict::Reproduced => {
                hermetic_failed += 1;
                ctx.say(format!(
                    "  guard {label}: REPRODUCED hermetically (the bug is back)"
                ));
            }
            super::hermetic::HermeticVerdict::Diverged => {
                // Drift, not regression: the code no longer makes the captured
                // calls. Quarantine (report, never block) so a refactor cannot
                // turn the gate red; the guard needs a re-capture to matter
                // again.
                quarantined += 1;
                ctx.say(format!(
                    "  guard {label}: DRIFTED (quarantined, not blocking): the code's outbound \
                     calls changed; re-capture to re-arm this guard"
                ));
                for report in &outcome.divergences {
                    ctx.say(format!("    {report}"));
                }
            }
            super::hermetic::HermeticVerdict::Inconclusive => {
                hermetic_failed += 1;
                ctx.say(format!(
                    "  guard {label}: could not re-execute (boot or answer failed); fails closed"
                ));
            }
        }
    }
    if artifacts.is_empty() {
        ctx.say(format!(
            "  guards: {} hermetic, {} failing, {} drifted (quarantined)",
            hermetic_guards.len(),
            hermetic_failed,
            quarantined
        ));
        return Ok(Some(if hermetic_failed > 0 {
            Exit::Regression.code()
        } else {
            ExitCode::SUCCESS
        }));
    }
    let current = CurrentContracts::load(None);
    let mut failed = 0usize;
    for artifact in &artifacts {
        let raw_id = artifact
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        let label = repro::display_repro_id(raw_id);
        let outcome = replay_artifact(artifact, &current).await?;
        match &outcome.verdict {
            ArtifactVerdict::Reproduced => {
                failed += 1;
                ctx.say(format!("  guard {label}: REPRODUCED (the bug is back)"));
            }
            ArtifactVerdict::Inconclusive => {
                failed += 1;
                ctx.say(format!(
                    "  guard {label}: could not verify (unauthenticated, rate-limited, or \
                     target down); fails closed"
                ));
            }
            ArtifactVerdict::Fixed => {
                ctx.say(format!("  guard {label}: held (does not reproduce)"));
            }
            ArtifactVerdict::Retracted(reason) => {
                ctx.say(format!("  guard {label}: retracted, {reason}"));
            }
        }
    }
    ctx.say(format!(
        "  guards: {} kept, {} failing, {} hermetic ({} failing, {} drifted)",
        artifacts.len(),
        failed,
        hermetic_guards.len(),
        hermetic_failed,
        quarantined
    ));
    Ok(Some(if failed + hermetic_failed > 0 {
        Exit::Regression.code()
    } else {
        ExitCode::SUCCESS
    }))
}

pub(super) fn value_as_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::retarget_url;

    #[test]
    fn recorded_urls_rebase_onto_the_current_target_origin() {
        // A guard found against one ephemeral booted port must replay against
        // the port THIS run booted; path and query stay untouched.
        let base: reqwest::Url = "http://127.0.0.1:60123".parse().unwrap();
        assert_eq!(
            retarget_url("http://127.0.0.1:54217/items?limit=2", &base).as_deref(),
            Some("http://127.0.0.1:60123/items?limit=2")
        );
        // An unparseable recorded URL is left alone rather than guessed at.
        assert_eq!(retarget_url("not a url", &base), None);
    }
}
