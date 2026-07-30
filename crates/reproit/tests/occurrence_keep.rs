#[cfg(unix)]
mod unix {
    use serde_json::Value;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Output};

    fn project_root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("reproit-occurrence-keep-{}", std::process::id()));
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

    #[test]
    fn empty_stream_occurrence_can_be_kept_and_checked_as_a_required_guard() {
        let root = project_root();
        fs::create_dir(root.join("home")).unwrap();
        let capture = run(
            &root,
            &[
                "--json",
                "--yes",
                "internal",
                "capture",
                "--local-only",
                "--include-output",
                "--",
                "/bin/sh",
                "-c",
                "exit 17",
            ],
        );
        assert_eq!(capture.status.code(), Some(17));
        let capture_json = json(&capture);
        let occurrence = capture_json["occurrenceId"].as_str().unwrap();
        assert_eq!(capture_json["streams"]["stdoutBytes"], 0);
        assert_eq!(capture_json["streams"]["stderrBytes"], 0);

        let keep = run(
            &root,
            &[
                "--json",
                "--yes",
                "keep",
                occurrence,
                "--as",
                "empty-stream-exit",
                "--strict",
            ],
        );
        assert!(
            keep.status.success(),
            "{:?}",
            String::from_utf8_lossy(&keep.stderr)
        );
        let keep_json = json(&keep);
        assert_eq!(keep_json["source"], "occurrence");
        assert_eq!(keep_json["status"], "required");
        let id = keep_json["id"].as_str().unwrap();
        let raw_id = id.strip_prefix("rep_").unwrap();
        let guard = root.join(".reproit/repros").join(raw_id);
        assert!(guard.join("package.json").is_file());
        assert!(guard.join("providers.yaml").is_file());
        fs::remove_dir_all(root.join(".reproit/private-providers")).unwrap();

        let check = run(&root, &["--json", "--yes", "check", id]);
        assert_eq!(check.status.code(), Some(1));
        assert_eq!(json(&check)["outcome"], "fail");
        fs::remove_dir_all(root).unwrap();
    }
}
