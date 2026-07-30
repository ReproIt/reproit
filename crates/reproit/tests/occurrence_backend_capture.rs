#[cfg(unix)]
mod unix {
    use reproit_protocol::{
        backend_capture_from_batch, compile_capture_failure, CaptureAssessmentScope, CaptureBatch,
        ReproductionPackage, PACKAGE_VERSION,
    };
    use serde_json::{json, Value};
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Output};

    fn project_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "reproit-occurrence-backend-capture-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn run(root: &PathBuf, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_reproit"))
            .current_dir(root)
            .env("HOME", root.join("home"))
            .env_remove("REPROIT_CLOUD_KEY")
            .args(args)
            .output()
            .unwrap()
    }

    fn json(output: &Output) -> Value {
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid JSON: {error}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    /// A capture batch in the exact shape the Rust backend SDK emitter ships
    /// for a failed `createOrder`: operation-start, a trigger nesting the raw
    /// start input, effect events nesting the raw effects, the raw return
    /// event under the operation-return subject, operation-end, observation.
    fn sdk_batch() -> CaptureBatch {
        let raw_effect = json!({
            "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder", "actionIndex": 0,
            "operation": "createOrder", "sequence": 2,
            "kind": "effect", "effect": "read", "resource": "inventory", "key": "widget",
        });
        let raw_return = json!({
            "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder", "actionIndex": 0,
            "operation": "createOrder", "sequence": 3,
            "kind": "return", "output": {"error": "boom"},
            "status": 500, "success": false, "effectsComplete": true,
        });
        let events = [
            json!({"kind": "operation-start", "name": "createOrder"}),
            json!({
                "kind": "trigger", "trigger": "http-request", "subject": "createOrder",
                "value": {
                    "representation": "replayable",
                    "value": {"body": {"item": "widget", "qty": 2}},
                    "redaction": "redacted-at-source",
                },
            }),
            json!({
                "kind": "effect", "effect": "read", "subject": "inventory",
                "value": {
                    "representation": "replayable",
                    "value": raw_effect,
                    "redaction": "redacted-at-source",
                },
            }),
            json!({
                "kind": "effect", "effect": "operation-return", "subject": "operation-return",
                "value": {
                    "representation": "replayable",
                    "value": raw_return,
                    "redaction": "redacted-at-source",
                },
            }),
            json!({"kind": "operation-end", "name": "createOrder", "outcome": "failed"}),
            json!({
                "kind": "observation",
                "failure": {
                    "observation": "exception",
                    "authority": "runtime-diagnosis",
                    "summary": "backend operation createOrder returned HTTP 500",
                    "signature": "backend-server-error:createOrder",
                    "observationPoint": "createOrder",
                    "artifactIds": [],
                },
            }),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            let sequence = index as u64 + 1;
            json!({
                "id": format!("evt_backend-rust_{sequence}"),
                "sequence": sequence,
                "monotonicNs": sequence,
                "traceId": "cap-1-1",
                "causalParentIds": if sequence == 1 {
                    json!([])
                } else {
                    json!([format!("evt_backend-rust_{}", sequence - 1)])
                },
                "event": event,
            })
        })
        .collect::<Vec<_>>();
        let batch: CaptureBatch = serde_json::from_value(json!({
            "version": 1,
            "batchId": "cb-rust-1-1",
            "projectId": "app-demo",
            "sessionId": "cap-1-1",
            "emitter": {
                "id": "backend-rust",
                "kind": "runtime-sdk",
                "component": "backend",
                "runtime": "rust",
            },
            "observedAt": "1753747200000",
            "policy": {"consent": "application-telemetry", "retentionClass": "standard"},
            "capabilities": [],
            "events": events,
            "artifacts": [],
        }))
        .unwrap();
        batch.validate().unwrap();
        batch
    }

    #[test]
    fn backend_occurrence_replays_offline_from_its_projected_capture() {
        let root = project_root();
        fs::create_dir(root.join("home")).unwrap();

        // Persist the occurrence exactly as a Cloud pull would: the package
        // compiled from the batch, plus the projected backend capture.
        let batch = sdk_batch();
        let compiled = compile_capture_failure(
            &batch,
            "2026-07-30T00:00:00Z",
            CaptureAssessmentScope::Portable,
        )
        .unwrap()
        .expect("server-error batch compiles to an occurrence");
        let occurrence_id = compiled.occurrence.occurrence_id.clone();
        let mut package = ReproductionPackage {
            version: PACKAGE_VERSION,
            id: String::new(),
            occurrence: compiled.occurrence,
            assessment: compiled.assessment,
            plan: None,
            capsule: None,
            legacy: None,
        };
        package.finalize_id().unwrap();
        package.validate().unwrap();
        let directory = root.join(".reproit/occurrences").join(&occurrence_id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("package.json"),
            serde_json::to_vec_pretty(&package).unwrap(),
        )
        .unwrap();
        let projected =
            backend_capture_from_batch(&batch).expect("SDK batch projects to a backend capture");
        fs::write(
            directory.join("backend-capture.json"),
            serde_json::to_vec_pretty(&projected).unwrap(),
        )
        .unwrap();

        // `reproit occ_<id>` must route to the offline capture re-evaluation
        // and report the captured server error as a reproduced regression.
        let replay = run(&root, &["--json", "--yes", &occurrence_id]);
        assert_eq!(
            replay.status.code(),
            Some(1),
            "stderr:\n{}",
            String::from_utf8_lossy(&replay.stderr)
        );
        let report = json(&replay);
        assert_eq!(report["command"], "check");
        assert_eq!(report["outcome"], "fail");
        assert_eq!(report["capture"]["operation"], "createOrder");
        assert_eq!(report["capture"]["oracle"], "backend-server-error");
        assert_eq!(report["capture"]["reproduced"], true);
        assert_eq!(report["capture"]["events"], 3);
        fs::remove_dir_all(root).unwrap();
    }
}
