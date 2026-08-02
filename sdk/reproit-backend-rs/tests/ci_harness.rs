//! CI capture mode end to end, mirroring the Node reference's ci.test.js:
//! a failing test spools a test-trigger capsule with the recorded exchange,
//! replay re-runs only the named test and reports the structured result
//! marker (failed, then passed with the fix), a full spool drops loudly, and
//! without either env the wrapper is inert. Each scenario re-runs THIS test
//! binary as a child process (`--exact` on the child test) because
//! capture/replay mode is decided by env at run() time and the spool is
//! process-global state; REPROIT_CI_CHILD selects the child scenario so the
//! drivers never recurse.
#![cfg(feature = "instrument")]

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};

use reproit_backend::ci;

const CHILD_ENV: &str = "REPROIT_CI_CHILD";
const CHILD_TEST: &str = "child_asserts_the_upstream_answer";

/// One upstream call, one assertion that fails unless FIXED=1. The upstream
/// stub only boots outside replay, exactly like a real suite's dependencies.
#[tokio::test]
async fn child_asserts_the_upstream_answer() {
    if std::env::var(CHILD_ENV).ok().as_deref() != Some("1") {
        return;
    }
    ci::run("unit", "asserts the upstream answer", async {
        let url = upstream_url();
        let client = reqwest::Client::new();
        let request = client.get(format!("{url}/n")).build().expect("request");
        let response = reproit_backend::instrument::http::send(&client, request)
            .await
            .expect("upstream");
        let body: Value = response.json().expect("json");
        let expected = if std::env::var("FIXED").ok().as_deref() == Some("1") {
            7
        } else {
            8
        };
        assert_eq!(body["n"], serde_json::json!(expected));
    })
    .await;
}

/// A one-thread HTTP stub answering `{"n":7}` on an ephemeral port. Never
/// started in replay mode: the SDK serves the recording and matching is on
/// the path alone, so the placeholder origin is never dialed.
fn upstream_url() -> String {
    if std::env::var("REPROIT_REPLAY").is_ok() {
        return "http://127.0.0.1:9".to_string();
    }
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind upstream stub");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {}
                }
            }
            let body = "{\"n\":7}";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn run_child(envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(std::env::current_exe().expect("current_exe"));
    command
        .args(["--exact", CHILD_TEST, "--test-threads=1"])
        .env(CHILD_ENV, "1")
        // A leaked mode env would silently change every scenario.
        .env_remove("REPROIT_REPLAY")
        .env_remove("REPROIT_CI_CAPTURE")
        .env_remove("REPROIT_CI_SPOOL")
        .env_remove("REPROIT_CI_SPOOL_MAX")
        .env_remove("FIXED");
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().expect("spawn child")
}

fn temp_spool(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "reproit-ci-{label}-{}-{}",
        std::process::id(),
        crate::unique()
    ));
    std::fs::create_dir_all(&dir).expect("spool dir");
    dir
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn spooled_capsule(dir: &std::path::Path) -> PathBuf {
    let mut capsules: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read spool")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("capsule-"))
        })
        .collect();
    assert_eq!(capsules.len(), 1, "exactly one capsule expected");
    capsules.remove(0)
}

fn result_line(stderr: &str) -> Value {
    let line = stderr
        .lines()
        .find_map(|line| line.strip_prefix(ci::RESULT_MARKER))
        .expect("result marker");
    serde_json::from_str(line).expect("result marker JSON")
}

#[test]
fn a_failing_test_spools_a_test_trigger_capsule_with_the_exchange() {
    let spool = temp_spool("spool");
    let output = run_child(&[
        ("REPROIT_CI_CAPTURE", "1"),
        ("REPROIT_CI_SPOOL", spool.to_str().unwrap()),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains(ci::SPOOL_MARKER), "{stderr}");
    let capsule: Value =
        serde_json::from_slice(&std::fs::read(spooled_capsule(&spool)).expect("read capsule"))
            .expect("capsule JSON");
    assert_eq!(capsule["format"], "reproit-backend-capture");
    assert_eq!(capsule["version"], 2);
    assert_eq!(
        capsule["operation"],
        "test:unit#asserts the upstream answer"
    );
    assert_eq!(capsule["oracle"], ci::TEST_FAILURE_ORACLE);
    assert!(capsule["envelope"]["replaySeed"].is_string());
    let events = capsule["events"].as_array().expect("events");
    let exchanges: Vec<&Value> = events
        .iter()
        .filter(|event| event.get("exchange").is_some())
        .collect();
    assert_eq!(exchanges.len(), 1);
    assert_eq!(exchanges[0]["exchange"]["response"]["body"]["n"], 7);
    let returned = events.last().expect("return event");
    assert_eq!(returned["kind"], "return");
    assert_eq!(returned["success"], false);
    let error = returned["output"]["error"].as_str().expect("error text");
    assert!(
        error.contains("left") && error.contains('7') && error.contains('8'),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(&spool);
}

#[test]
fn replay_reruns_the_named_test_and_reports_failed_then_passed() {
    let spool = temp_spool("replay");
    let captured = run_child(&[
        ("REPROIT_CI_CAPTURE", "1"),
        ("REPROIT_CI_SPOOL", spool.to_str().unwrap()),
    ]);
    assert!(!captured.status.success());
    let capsule_path = spooled_capsule(&spool);
    let capsule: Value =
        serde_json::from_slice(&std::fs::read(&capsule_path).expect("read")).expect("JSON");
    // No upstream exists in either replay run; the SDK serves the recording.
    let failed = run_child(&[("REPROIT_REPLAY", capsule_path.to_str().unwrap())]);
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr).into_owned();
    let report = result_line(&stderr);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["operation"], "test:unit#asserts the upstream answer");
    // The replayed failure IS the recorded failure, byte for byte: this
    // equality is what lets `reproit check` verdict Reproduced instead of
    // Inconclusive.
    let recorded = capsule["events"]
        .as_array()
        .and_then(|events| events.last())
        .and_then(|event| event["output"]["error"].as_str())
        .expect("recorded failure");
    assert_eq!(report["failure"], recorded);
    let passed = run_child(&[
        ("REPROIT_REPLAY", capsule_path.to_str().unwrap()),
        ("FIXED", "1"),
    ]);
    let stderr = String::from_utf8_lossy(&passed.stderr).into_owned();
    assert!(passed.status.success(), "{stderr}");
    assert!(stderr.contains("\"status\":\"passed\""), "{stderr}");
    let _ = std::fs::remove_dir_all(&spool);
}

#[test]
fn a_full_spool_drops_the_capsule_and_counts_the_drop() {
    let spool = temp_spool("full");
    // Pre-fill the spool to the floor cap so the next capsule cannot fit.
    std::fs::write(spool.join("existing.json"), "x".repeat(4 * 1024)).expect("prefill");
    let output = run_child(&[
        ("REPROIT_CI_CAPTURE", "1"),
        ("REPROIT_CI_SPOOL", spool.to_str().unwrap()),
        ("REPROIT_CI_SPOOL_MAX", "4096"),
    ]);
    assert!(!output.status.success());
    let capsules = std::fs::read_dir(&spool)
        .expect("read spool")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("capsule-"))
        .count();
    assert_eq!(capsules, 0);
    let dropped = std::fs::read_to_string(spool.join("dropped.count")).expect("counter");
    assert_eq!(dropped.trim(), "1");
    let _ = std::fs::remove_dir_all(&spool);
}

#[test]
fn without_capture_or_replay_env_the_wrapper_is_inert() {
    let output = run_child(&[]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!stderr.contains(ci::SPOOL_MARKER), "{stderr}");
    assert!(!stderr.contains(ci::RESULT_MARKER), "{stderr}");
}
