use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PLATFORMS: &[&str] = &[
    "flutter-ios-sim",
    "web",
    "rn-appium",
    "electron",
    "tauri",
    "swift-ios",
    "android",
    "swift-macos",
    "winui",
    "tui",
    "qt",
    "gtk",
    "avalonia",
    "wxwidgets",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn reproit_bin() -> PathBuf {
    option_env!("CARGO_BIN_EXE_reproit")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/debug/reproit"))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "reproit-framework-smoke-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn common() -> &'static str {
    "devices:\n  namePrefix: Smoke\njourneys:\n  driver: \"\"\n  doneMarkers: [\"All tests \
     passed\"]\n"
}

fn config_for(platform: &str) -> String {
    let root = repo_root();
    let runners = root.join("runners").display().to_string();
    let web_runner = root.join("runners/web").display().to_string();
    match platform {
        "flutter-ios-sim" => format!(
            concat!(
                "app:\n  platform: flutter-ios-sim\n",
                "  projectDir: frontend\n  bundleId: com.example.smoke\n{}"
            ),
            common()
        ),
        "web" => format!(
            "app:\n  platform: web\n  webRunnerDir: {web_runner}\n  url: http://127.0.0.1:9\n{}",
            common()
        ),
        "rn-appium" => format!(
            concat!(
                "app:\n  platform: rn-appium\n  rnRunnerDir: {}/runners/rn\n",
                "  appiumUrl: http://127.0.0.1:4723\n  appiumCaps:\n",
                "    platformName: Android\n",
                "    appium:automationName: UiAutomator2\n",
                "    appium:app: ./app-debug.apk\n{}"
            ),
            root.display(),
            common()
        ),
        "swift-ios" => format!(
            concat!(
                "app:\n  platform: swift-ios\n",
                "  appiumUrl: http://127.0.0.1:4723\n  appiumCaps:\n",
                "    platformName: iOS\n",
                "    appium:automationName: XCUITest\n",
                "    appium:bundleId: com.example.smoke\n{}"
            ),
            common()
        ),
        "android" => format!(
            concat!(
                "app:\n  platform: android\n",
                "  appiumUrl: http://127.0.0.1:4723\n  appiumCaps:\n",
                "    platformName: Android\n",
                "    appium:automationName: UiAutomator2\n",
                "    appium:app: ./app-debug.apk\n{}"
            ),
            common()
        ),
        "electron" | "tauri" | "tui" | "qt" | "gtk" | "avalonia" | "wxwidgets" => format!(
            concat!(
                "app:\n  platform: {platform}\n",
                "  executable: ./missing-smoke-target\n",
                "  runnerDir: {runners}\n{}"
            ),
            common(),
            platform = platform,
            runners = runners
        ),
        "swift-macos" => format!(
            concat!(
                "app:\n  platform: swift-macos\n",
                "  executable: /Applications/MissingSmokeTarget.app\n",
                "  bundleId: com.example.smoke\n",
                "  runnerDir: {runners}\n{}"
            ),
            common(),
            runners = runners
        ),
        "winui" => format!(
            concat!(
                "app:\n  platform: winui\n",
                "  executable: C:/MissingSmokeTarget/Smoke.exe\n",
                "  runnerDir: {runners}\n{}"
            ),
            common(),
            runners = runners
        ),
        other => panic!("missing config template for {other}"),
    }
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> (bool, String, String, Option<i32>) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn reproit");
    let start = Instant::now();
    loop {
        if let Some(_status) = child.try_wait().expect("poll child") {
            let out = child.wait_with_output().expect("collect output");
            return (
                false,
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                out.status.code(),
            );
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let out = child.wait_with_output().expect("collect killed output");
            return (
                true,
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                None,
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn scan_help_is_bounded() {
    let mut cmd = Command::new(reproit_bin());
    cmd.arg("scan").arg("--help");
    let (timed_out, stdout, stderr, code) = run_with_timeout(cmd, Duration::from_secs(5));
    assert!(!timed_out, "scan --help timed out\n{stdout}\n{stderr}");
    assert_eq!(code, Some(0), "scan --help failed\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("scan") || stdout.contains("Scan"),
        "{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn fuzz_command_reports_artifact_and_guard_state_without_aggregate_cause() {
    let root = repo_root();
    let dir = temp_dir("fuzz-output-contract");
    let config = dir.join("reproit.yaml");
    fs::copy(
        root.join("validation/release/tui-output-contract.yaml"),
        &config,
    )
    .unwrap();

    let mut cmd = Command::new(reproit_bin());
    cmd.env("REPROIT_ROOT", &root)
        .arg("--config")
        .arg(&config)
        .arg("--json")
        .arg("--yes")
        .arg("fuzz")
        .arg("--runs")
        .arg("1")
        .arg("--budget")
        .arg("1")
        .arg("--uniform");
    let (timed_out, stdout, stderr, code) = run_with_timeout(cmd, Duration::from_secs(60));
    assert!(
        !timed_out,
        "fuzz output contract timed out\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(
        code,
        Some(0),
        "fuzz output contract failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let output: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output["confirmedFindings"], 1);
    assert_eq!(output["findings"].as_array().map(Vec::len), Some(1));
    assert_eq!(output["findings"][0]["findingArtifactSaved"], true);
    assert_eq!(output["findings"][0]["regressionGuardKept"], false);
    assert!(output.get("cause").is_none());
    assert!(output.get("regressionSaved").is_none());
    assert!(stderr.contains("Finding artifact saved: yes"), "{stderr}");
    assert!(stderr.contains("Regression guard kept: no"), "{stderr}");
}

#[test]
fn clean_fuzz_json_does_not_leak_a_historical_finding() {
    let root = repo_root();
    let dir = temp_dir("fuzz-no-stale-finding");
    let config = dir.join("reproit.yaml");
    fs::copy(
        root.join("validation/release/tui-clean-output-contract.yaml"),
        &config,
    )
    .unwrap();
    let old_run = dir.join(".reproit/runs/old-finding");
    fs::create_dir_all(&old_run).unwrap();
    fs::write(
        old_run.join("fuzz.md"),
        "# fuzz finding (seed 99)\n\n## confirmed repro\n\n```\nkey:Down\n```\n",
    )
    .unwrap();

    let mut cmd = Command::new(reproit_bin());
    cmd.env("REPROIT_ROOT", &root)
        .arg("--config")
        .arg(&config)
        .arg("--json")
        .arg("--yes")
        .arg("fuzz")
        .arg("--runs")
        .arg("1")
        .arg("--budget")
        .arg("1")
        .arg("--uniform");
    let (timed_out, stdout, stderr, code) = run_with_timeout(cmd, Duration::from_secs(60));
    assert!(
        !timed_out,
        "clean fuzz output contract timed out\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        matches!(code, Some(0) | Some(1)),
        "clean fuzz output contract failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let output: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output["found"], false);
    assert_eq!(output["confirmedFindings"], 0);
    assert!(output.get("id").is_none());
}

#[test]
fn every_supported_platform_doctor_exits_before_timeout() {
    for platform in PLATFORMS {
        let dir = temp_dir(platform);
        let config = dir.join("reproit.yaml");
        fs::write(&config, config_for(platform)).unwrap();

        let mut cmd = Command::new(reproit_bin());
        cmd.arg("--config").arg(&config).arg("--json").arg("doctor");
        let (timed_out, stdout, stderr, code) = run_with_timeout(cmd, Duration::from_secs(8));
        assert!(
            !timed_out,
            "doctor timed out for platform {platform}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            matches!(code, Some(0) | Some(1)),
            "unexpected exit for platform {platform}: \
             {code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains("\"command\": \"doctor\""),
            "doctor did not emit JSON for platform \
             {platform}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            !stderr.contains("panicked at"),
            "doctor panicked for platform {platform}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}

#[test]
fn tui_map_structural_exits_when_frontier_is_exhausted() {
    let root = repo_root();
    let dir = temp_dir("tui-map-budget");
    let config = dir.join("reproit.yaml");
    let menu = root.join("examples/tui-demo/menu.py");
    let runners = root.join("runners");
    fs::write(
        &config,
        format!(
            "app:\n  platform: tui\n  executable: python3 {}\n  runnerDir: {}\n{}\n",
            menu.display(),
            runners.display(),
            common()
        ),
    )
    .unwrap();

    let mut cmd = Command::new(reproit_bin());
    cmd.arg("--config")
        .arg(&config)
        .arg("debug")
        .arg("map")
        .arg("structural")
        .arg("--yes");
    // 120s, not 30: the walk takes seconds but "drive app-A (building)"
    // compiles the demo app first, and a cold CI runner has flaked past 30s
    // on exactly that step.
    let (timed_out, stdout, stderr, code) = run_with_timeout(cmd, Duration::from_secs(120));
    assert!(
        !timed_out,
        "TUI map timed out\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(
        code,
        Some(0),
        "TUI map failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("map:"),
        "TUI map did not report a map\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
