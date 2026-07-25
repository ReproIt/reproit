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
        let schema = Path::new(&artifact.schema);
        let document = load_document(schema)?;
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
    let artifact: BackendFindingArtifact = serde_json::from_slice(&std::fs::read(artifact_path)?)?;
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
    let expected = artifact
        .finding
        .get("fingerprint")
        .and_then(Value::as_str)
        .context("backend artifact has no finding fingerprint")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let endpoint = replay_endpoint(&artifact.failing);
    let verdict = replay_sequence(
        &client,
        &artifact.setup,
        &endpoint,
        &artifact.failing.request,
        expected,
        artifact.reset_url.as_deref(),
    )
    .await?;
    let verdict = match verdict {
        ReplayVerdict::Fixed => ArtifactVerdict::Fixed,
        ReplayVerdict::Inconclusive => ArtifactVerdict::Inconclusive,
        // It still reproduces against the contract it was recorded under. If
        // that contract has since been edited, re-check against what the project
        // asserts now: the same response can stop being a violation because the
        // claim moved rather than because the server did.
        ReplayVerdict::Reproduced => match status {
            ContractStatus::Changed(claim) => {
                let mut step = artifact.failing.clone();
                step.contract = claim.contract;
                step.policy = claim.policy;
                let current_endpoint = replay_endpoint(&step);
                let recheck = replay_sequence(
                    &client,
                    &artifact.setup,
                    &current_endpoint,
                    &artifact.failing.request,
                    expected,
                    artifact.reset_url.as_deref(),
                )
                .await?;
                // Only an evaluable non-reproduction retracts. A re-check that
                // could not be evaluated leaves the blocking verdict standing,
                // so a flaky or unreachable run can never retract a live bug.
                if recheck == ReplayVerdict::Fixed {
                    ArtifactVerdict::Retracted(retraction::changed_reason(&operation))
                } else {
                    ArtifactVerdict::Reproduced
                }
            }
            _ => ArtifactVerdict::Reproduced,
        },
    };
    Ok(ReplayOutcome {
        verdict,
        finding: artifact.finding,
    })
}

pub async fn try_replay(ctx: &Ctx, id: &str) -> Result<Option<ExitCode>> {
    let Some(raw_id) = repro::raw_finding_id(id) else {
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
    Ok(Some(
        if matches!(
            outcome.verdict,
            ArtifactVerdict::Fixed | ArtifactVerdict::Retracted(_)
        ) {
            ExitCode::SUCCESS
        } else {
            Exit::Regression.code()
        },
    ))
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
            None => return Ok(()),
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
        let directory = layout::finding_dir(root, raw_id);
        for name in ["backend.json", "backend-schema.json"] {
            let artifact = directory.join(name);
            if artifact.is_file() {
                return Ok(Some(artifact));
            }
        }
    }
    Ok(None)
}

pub(super) fn value_as_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}
