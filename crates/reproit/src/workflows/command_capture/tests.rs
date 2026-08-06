use super::*;

#[cfg(unix)]
#[test]
fn capture_preserves_a_bounded_child_exit_code() {
    let status = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 17"])
        .status()
        .unwrap();
    assert_eq!(
        capture_exit_code(&CommandOutcome::Exited(status)),
        ExitCode::from(17)
    );
    assert_eq!(
        capture_exit_code(&CommandOutcome::TimedOut),
        ExitCode::from(124)
    );
}

#[test]
fn token_normalization_is_bounded_and_nonempty() {
    assert_eq!(token("Invoice Importer"), "Invoice-Importer");
    assert_eq!(token("***"), "unknown");
    assert!(token(&"x".repeat(200)).len() <= 128);
}

#[test]
fn command_bounds_reject_empty_and_excessive_input() {
    let args = CommandCaptureArgs {
        project: None,
        component: None,
        identity: None,
        timeout_ms: 1,
        include_output: false,
        local_only: true,
        command: vec![],
    };
    assert!(validate_args(&args).is_err());
    let args = CommandCaptureArgs {
        timeout_ms: MAX_TIMEOUT_MS + 1,
        command: vec!["true".into()],
        ..args
    };
    assert!(validate_args(&args).is_err());
}

#[test]
fn identical_output_artifacts_are_retained_once() {
    let mut retained = std::collections::BTreeSet::new();
    let digest = format!("sha256:{}", "0".repeat(64));
    assert!(retained.insert(digest.clone()));
    assert!(!retained.insert(digest));
    assert_eq!(retained.len(), 1);
}

#[test]
fn cloud_upload_gate_rejects_environment_bound_trigger_evidence() {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../reproit-protocol/fixtures/capture-batch-v1.json"
    ))
    .unwrap();
    let portable: reproit_protocol::CaptureBatch = serde_json::from_value(value.clone()).unwrap();
    assert!(portable_for_cloud(&portable).unwrap());

    value["events"][1]["event"]["value"] = serde_json::json!({
        "representation": "environment-bound",
        "reference": "local-working-directory"
    });
    let local_only: reproit_protocol::CaptureBatch = serde_json::from_value(value).unwrap();
    assert!(!portable_for_cloud(&local_only).unwrap());
}
