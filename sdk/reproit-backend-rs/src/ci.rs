//! CI capture mode (feature `instrument`): the flaky-CI wedge.
//!
//! Rust port of the Node SDK's `ci.js`. Wrap a test body in
//! [`run`]`(suite, test, body)` and the trigger identity is the TEST (suite
//! plus test id), not an inbound HTTP request. With `REPROIT_CI_CAPTURE=1`
//! the body runs inside its own trace, so the instrumented outbound
//! boundaries (`instrument::http`, `db::run`, `pg`) record dependency
//! exchanges and the determinism envelope exactly as production capture
//! does; a FAILING test spools a version-2 `reproit-backend-capture` capsule
//! to a bounded on-disk spool. With `REPROIT_REPLAY` set the SAME wrapper
//! re-runs only the capsule's named test while the SDK serves the recorded
//! exchanges in process, and reports the observed result as a structured
//! stderr marker for `reproit check`. Without either env the wrapper runs
//! the body untouched.
//!
//! The wire is the existing capture payload: the test identity rides in the
//! `operation` field as `test:<suite>#<test>`, and the failed assertion is
//! the existing `backend-authored-invariant` registry oracle (a test IS an
//! authored invariant). No new protocol fields, no new oracle ids.
//!
//! Named deviations from the Node reference, forced by cargo test's model:
//!
//! - Adoption is PER TEST BODY, not a runner-level `test()` replacement: a
//!   Rust library cannot intercept `#[test]` functions, so each test wraps
//!   its body in `ci::run` the way an app adopts a logger. An unwrapped test
//!   is invisible to capture, exactly like an unwrapped client.
//! - libtest runs tests in parallel by default; order-dependent suites need
//!   `cargo test -- --test-threads=1` (sequential, name-sorted), which the
//!   SDK cannot impose from library code.
//! - Failure identity is the panic payload (`assert!` messages). Tests that
//!   fail by aborting the process, or `#[should_panic]` inversions, are not
//!   capturable at this layer.
//! - In replay mode non-target tests are skipped silently (their bodies do
//!   not run and libtest reports them as passed); node:test marks them
//!   skipped. The process exit code still speaks for the named test alone.
//!
//! Markers are written straight to the stderr handle so libtest's output
//! capture cannot swallow them (`--nocapture` is not required).
//!
//! Honest limit: replay pins the envelope and the recorded exchanges, which
//! is the whole boundary this SDK can see. A race the boundary cannot see
//! (scheduling, shared memory) is not reproduced by this capsule; `reproit
//! check` reports such runs Inconclusive, never a fake reproduction.

use crate::capture::{determinism_envelope, valid_token, CAPTURE_FORMAT};
use crate::framework::Recorder;
use crate::{instrument, BackendTrace, TraceContext};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Test-trigger identity prefix inside the existing `operation` field.
pub const TEST_TRIGGER_PREFIX: &str = "test:";
/// The registry oracle a failed test capsule carries: an authored invariant
/// (the test's own assertion) was violated. Existing id, not a new one.
pub const TEST_FAILURE_ORACLE: &str = "backend-authored-invariant";
/// Structured stderr markers `reproit check` parses, like REPROIT:DIVERGENCE.
pub const RESULT_MARKER: &str = "REPROIT:CI-TEST ";
pub const SPOOL_MARKER: &str = "REPROIT:CI-CAPSULE ";

/// Spool bounds. The cap covers the TOTAL bytes on disk; capsules beyond it
/// are dropped and counted (in-process stats plus the on-disk
/// `dropped.count`), never silently.
pub const DEFAULT_SPOOL_DIR: &str = ".reproit/ci-spool";
pub const DEFAULT_SPOOL_MAX_BYTES: u64 = 16 * 1024 * 1024;
const SPOOL_MAX_FLOOR_BYTES: u64 = 4 * 1024;
const SPOOL_MAX_CEIL_BYTES: u64 = 64 * 1024 * 1024;
/// Suite and test names share the operation field's 256-code-point bound.
const MAX_NAME: usize = 120;
const MAX_ERROR_CHARS: usize = 2048;

static TRACE_SEQ: AtomicU64 = AtomicU64::new(1);
static SPOOLED: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static FAILED_CAPTURES: AtomicU64 = AtomicU64::new(0);

/// CI capture counters, the Node reference's `ci.stats()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CiStats {
    pub spooled_capsules: u64,
    pub dropped_capsules: u64,
    pub failed_captures: u64,
}

pub fn stats() -> CiStats {
    CiStats {
        spooled_capsules: SPOOLED.load(Ordering::Relaxed),
        dropped_capsules: DROPPED.load(Ordering::Relaxed),
        failed_captures: FAILED_CAPTURES.load(Ordering::Relaxed),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Capture,
    Replay,
}

fn replay_path() -> Option<String> {
    std::env::var("REPROIT_REPLAY")
        .ok()
        .filter(|path| !path.trim().is_empty())
}

fn mode() -> Mode {
    if replay_path().is_some() {
        return Mode::Replay;
    }
    if std::env::var("REPROIT_CI_CAPTURE").ok().as_deref() == Some("1") {
        return Mode::Capture;
    }
    Mode::Off
}

fn bounded_name(value: &str) -> String {
    value.trim().chars().take(MAX_NAME).collect()
}

/// The capsule's operation identity: `test:<suite>#<test>`.
pub fn operation_for(suite: &str, test: &str) -> String {
    format!(
        "{TEST_TRIGGER_PREFIX}{}#{}",
        bounded_name(suite),
        bounded_name(test)
    )
}

fn bounded_error(message: &str) -> String {
    message.chars().take(MAX_ERROR_CHARS).collect()
}

/// The failure identity of a panicked test body: its panic payload.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    "test panicked".to_string()
}

/// Synthesized trace context: the CI job stands where production stood.
fn ci_context() -> TraceContext {
    let build = ["REPROIT_COMMIT", "GITHUB_SHA"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| valid_token(value)));
    TraceContext {
        trace_id: format!(
            "ci-{}-{}",
            crate::capture::now_millis(),
            TRACE_SEQ.fetch_add(1, Ordering::Relaxed)
        ),
        actor: None,
        action_index: 0,
        build,
        config_contract: None,
        capture_envelope: true,
        replay_seed: Some(format!("{:016x}", crate::capture::now_millis() | 1)),
    }
}

/// Straight to fd 2: libtest's output capture reroutes the print macros to a
/// buffer it re-prints on STDOUT, where `reproit check` (watching stderr)
/// could never see a marker.
fn stderr_line(line: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{line}");
}

fn spool_dir() -> PathBuf {
    match std::env::var("REPROIT_CI_SPOOL") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(DEFAULT_SPOOL_DIR),
    }
}

fn spool_max_bytes() -> u64 {
    match std::env::var("REPROIT_CI_SPOOL_MAX")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        Some(parsed) => parsed.clamp(SPOOL_MAX_FLOOR_BYTES, SPOOL_MAX_CEIL_BYTES),
        None => DEFAULT_SPOOL_MAX_BYTES,
    }
}

fn record_drop(dir: &std::path::Path) -> std::io::Result<()> {
    let counter = dir.join("dropped.count");
    // First drop: the counter does not exist yet.
    let dropped = std::fs::read_to_string(&counter)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);
    std::fs::write(&counter, format!("{}\n", dropped + 1))
}

/// Write one capsule inside the byte cap; over-cap capsules are dropped and
/// counted. serde_json's BTreeMap order makes the bytes canonical, matching
/// the Node reference's canonicalJson.
fn spool(payload: &Value, operation: &str) -> std::io::Result<Option<PathBuf>> {
    let body = serde_json::to_vec(payload).map_err(std::io::Error::other)?;
    let dir = spool_dir();
    std::fs::create_dir_all(&dir)?;
    let mut used = 0u64;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        // A concurrently removed entry counts as zero.
        used += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
    }
    if used + body.len() as u64 > spool_max_bytes() {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        record_drop(&dir)?;
        return Ok(None);
    }
    let digest = Sha256::digest(&body);
    let short: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let file = dir.join(format!("capsule-{short}.json"));
    std::fs::write(&file, &body)?;
    SPOOLED.fetch_add(1, Ordering::Relaxed);
    // Field order mirrors the Node reference's insertion order.
    stderr_line(&format!(
        "{SPOOL_MARKER}{{\"file\":{},\"operation\":{}}}",
        json!(file.display().to_string()),
        json!(operation),
    ));
    Ok(Some(file))
}

fn finish_and_spool(recorder: Recorder, operation: &str, message: &str) {
    // Capture must never mask the test's own failure: every fallible step
    // lands in failed_captures instead of propagating.
    let Some(mut trace) = recorder.into_trace() else {
        FAILED_CAPTURES.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if trace
        .finish_test(json!({ "error": message }), false)
        .is_err()
    {
        FAILED_CAPTURES.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // Same envelope shape production capture records; the seed pins the
    // REPLAY run's randomness, it does not reproduce the test run's.
    let observed_at = trace
        .events()
        .first()
        .and_then(|event| event.get("at"))
        .and_then(Value::as_u64);
    let payload = json!({
        "format": CAPTURE_FORMAT,
        "version": 2,
        "operation": operation,
        "oracle": TEST_FAILURE_ORACLE,
        "envelope": determinism_envelope(observed_at),
        "events": trace.events(),
    });
    if spool(&payload, operation).is_err() {
        FAILED_CAPTURES.fetch_add(1, Ordering::Relaxed);
    }
}

/// The one test the loaded capsule names; a capsule without a test trigger
/// identity fails closed (every wrapped test panics with the reason).
fn replay_target() -> &'static str {
    static TARGET: OnceLock<String> = OnceLock::new();
    TARGET.get_or_init(|| {
        let path = replay_path().expect("replay mode without REPROIT_REPLAY");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reproit ci replay refused: read {path}: {error}"));
        let payload: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("reproit ci replay refused: not JSON: {error}"));
        let operation = payload
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            operation.starts_with(TEST_TRIGGER_PREFIX),
            "REPROIT_REPLAY capsule does not carry a test trigger identity"
        );
        operation.to_string()
    })
}

fn report_result(operation: &str, status: &str, failure: Option<&str>) {
    // Hand-assembled so the field order matches the Node reference's
    // insertion order byte for byte.
    let mut line = format!(
        "{RESULT_MARKER}{{\"operation\":{},\"status\":{}",
        json!(operation),
        json!(status),
    );
    if let Some(failure) = failure {
        line.push_str(&format!(",\"failure\":{}", json!(failure)));
    }
    line.push('}');
    stderr_line(&line);
}

/// Run one test body under CI capture/replay, the Node reference's
/// `ci.suite(name)(test, fn)`. Call it as the whole body of a `#[tokio::test]`
/// (or any test running inside a tokio runtime):
///
/// ```ignore
/// #[tokio::test]
/// async fn order_total_applies_the_tax_rate() {
///     reproit_backend::ci::run("checkout", "order total applies the tax rate", async {
///         assert_eq!(order_total(100.0).await, 125.0);
///     })
///     .await;
/// }
/// ```
///
/// Off mode runs the body untouched. Capture mode scopes it over a fresh
/// trace (the instrument boundaries record exchanges) and spools a capsule
/// when it panics, then re-raises the panic so the test still fails. Replay
/// mode runs ONLY the capsule's named test and reports the observed result
/// as the `REPROIT:CI-TEST` stderr marker.
pub async fn run<F>(suite: &str, test: &str, body: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    match mode() {
        Mode::Off => body.await,
        Mode::Capture => capture_run(suite, test, body).await,
        Mode::Replay => replay_run(suite, test, body).await,
    }
}

async fn capture_run<F>(suite: &str, test: &str, body: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let operation = operation_for(suite, test);
    let begun = BackendTrace::begin(
        ci_context(),
        operation.as_str(),
        None,
        None,
        None,
        json!({ "suite": bounded_name(suite), "test": bounded_name(test) }),
        Vec::new(),
    );
    let Ok(trace) = begun else {
        // Capture failed to start; the test itself must still run and speak.
        FAILED_CAPTURES.fetch_add(1, Ordering::Relaxed);
        body.await;
        return;
    };
    let recorder = Recorder::standalone(trace);
    // tokio::spawn is the panic boundary: a panicking body surfaces as a
    // JoinError instead of unwinding through this frame, so the capsule
    // spools BEFORE the failure re-raises.
    let outcome = tokio::spawn(instrument::scope(recorder.clone(), body)).await;
    match outcome {
        Ok(()) => {
            // An over-long passing trace has nothing to spool anyway.
            if let Some(mut trace) = recorder.into_trace() {
                let _ = trace.finish_test(Value::Null, true);
            }
        }
        Err(joined) => {
            let message = bounded_error(&if joined.is_panic() {
                panic_message(joined.into_panic())
            } else {
                "test aborted".to_string()
            });
            finish_and_spool(recorder, &operation, &message);
            std::panic::resume_unwind(Box::new(message));
        }
    }
}

async fn replay_run<F>(suite: &str, test: &str, body: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    // Load the session now so the envelope (TZ, clock offset, seed) is
    // pinned before any test code runs.
    instrument::init();
    let operation = operation_for(suite, test);
    if operation != replay_target() {
        // The capsule names exactly one test; everything else is skipped so
        // the process exit code speaks for the named test alone.
        return;
    }
    let outcome = tokio::spawn(body).await;
    match outcome {
        Ok(()) => report_result(&operation, "passed", None),
        Err(joined) => {
            let message = bounded_error(&if joined.is_panic() {
                panic_message(joined.into_panic())
            } else {
                "test aborted".to_string()
            });
            report_result(&operation, "failed", Some(&message));
            std::panic::resume_unwind(Box::new(message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_identity_is_bounded_and_prefixed() {
        assert_eq!(
            operation_for(" checkout ", "order total"),
            "test:checkout#order total"
        );
        let long = "x".repeat(400);
        let operation = operation_for(&long, &long);
        assert_eq!(
            operation.chars().count(),
            TEST_TRIGGER_PREFIX.len() + MAX_NAME * 2 + 1
        );
    }

    #[test]
    fn result_marker_matches_the_node_field_order() {
        // Byte parity with node's `JSON.stringify({operation, status, failure})`.
        let mut line = format!(
            "{RESULT_MARKER}{{\"operation\":{},\"status\":{}",
            json!("test:s#t"),
            json!("failed"),
        );
        line.push_str(&format!(",\"failure\":{}", json!("7 != 8")));
        line.push('}');
        assert_eq!(
            line,
            "REPROIT:CI-TEST {\"operation\":\"test:s#t\",\"status\":\"failed\",\
             \"failure\":\"7 != 8\"}"
        );
    }

    #[test]
    fn panic_payloads_and_bounds_shape_the_failure_identity() {
        assert_eq!(panic_message(Box::new("boom")), "boom");
        assert_eq!(panic_message(Box::new("boom".to_string())), "boom");
        assert_eq!(panic_message(Box::new(7u8)), "test panicked");
        assert_eq!(
            bounded_error(&"e".repeat(4096)).chars().count(),
            MAX_ERROR_CHARS
        );
    }
}
