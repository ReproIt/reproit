// A self-describing backend capture reproduces with no project config at all.
//
// `reproit check <capture.json>` must route on the capture's own format marker,
// not on a reproit.yaml being present. This is what makes reproducing a shared
// capture a genuine one-command action in a fresh directory: install the CLI,
// run the command, get the verdict. Regressing it (re-adding a project gate)
// silently breaks every "here, try it" hand-off.
#[cfg(unix)]
mod unix {
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn empty_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("reproit-bare-capture-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn check(dir: &PathBuf, file: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_reproit"))
            .current_dir(dir)
            .env("HOME", dir.join("home"))
            .args(["check", file])
            .output()
            .unwrap()
    }

    fn capture(status: u16, success: bool) -> serde_json::Value {
        json!({
            "format": "reproit-backend-capture",
            "version": 1,
            "operation": "submitRun",
            "oracle": "backend-server-error",
            "events": [
                {
                    "traceId": "t-1", "spanId": "t-1:submitRun", "actionIndex": 0,
                    "operation": "submitRun", "sequence": 1, "kind": "start",
                    "input": {"body": {"pipeline": "nightly-etl"}}
                },
                {
                    "traceId": "t-1", "spanId": "t-1:submitRun", "actionIndex": 0,
                    "operation": "submitRun", "sequence": 2, "kind": "return",
                    "output": {"error": "boom"}, "status": status,
                    "success": success, "effectsComplete": true
                }
            ]
        })
    }

    #[test]
    fn bare_capture_reproduces_a_server_error_without_any_config() {
        let dir = empty_dir("fail");
        fs::write(dir.join("occurrence.json"), capture(500, false).to_string()).unwrap();
        let out = check(&dir, "occurrence.json");
        let code = out.status.code().unwrap_or(-1);
        assert_eq!(
            code,
            1,
            "a bare 500 capture must reproduce (exit 1) with no reproit.yaml.\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn bare_capture_of_a_success_passes_without_any_config() {
        let dir = empty_dir("pass");
        fs::write(dir.join("occurrence.json"), capture(202, true).to_string()).unwrap();
        let out = check(&dir, "occurrence.json");
        assert_eq!(
            out.status.code().unwrap_or(-1),
            0,
            "a clean capture must pass (exit 0) with no reproit.yaml.\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = fs::remove_dir_all(dir);
    }
}
