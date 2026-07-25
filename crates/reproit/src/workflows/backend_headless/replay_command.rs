use super::*;

/// The result of replaying one persisted finding artifact against the live
/// target: the three-state verdict plus the finding so a caller can label it.
/// `Fixed` is machine-checkable proof this defect is gone (the replay
/// re-exercises the exact failing request); `Inconclusive` means the run could
/// not evaluate the operation (unauthenticated, rate-limited, target down, or a
/// setup step that would not re-run) and must never be read as fixed.
pub(super) struct ReplayOutcome {
    pub verdict: ReplayVerdict,
    pub finding: Value,
}

/// Replay a single finding artifact (backend.json or backend-schema.json) and
/// report its verdict. Shared by the direct `reproit <id>` form and the batch
/// `reproit verify` regression suite. The live-replay client picks up any
/// identity pool installed by the caller, so an auth-gated finding is replayed
/// authenticated rather than bouncing off a 401.
pub(super) async fn replay_artifact(artifact_path: &Path) -> Result<ReplayOutcome> {
    if artifact_path.file_name().and_then(|value| value.to_str()) == Some("backend-schema.json") {
        let artifact: BackendSchemaFindingArtifact =
            serde_json::from_slice(&std::fs::read(artifact_path)?)?;
        let schema = Path::new(&artifact.schema);
        let document = load_document(schema)?;
        // A static schema check is deterministic: it either reproduces or not.
        let verdict = if backend::validate_openapi_parameter_uniqueness(&document)
            .iter()
            .any(|value| value.fingerprint == artifact.violation.fingerprint)
        {
            ReplayVerdict::Reproduced
        } else {
            ReplayVerdict::Fixed
        };
        return Ok(ReplayOutcome {
            verdict,
            finding: artifact.finding,
        });
    }
    let artifact: BackendFindingArtifact = serde_json::from_slice(&std::fs::read(artifact_path)?)?;
    if std::env::var_os("REPROIT_BACKEND_RESET_URL").is_none() {
        if let Some(reset_url) = &artifact.reset_url {
            std::env::set_var("REPROIT_BACKEND_RESET_URL", reset_url);
        }
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
    )
    .await?;
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
    let outcome = replay_artifact(&artifact_path).await?;
    let (state, message) = match outcome.verdict {
        ReplayVerdict::Reproduced => ("reproduced", format!("{id}: reproduced exactly")),
        ReplayVerdict::Fixed => ("fixed", format!("{id}: no longer reproduces")),
        ReplayVerdict::Inconclusive => (
            "inconclusive",
            format!("{id}: could not verify (unauthenticated, rate-limited, or target down)"),
        ),
    };
    let report = json!({
        "command": "backend replay",
        "id": id,
        "state": state,
        "reproduced": outcome.verdict == ReplayVerdict::Reproduced,
        "finding": outcome.finding,
    });
    if ctx.json {
        ctx.emit(&report);
    } else {
        ctx.say(message);
    }
    // Only a proven Fixed is a pass; Reproduced and Inconclusive both fail closed,
    // so a replay that could not evaluate never reports success.
    Ok(Some(if outcome.verdict == ReplayVerdict::Fixed {
        ExitCode::SUCCESS
    } else {
        Exit::Regression.code()
    }))
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

pub(super) async fn maybe_reset_target(client: &reqwest::Client, failing_url: &str) -> Result<()> {
    let Some(reset) = std::env::var_os("REPROIT_BACKEND_RESET_URL") else {
        return Ok(());
    };
    let reset = reset.to_string_lossy();
    validate_base_url(&reset).context("REPROIT_BACKEND_RESET_URL")?;
    let failing = failing_url.parse::<reqwest::Url>()?;
    let reset_url = reset.parse::<reqwest::Url>()?;
    if failing.origin() != reset_url.origin() {
        bail!("REPROIT_BACKEND_RESET_URL must use the same origin as the replay target");
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
