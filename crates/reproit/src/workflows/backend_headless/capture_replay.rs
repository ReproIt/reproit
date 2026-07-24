use super::*;
use crate::domain::backend::{Authority, IdempotencyResponseReplay};

/// The replayable payload the backend SDKs' production capture mode attaches
/// to a `backend-server-error` finding (`context.reproitCapture` on
/// `/v1/errors/:app`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CaptureArtifact {
    format: String,
    version: u16,
    pub(super) operation: String,
    pub(super) oracle: String,
    pub(super) events: Vec<BackendEvent>,
}

/// Parse and validate a captured-production payload. Shared by
/// `debug replay-capture` and backend inspection so both accept exactly the
/// same artifact.
pub(super) fn parse_capture(bytes: &[u8]) -> Result<CaptureArtifact> {
    let artifact: CaptureArtifact = serde_json::from_slice(bytes)
        .context("capture file is not a reproit-backend-capture payload")?;
    if artifact.format != "reproit-backend-capture" {
        bail!("unsupported capture format {:?}", artifact.format);
    }
    if artifact.version != 1 {
        bail!("unsupported capture version {}", artifact.version);
    }
    if artifact.events.is_empty() {
        bail!("capture has no events to replay");
    }
    Ok(artifact)
}

/// The deterministic re-evaluation shared by `debug replay-capture` and
/// `check <capture.json>`: parse the file, evaluate its events under the
/// synthesized capture contracts, and report whether the captured violation
/// still fires.
struct CaptureEvaluation {
    artifact: CaptureArtifact,
    findings: Vec<Value>,
    reproduced: bool,
}

fn evaluate_capture_file(file: &Path) -> Result<CaptureEvaluation> {
    let artifact =
        parse_capture(&std::fs::read(file).with_context(|| format!("read {}", file.display()))?)?;
    let operations = artifact
        .events
        .iter()
        .map(|event| event.operation.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(capture_contract)
        .collect();
    let config = BackendConfig {
        enabled: true,
        operations,
        ..BackendConfig::default()
    };
    let violations = backend::evaluate(&config, &artifact.events);
    let findings: Vec<Value> = violations.iter().map(backend::finding).collect();
    let reproduced = violations
        .iter()
        .zip(&findings)
        .any(|(violation, finding)| {
            violation.operation == artifact.operation
                && finding.get("oracle").and_then(Value::as_str) == Some(artifact.oracle.as_str())
        });
    Ok(CaptureEvaluation {
        artifact,
        findings,
        reproduced,
    })
}

/// Sniff whether `path` holds a `reproit-backend-capture` payload. Routing
/// only: an unreadable or non-capture file is simply "not a capture" here;
/// version/shape validation errors surface once the file is actually routed.
pub fn is_capture_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| {
            value
                .get("format")
                .map(|format| format == "reproit-backend-capture")
        })
        .unwrap_or(false)
}

/// `reproit debug replay-capture <file>`: deterministically re-evaluate a
/// captured production event sequence against the backend oracles and report
/// whether the captured violation reproduces. The capture is the witness, so
/// each operation gets a synthesized declared contract with an open input
/// domain; the oracle predicates themselves are unchanged.
pub fn replay_capture(ctx: &Ctx, file: &Path) -> Result<ExitCode> {
    let evaluation = evaluate_capture_file(file)?;
    let artifact = &evaluation.artifact;
    let report = json!({
        "command": "backend capture replay",
        "file": file.display().to_string(),
        "operation": artifact.operation,
        "oracle": artifact.oracle,
        "events": artifact.events.len(),
        "reproduced": evaluation.reproduced,
        "findings": evaluation.findings,
    });
    if ctx.json {
        ctx.emit(&report);
    } else if evaluation.reproduced {
        ctx.say(format!(
            "{}: reproduced exactly ({} on {})",
            file.display(),
            artifact.oracle,
            artifact.operation
        ));
    } else {
        ctx.say(format!("{}: no longer reproduces", file.display()));
    }
    Ok(if evaluation.reproduced {
        Exit::Regression.code()
    } else {
        ExitCode::SUCCESS
    })
}

/// `reproit check <capture.json>`: the same deterministic re-evaluation, under
/// `check`'s verdict contract. A capture that still reproduces is a real
/// regression (outcome `fail`, exit 1); one that no longer fires is `pass`.
pub fn check_capture(ctx: &Ctx, file: &Path) -> Result<ExitCode> {
    let evaluation = evaluate_capture_file(file)?;
    let artifact = &evaluation.artifact;
    let outcome = if evaluation.reproduced {
        repro::Outcome::Fail
    } else {
        repro::Outcome::Pass
    };
    ctx.emit(&json!({
        "command": "check",
        "capture": {
            "file": file.display().to_string(),
            "operation": artifact.operation,
            "oracle": artifact.oracle,
            "events": artifact.events.len(),
            "reproduced": evaluation.reproduced,
            "findings": evaluation.findings,
        },
        "outcome": outcome.as_str(),
        "exit": outcome.exit_code(),
    }));
    ctx.say(format!("check capture {}", file.display()));
    if evaluation.reproduced {
        ctx.say(format!(
            "  FAIL reproduced exactly ({} on {})",
            artifact.oracle, artifact.operation
        ));
    } else {
        ctx.say("  PASS no longer reproduces");
    }
    ctx.say(format!(
        "\ncheck: {} (1 capture)",
        outcome.as_str().to_uppercase()
    ));
    Ok(if evaluation.reproduced {
        Exit::Regression.code()
    } else {
        ExitCode::SUCCESS
    })
}

/// The synthesized declared contract a capture is evaluated under: the capture
/// itself is the witness, so the input domain is open and the oracle
/// predicates are unchanged. Inspection uses the same contract so its verdict
/// matches `debug replay-capture` exactly.
pub(super) fn capture_contract(id: String) -> OperationContract {
    OperationContract {
        id,
        authority: Authority::Declared,
        input: Some(ValueDomain::Any),
        output: None,
        outputs_by_status: BTreeMap::new(),
        success_statuses: Vec::new(),
        read_only: false,
        idempotent: false,
        idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
        tenant_isolated: false,
        promised_effects: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sdk_wire_capture() -> Value {
        json!({
            "format": "reproit-backend-capture",
            "version": 1,
            "operation": "createOrder",
            "oracle": "backend-server-error",
            "events": [
                {
                    "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder",
                    "actionIndex": 0, "operation": "createOrder", "sequence": 1,
                    "kind": "start",
                    "input": {"body": {"item": "widget", "discount": 5}}
                },
                {
                    "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder",
                    "actionIndex": 0, "operation": "createOrder", "sequence": 2,
                    "kind": "return", "output": {"error": "internal"},
                    "status": 500, "success": false, "effectsComplete": true
                }
            ]
        })
    }

    /// The shared file evaluation behind both `debug replay-capture` and
    /// `check <capture.json>`: one parse, one verdict.
    #[test]
    fn capture_file_evaluation_reproduces_the_captured_violation() {
        let dir = std::env::temp_dir().join(format!("reproit-cap-eval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("capture.json");
        std::fs::write(&file, sdk_wire_capture().to_string()).unwrap();
        let evaluation = evaluate_capture_file(&file).unwrap();
        assert!(evaluation.reproduced);
        assert_eq!(evaluation.artifact.oracle, "backend-server-error");
        assert!(!evaluation.findings.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The routing sniff accepts only the capture format marker; other JSON
    /// files (and missing files) are "not a capture", never an error.
    #[test]
    fn is_capture_file_sniffs_the_format_marker_only() {
        let dir = std::env::temp_dir().join(format!("reproit-cap-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let capture = dir.join("capture.json");
        std::fs::write(&capture, sdk_wire_capture().to_string()).unwrap();
        assert!(is_capture_file(&capture));
        let other = dir.join("other.json");
        std::fs::write(&other, "{\"format\":\"something-else\"}").unwrap();
        assert!(!is_capture_file(&other));
        let text = dir.join("notes.txt");
        std::fs::write(&text, "not json").unwrap();
        assert!(!is_capture_file(&text));
        assert!(!is_capture_file(&dir.join("missing.json")));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Pin the SDK wire shape end to end: events exactly as the Rust backend
    /// SDK serializes them (camelCase, flattened kind) must parse into
    /// `BackendEvent` and reproduce `backend-server-error` under the
    /// synthesized capture contract.
    #[test]
    fn sdk_wire_capture_reproduces_server_error() {
        let events: Vec<BackendEvent> = serde_json::from_value(json!([
            {
                "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder",
                "actionIndex": 0, "operation": "createOrder", "sequence": 1,
                "kind": "start",
                "input": {"body": {"item": "widget", "discount": 5}}
            },
            {
                "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder",
                "actionIndex": 0, "operation": "createOrder", "sequence": 2,
                "kind": "effect", "effect": "read",
                "resource": "inventory", "key": "widget"
            },
            {
                "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder",
                "actionIndex": 0, "operation": "createOrder", "sequence": 3,
                "kind": "return", "output": {"error": "internal"},
                "status": 500, "success": false, "effectsComplete": true
            }
        ]))
        .expect("SDK wire events parse into BackendEvent");
        let config = BackendConfig {
            enabled: true,
            operations: vec![capture_contract("createOrder".into())],
            ..BackendConfig::default()
        };
        let violations = backend::evaluate(&config, &events);
        let reproduced = violations.iter().any(|violation| {
            violation.operation == "createOrder"
                && backend::finding(violation)["oracle"] == "backend-server-error"
        });
        assert!(reproduced, "violations: {violations:?}");
    }
}
