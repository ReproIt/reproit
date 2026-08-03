//! `reproit verify` is the proof-of-fix command, so its four answers have to be
//! distinguishable through the real binary: a live bug blocks, a genuine server
//! fix is held, a schema claim the project has withdrawn is retracted (passing,
//! but never counted as proof), and naming an id must narrow the work rather
//! than replay the whole suite at the target.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// A fixture service for one test: counts the requests it answers so a test can
/// assert the target was never touched, and can start returning the field the
/// contract asks for (a real implementation change).
struct Service {
    port: u16,
    requests: Arc<AtomicUsize>,
    complete: Arc<AtomicBool>,
}

impl Service {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture service");
        let port = listener.local_addr().expect("fixture port").port();
        let requests = Arc::new(AtomicUsize::new(0));
        let complete = Arc::new(AtomicBool::new(false));
        let service = Self {
            port,
            requests: Arc::clone(&requests),
            complete: Arc::clone(&complete),
        };
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                requests.fetch_add(1, Ordering::SeqCst);
                let _ = answer(stream, complete.load(Ordering::SeqCst));
            }
        });
        service
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// Ship the field the contract requires: the genuine fix.
    fn start_returning_server_time(&self) {
        self.complete.store(true, Ordering::SeqCst);
    }
}

fn answer(mut stream: TcpStream, complete: bool) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
    }
    let nearby = request_line.contains(" /nearby");
    let body = match (nearby, complete) {
        (false, _) => "{}".to_string(),
        (true, false) => r#"{"ok":true}"#.to_string(),
        (true, true) => r#"{"ok":true,"serverTime":"2026-07-25T00:00:00Z"}"#.to_string(),
    };
    let status = if nearby { "200 OK" } else { "404 Not Found" };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: \
         close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

const SCHEMA: &str = r#"{
  "openapi": "3.0.3",
  "info": {"title": "fixture", "version": "1"},
  "servers": [{"url": "%URL%"}],
  "paths": {"%PATH%": {"get": {"operationId": "%OPERATION%",
    "responses": {"200": {"content": {"application/json": {"schema": {
      "type": "object",
      "additionalProperties": false,
      "required": %REQUIRED%,
      "properties": %PROPERTIES%
    }}}}}}}}
}"#;

fn write_schema(root: &Path, service: &Service, path: &str, operation: &str, claim: bool) {
    let (required, properties) = if claim {
        (
            r#"["ok","serverTime"]"#,
            r#"{"ok":{"type":"boolean"},"serverTime":{"type":"string"}}"#,
        )
    } else {
        (r#"["ok"]"#, r#"{"ok":{"type":"boolean"}}"#)
    };
    let document = SCHEMA
        .replace("%URL%", &service.url())
        .replace("%PATH%", path)
        .replace("%OPERATION%", operation)
        .replace("%REQUIRED%", required)
        .replace("%PROPERTIES%", properties);
    std::fs::write(root.join("api.json"), document).expect("write schema");
}

/// Write the project. `claims_server_time` is the contract under test: the
/// schema asserting a field the service does not return is the false claim whose
/// correct fix is to withdraw it, not to change the product.
fn write_project(root: &Path, service: &Service, claims_server_time: bool) {
    write_schema(root, service, "/nearby", "getNearby", claims_server_time);
    std::fs::write(
        root.join("reproit.yaml"),
        format!(
            "backend:\n  enabled: true\n  schemas: [api.json]\n  target: {}\n",
            service.url()
        ),
    )
    .expect("write config");
}

fn workspace(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("reproit-verify-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create workspace");
    root
}

fn reproit(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reproit"))
        .args(args)
        .current_dir(root)
        .output()
        .expect("run reproit")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn findings(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join(".reproit/findings")) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

/// Scan with the claim in place and assert it produced exactly one finding.
fn scan_one_finding(root: &Path) {
    let scan = reproit(root, &["scan", "--yes"]);
    assert!(
        scan.status.success() || scan.status.code() == Some(1),
        "scan failed: {}{}",
        stdout(&scan),
        String::from_utf8_lossy(&scan.stderr)
    );
    assert_eq!(findings(root).len(), 1, "expected one recorded finding");
}

#[test]
fn naming_an_id_does_not_replay_the_rest_of_the_suite() {
    let service = Service::start();
    let root = workspace("filter");
    write_project(&root, &service, true);
    scan_one_finding(&root);

    // Everything the scan did is already counted; verifying an id this project
    // has never recorded must add nothing, because the filter has to run before
    // the replay rather than on its results.
    let before = service.requests();
    let verify = reproit(&root, &["verify", "fnd_deadbeef0000"]);
    assert_eq!(
        service.requests(),
        before,
        "verifying an unrelated id sent requests at the live target"
    );
    assert!(verify.status.success(), "{}", stdout(&verify));
    assert!(stdout(&verify).contains("no matching findings"));

    // And the counter has teeth: naming the id this project DID record replays
    // it, so the assertion above is about the filter and not about a fixture
    // that never counts anything.
    let recorded = findings(&root).pop().expect("one finding");
    reproit(&root, &["verify", &format!("fnd_{recorded}")]);
    assert!(
        service.requests() > before,
        "verifying the recorded id should have replayed it"
    );
}

#[test]
fn a_live_bug_blocks_and_a_real_server_fix_is_held() {
    let service = Service::start();
    let root = workspace("held");
    write_project(&root, &service, true);
    scan_one_finding(&root);

    let blocked = reproit(&root, &["verify"]);
    assert_eq!(blocked.status.code(), Some(1), "{}", stdout(&blocked));
    assert!(stdout(&blocked).contains("1 still reproducing"));

    // The implementation changes: the response now carries the promised field.
    service.start_returning_server_time();
    let held = reproit(&root, &["verify"]);
    assert!(held.status.success(), "{}", stdout(&held));
    assert!(
        stdout(&held).contains("1 held"),
        "a genuine fix must be held, got: {}",
        stdout(&held)
    );
}

#[test]
fn withdrawing_a_false_claim_retracts_instead_of_blocking_forever() {
    let service = Service::start();
    let root = workspace("retract");
    write_project(&root, &service, true);
    scan_one_finding(&root);
    let recorded = findings(&root);

    assert_eq!(reproit(&root, &["verify"]).status.code(), Some(1));

    // The finding was true, but the contract was wrong: the API never promised
    // serverTime. The correct fix is to withdraw the claim, and the schema is
    // the authority for what the project asserts, so `scan` now reports nothing.
    write_project(&root, &service, false);
    let clean = reproit(&root, &["scan", "--yes"]);
    assert!(
        stdout(&clean).contains("0 confirmed finding"),
        "scan should be clean after the claim is withdrawn: {}",
        stdout(&clean)
    );

    // Verify has to agree. The recorded request still produces the recorded
    // violation under the recorded contract, so replaying it can never go green;
    // without a retracted state the only way to close it is deleting the
    // artifact by hand.
    let retracted = reproit(&root, &["verify"]);
    assert!(retracted.status.success(), "{}", stdout(&retracted));
    let report = stdout(&retracted);
    assert!(
        report.contains("0 held") && report.contains("1 retracted"),
        "a withdrawn claim is retracted, never held: {report}"
    );
    assert_eq!(
        findings(&root),
        recorded,
        "verify must not delete by itself"
    );

    let pruned = reproit(&root, &["verify", "--prune-retracted"]);
    assert!(pruned.status.success(), "{}", stdout(&pruned));
    assert!(
        findings(&root).is_empty(),
        "--prune-retracted must remove the disowned constraint"
    );
}

#[test]
fn an_operation_dropped_from_the_schema_retracts_without_calling_the_target() {
    let service = Service::start();
    let root = workspace("absent");
    write_project(&root, &service, true);
    scan_one_finding(&root);

    // The whole operation leaves the schema. Nothing about the recorded finding
    // can be re-checked, so it must not be replayed at all.
    write_schema(&root, &service, "/health", "getHealth", false);

    let before = service.requests();
    let retracted = reproit(&root, &["verify"]);
    assert!(retracted.status.success(), "{}", stdout(&retracted));
    assert!(
        stdout(&retracted).contains("1 retracted"),
        "{}",
        stdout(&retracted)
    );
    assert_eq!(
        service.requests(),
        before,
        "a dropped operation must retract without sending its request"
    );
}
