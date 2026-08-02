//! Hermetic re-execution of a CI-captured test failure (the flaky-CI wedge).
//!
//! A test-trigger capture is the same `reproit-backend-capture` payload the
//! production SDK writes, but its trigger is a TEST, not an inbound HTTP
//! request: the `operation` field carries `test:<suite>#<test>` (stamped by
//! the SDK's ci module) and the oracle is the existing
//! `backend-authored-invariant` id. `reproit check <capsule> --exec "<test
//! command>"` therefore boots NOTHING: it re-runs the test command with
//! `REPROIT_REPLAY` pointed at the capsule, the SDK re-executes only the
//! named test with every recorded exchange served in process and the
//! envelope pinned, and the verdict comes from the observed result marker:
//!
//! - `reproduced`: the named test fails with the recorded failure again.
//! - `fixed`: the named test passes UNDER the recorded envelope and
//!   exchanges. A plain rerun passing outside the capsule proves nothing
//!   (that is the flaky trap this wedge exists for) and never reaches this
//!   verdict, because this run's conditions are the recorded ones.
//! - `diverged`: the test made a call the capture never saw.
//! - `inconclusive`: the named test did not run, the runner died, the run
//!   timed out, or the test failed in a DIFFERENT way than recorded. A
//!   different failure is not this failure; races the replay boundary
//!   cannot see land here, never in `reproduced`.

use crate::domain::backend::BackendEventKind;
use crate::interface::cli::context::Ctx;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::BufRead;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::hermetic::HermeticVerdict;

/// The SDK's structured markers (replay.js and ci.js).
const DIVERGENCE_MARKER: &str = "REPROIT:DIVERGENCE ";
const RESULT_MARKER: &str = "REPROIT:CI-TEST ";
/// Test-trigger identity prefix inside the existing `operation` field.
pub const TEST_TRIGGER_PREFIX: &str = "test:";
/// Whole-run budget (runner startup plus the one named test) before the run
/// is declared inconclusive and the child is killed.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Routing sniff: does this capture file carry a test trigger identity?
/// Parse errors read as "no"; shape validation happens once routed.
pub fn capture_is_test_trigger(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| {
            value
                .get("operation")
                .and_then(Value::as_str)
                .map(|operation| operation.starts_with(TEST_TRIGGER_PREFIX))
        })
        .unwrap_or(false)
}

/// The `REPROIT:CI-TEST` result the replayed runner reported for the named
/// test.
struct TestResult {
    operation: String,
    status: String,
    failure: Option<String>,
}

#[derive(Default)]
struct Observation {
    divergences: Vec<Value>,
    result: Option<TestResult>,
}

fn watch_stderr(child: &mut std::process::Child) -> Arc<Mutex<Observation>> {
    let sink = Arc::new(Mutex::new(Observation::default()));
    if let Some(stderr) = child.stderr.take() {
        let observation = Arc::clone(&sink);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                if let Some(report) = line.strip_prefix(DIVERGENCE_MARKER) {
                    let parsed =
                        serde_json::from_str(report).unwrap_or_else(|_| json!({"raw": report}));
                    if let Ok(mut guard) = observation.lock() {
                        guard.divergences.push(parsed);
                    }
                } else if let Some(report) = line.strip_prefix(RESULT_MARKER) {
                    let parsed: Value = serde_json::from_str(report).unwrap_or(Value::Null);
                    let read =
                        |name: &str| parsed.get(name).and_then(Value::as_str).map(str::to_string);
                    if let (Some(operation), Some(status)) = (read("operation"), read("status")) {
                        if let Ok(mut guard) = observation.lock() {
                            // First result wins: the SDK replays one test.
                            if guard.result.is_none() {
                                guard.result = Some(TestResult {
                                    operation,
                                    status,
                                    failure: read("failure"),
                                });
                            }
                        }
                    }
                } else {
                    // The runner's own failure output is the developer's
                    // context; pass it through like the process capsule does.
                    eprintln!("{line}");
                }
            }
        });
    }
    sink
}

/// The recorded failure identity: the bounded assertion message the SDK put
/// in the return event's output when it spooled the capsule.
fn recorded_failure(artifact: &super::capture_replay::CaptureArtifact) -> Option<String> {
    artifact
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.event {
            BackendEventKind::Return { output, .. } => output
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        })
}

/// Pure verdict mapping, split out so the contract is unit-testable.
fn classify(
    timed_out: bool,
    divergences_empty: bool,
    result: Option<&TestResult>,
    operation: &str,
    recorded_failure: Option<&str>,
) -> HermeticVerdict {
    if timed_out {
        return HermeticVerdict::Inconclusive;
    }
    if !divergences_empty {
        return HermeticVerdict::Diverged;
    }
    let Some(result) = result.filter(|result| result.operation == operation) else {
        return HermeticVerdict::Inconclusive;
    };
    match result.status.as_str() {
        "passed" => HermeticVerdict::Fixed,
        "failed" => {
            // A capsule that recorded a declared failure demands the same one
            // back; a different failure is not this failure and fails closed.
            let matches = match (recorded_failure, result.failure.as_deref()) {
                (Some(recorded), Some(seen)) => recorded == seen,
                (Some(_), None) => false,
                (None, _) => true,
            };
            if matches {
                HermeticVerdict::Reproduced
            } else {
                HermeticVerdict::Inconclusive
            }
        }
        _ => HermeticVerdict::Inconclusive,
    }
}

/// `check <capsule.json> --exec "<test command>"` on a test-trigger capture:
/// re-run the named test hermetically and report the four-way verdict.
pub async fn check_capture_test(ctx: &Ctx, file: &Path, command: &str) -> Result<ExitCode> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let artifact = super::capture_replay::parse_capture(&bytes)?;
    if !artifact.operation.starts_with(TEST_TRIGGER_PREFIX) {
        bail!(
            "capture operation {:?} is not a test trigger; use the hermetic request path",
            artifact.operation
        );
    }
    let recorded = recorded_failure(&artifact);
    let capture_path = std::fs::canonicalize(file)?;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("REPROIT_REPLAY", &capture_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn --exec command {command:?}"))?;
    let observation = watch_stderr(&mut child);

    let deadline = std::time::Instant::now() + RUN_TIMEOUT;
    let mut timed_out = false;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let (divergences, result) = {
        let guard = observation
            .lock()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))?;
        (
            guard.divergences.clone(),
            guard.result.as_ref().map(|result| TestResult {
                operation: result.operation.clone(),
                status: result.status.clone(),
                failure: result.failure.clone(),
            }),
        )
    };
    let verdict = classify(
        timed_out,
        divergences.is_empty(),
        result.as_ref(),
        &artifact.operation,
        recorded.as_deref(),
    );

    ctx.emit(&json!({
        "command": "check",
        "capture": {
            "file": file.display().to_string(),
            "operation": artifact.operation,
            "oracle": artifact.oracle,
            "mode": "test-hermetic",
            "verdict": verdict.as_str(),
            "divergences": divergences,
            "recordedFailure": recorded,
            "observedResult": result.as_ref().map(|r| json!({
                "operation": r.operation,
                "status": r.status,
                "failure": r.failure,
            })),
            "timedOut": timed_out,
            "envelope": artifact.envelope,
        },
        "outcome": match verdict {
            HermeticVerdict::Fixed => "pass",
            HermeticVerdict::Reproduced => "fail",
            _ => "stale",
        },
        "exit": verdict.exit_code(),
    }));
    ctx.say(format!("check capture {} (test hermetic)", file.display()));
    match verdict {
        HermeticVerdict::Reproduced => ctx.say(format!(
            "  FAIL reproduced by re-execution ({} on {})",
            artifact.oracle, artifact.operation
        )),
        HermeticVerdict::Fixed => ctx.say(
            "  PASS the test now passes under the recorded envelope and exchanges \
             (a plain rerun passing outside the capsule would prove nothing)",
        ),
        HermeticVerdict::Diverged => {
            ctx.say("  DIVERGED the test no longer makes the captured calls:");
            for report in &divergences {
                ctx.say(format!("    {report}"));
            }
        }
        HermeticVerdict::Inconclusive => {
            let reason = if timed_out {
                "the test run exceeded its budget".to_string()
            } else {
                match &result {
                    Some(result) if result.status == "failed" => format!(
                        "the test failed differently than recorded (recorded {:?}, observed {:?}); \
                         a different failure is not this failure",
                        recorded, result.failure
                    ),
                    _ => "the named test did not run under replay".to_string(),
                }
            };
            ctx.say(format!("  INCONCLUSIVE {reason}; failing closed"));
        }
    }
    Ok(ExitCode::from(verdict.exit_code()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(operation: &str, status: &str, failure: Option<&str>) -> TestResult {
        TestResult {
            operation: operation.to_string(),
            status: status.to_string(),
            failure: failure.map(str::to_string),
        }
    }

    const OP: &str = "test:checkout#order total";

    /// The whole verdict contract in one table: same failure reproduces, a
    /// pass under the capsule certifies, divergence outranks the result, and
    /// everything unobservable fails closed.
    #[test]
    fn verdicts_map_the_observed_result() {
        let same = result(OP, "failed", Some("1010 !== 110"));
        assert_eq!(
            classify(false, true, Some(&same), OP, Some("1010 !== 110")),
            HermeticVerdict::Reproduced
        );
        let passed = result(OP, "passed", None);
        assert_eq!(
            classify(false, true, Some(&passed), OP, Some("1010 !== 110")),
            HermeticVerdict::Fixed
        );
        assert_eq!(
            classify(false, false, Some(&same), OP, Some("1010 !== 110")),
            HermeticVerdict::Diverged
        );
    }

    /// A different failure is not this failure, a foreign test's result does
    /// not speak for the named one, and silence or a timeout can never be a
    /// verdict: all Inconclusive, never Reproduced and never Fixed.
    #[test]
    fn unprovable_runs_fail_closed() {
        let different = result(OP, "failed", Some("boom"));
        assert_eq!(
            classify(false, true, Some(&different), OP, Some("1010 !== 110")),
            HermeticVerdict::Inconclusive
        );
        let foreign = result("test:other#name", "failed", Some("1010 !== 110"));
        assert_eq!(
            classify(false, true, Some(&foreign), OP, Some("1010 !== 110")),
            HermeticVerdict::Inconclusive
        );
        assert_eq!(
            classify(false, true, None, OP, None),
            HermeticVerdict::Inconclusive
        );
        assert_eq!(
            classify(true, true, None, OP, None),
            HermeticVerdict::Inconclusive
        );
    }

    /// A capsule recorded before failure messages existed still reproduces on
    /// a failed named test; the comparison only tightens when evidence exists.
    #[test]
    fn recorded_failure_absence_does_not_block_reproduction() {
        let failed = result(OP, "failed", Some("anything"));
        assert_eq!(
            classify(false, true, Some(&failed), OP, None),
            HermeticVerdict::Reproduced
        );
    }

    /// The routing sniff keys on the operation prefix alone; other captures
    /// and non-files are "not a test trigger", never an error.
    #[test]
    fn sniff_keys_on_the_operation_prefix() {
        let dir = std::env::temp_dir().join(format!("reproit-test-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let test = dir.join("test.json");
        std::fs::write(
            &test,
            r#"{"format":"reproit-backend-capture","operation":"test:suite#name"}"#,
        )
        .unwrap();
        assert!(capture_is_test_trigger(&test));
        let http = dir.join("http.json");
        std::fs::write(
            &http,
            r#"{"format":"reproit-backend-capture","operation":"GET /quote"}"#,
        )
        .unwrap();
        assert!(!capture_is_test_trigger(&http));
        assert!(!capture_is_test_trigger(&dir.join("missing.json")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
