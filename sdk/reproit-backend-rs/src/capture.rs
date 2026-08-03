//! Production capture mode: config-gated self-sampling upload of finished
//! operation traces to the Reproit Cloud ingest endpoint
//! (`/v1/capture-batches`).
//!
//! Scan-time tracing stays untouched: this module only adds a place to hand a
//! finished `BackendTrace` when no `x-reproit-trace` header exists. The
//! adapter self-samples: operations that end in a server error (HTTP 5xx) or
//! report `success == false` are always captured; healthy operations are
//! captured only under an optional per-mille baseline sample (default 0).
//!
//! Everything is bounded and capture failure is invisible to the host app:
//! a fixed-depth queue drops oldest on overflow, batches and retries are
//! capped, uploads run on one detached worker thread, and `record` never
//! blocks or panics.

use crate::BackendTrace;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Payload format identifier of the replayable capture object attached to the
/// finding context (`context.reproitCapture`).
pub const CAPTURE_FORMAT: &str = "reproit-backend-capture";
pub const CAPTURE_VERSION: u16 = 1;
/// First-class registry oracle id for an operation that returned HTTP 5xx.
pub const SERVER_ERROR_ORACLE: &str = "backend-server-error";
/// Agent oracle vocabulary (registry ids, lowest confidence tier): authored
/// assertions an LLM/agent operation marks on its own trace via
/// `BackendTrace::oracle(id, detail)`. A marked operation is always captured
/// and its failure observation carries the marked id instead of the 5xx
/// default.
pub const AGENT_RESPONSE_ORACLE: &str = "agent-response-content";
pub const AGENT_GUARDRAIL_ORACLE: &str = "agent-guardrail-violation";
pub const AGENT_LOOP_BOUND_ORACLE: &str = "agent-loop-bound-exceeded";
pub const AGENT_ORACLES: [&str; 3] = [
    AGENT_RESPONSE_ORACLE,
    AGENT_GUARDRAIL_ORACLE,
    AGENT_LOOP_BOUND_ORACLE,
];
/// The effect resource that carries an oracle marker on the trace. A marker
/// is an `emit` effect so the scan-time wire shape stays inside the existing
/// event vocabulary.
pub const ORACLE_MARKER_RESOURCE: &str = "reproit-oracle";

/// First agent oracle marked on a finished trace's events, or `None`.
pub fn marked_oracle(events: &[Value]) -> Option<&'static str> {
    events.iter().find_map(|event| {
        if event.get("kind").and_then(Value::as_str) != Some("effect")
            || event.get("resource").and_then(Value::as_str) != Some(ORACLE_MARKER_RESOURCE)
        {
            return None;
        }
        let key = event.get("key").and_then(Value::as_str)?;
        AGENT_ORACLES.iter().find(|id| **id == key).copied()
    })
}

/// Bounds. Queue overflow drops the OLDEST pending operation; an oversized
/// capture payload drops trailing effect events before it drops itself.
const MAX_QUEUE_OPERATIONS: usize = 64;
#[cfg(test)]
const MAX_CAPTURE_JSON_BYTES: usize = 48 * 1024;
const MIN_FLUSH_INTERVAL_MS: u64 = 100;
const MAX_RETRY_LIMIT: u8 = 5;

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Full ingest URL, e.g. `https://cloud.example.com/v1/capture-batches`.
    pub endpoint: String,
    /// Project API key, sent as `Authorization: Bearer`.
    pub api_key: String,
    /// Cloud project app id the batches are posted under.
    pub app_id: String,
    /// Optional build/version identity stamped on batches and contexts.
    pub build: Option<String>,
    /// Code identity for the capture. When unset, REPROIT_COMMIT then
    /// GITHUB_SHA are consulted; never derived by shelling out to git.
    pub commit: Option<String>,
    /// Per-mille of healthy (successful, non-5xx) operations captured as
    /// baseline evidence. 0 disables healthy sampling entirely.
    pub healthy_sample_per_mille: u16,
    /// Gather window before a pending batch is sent.
    pub flush_interval: Duration,
    /// Per-request upload timeout.
    pub request_timeout: Duration,
    /// Upload retries per batch after the first attempt (5xx/network only).
    pub retry_limit: u8,
}

impl CaptureConfig {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        app_id: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            app_id: app_id.into(),
            build: None,
            commit: None,
            healthy_sample_per_mille: 0,
            flush_interval: Duration::from_millis(3_000),
            request_timeout: Duration::from_millis(5_000),
            retry_limit: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureStats {
    pub captured_operations: u64,
    pub dropped_operations: u64,
    pub sent_batches: u64,
    pub failed_batches: u64,
}

struct CapturedOperation {
    operation: String,
    status: Option<u16>,
    events: Vec<Value>,
}

#[derive(Default)]
struct QueueState {
    queue: VecDeque<CapturedOperation>,
    sending: bool,
    flush_now: bool,
}

struct Shared {
    state: Mutex<QueueState>,
    signal: Condvar,
    captured: AtomicU64,
    dropped: AtomicU64,
    sent: AtomicU64,
    failed: AtomicU64,
    rng: AtomicU64,
    trace_seq: AtomicU64,
    batch_seq: AtomicU64,
}

/// Handle to the capture worker. Cheap to clone; all clones share one queue
/// and one upload thread.
#[derive(Clone)]
pub struct Capture {
    shared: Arc<Shared>,
    config: Arc<CaptureConfig>,
}

impl Capture {
    /// Start capture mode. Returns `None` (capture disabled, host unaffected)
    /// when the config is unusable: empty endpoint/key, an app id that the
    /// ingest protocol would reject, or a worker thread that cannot start.
    pub fn new(mut config: CaptureConfig) -> Option<Self> {
        if config.endpoint.trim().is_empty() || config.api_key.trim().is_empty() {
            return None;
        }
        if !valid_token(&config.app_id) {
            return None;
        }
        if let Some(build) = &config.build {
            if !valid_token(build) {
                return None;
            }
        }
        config.commit = resolve_commit(config.commit.take());
        let minimum = Duration::from_millis(MIN_FLUSH_INTERVAL_MS);
        if config.flush_interval < minimum {
            config.flush_interval = minimum;
        }
        if config.retry_limit > MAX_RETRY_LIMIT {
            config.retry_limit = MAX_RETRY_LIMIT;
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(QueueState::default()),
            signal: Condvar::new(),
            captured: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            rng: AtomicU64::new(now_millis() | 1),
            trace_seq: AtomicU64::new(1),
            batch_seq: AtomicU64::new(1),
        });
        let capture = Self {
            shared,
            config: Arc::new(config),
        };
        let worker = capture.clone();
        std::thread::Builder::new()
            .name("reproit-capture".into())
            .spawn(move || worker.run_worker())
            .ok()?;
        Some(capture)
    }

    /// Synthesized trace context for capture-mode operations, replacing the
    /// scan-time `x-reproit-trace` header requirement.
    pub fn context(&self) -> crate::TraceContext {
        let sequence = self.shared.trace_seq.fetch_add(1, Ordering::Relaxed);
        crate::TraceContext {
            trace_id: format!("cap-{}-{sequence}", now_millis()),
            actor: None,
            action_index: 0,
            build: self.config.build.clone(),
            config_contract: None,
            // Capture-mode traces stamp per-event wall-clock and monotonic
            // offsets (the determinism envelope); scan-time traces never do.
            capture_envelope: true,
        }
    }

    /// Hand a finished trace to the sampler. Unfinished traces are ignored.
    /// Never blocks and never fails visibly; overflow drops the oldest
    /// queued operation.
    pub fn record(&self, trace: &BackendTrace) {
        let events = trace.events();
        let Some(returned) = events
            .iter()
            .rev()
            .find(|event| event.get("kind").and_then(Value::as_str) == Some("return"))
        else {
            return;
        };
        let success = returned
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let status = returned
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok());
        let error = !success || status.is_some_and(|status| status >= 500);
        // A marked agent oracle is an authored failure assertion, so the
        // operation is always captured, like a 5xx.
        let marked = marked_oracle(events).is_some();
        if !error && !marked && !self.sample_healthy() {
            return;
        }
        let Some(operation) = events
            .first()
            .and_then(|event| event.get("operation"))
            .and_then(Value::as_str)
        else {
            return;
        };
        let captured = CapturedOperation {
            operation: operation.to_string(),
            status,
            events: events.to_vec(),
        };
        self.shared.captured.fetch_add(1, Ordering::Relaxed);
        let mut state = lock(&self.shared.state);
        state.queue.push_back(captured);
        if state.queue.len() > MAX_QUEUE_OPERATIONS {
            state.queue.pop_front();
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        self.shared.signal.notify_all();
    }

    /// Block up to `timeout` until every queued operation has been sent (or
    /// dropped). Returns false on timeout. Intended for tests, examples, and
    /// graceful shutdown; request handling never needs it.
    pub fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.shared.state);
        state.flush_now = true;
        self.shared.signal.notify_all();
        while !state.queue.is_empty() || state.sending {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (next, _) = self
                .shared
                .signal
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
        true
    }

    pub fn stats(&self) -> CaptureStats {
        CaptureStats {
            captured_operations: self.shared.captured.load(Ordering::Relaxed),
            dropped_operations: self.shared.dropped.load(Ordering::Relaxed),
            sent_batches: self.shared.sent.load(Ordering::Relaxed),
            failed_batches: self.shared.failed.load(Ordering::Relaxed),
        }
    }

    fn sample_healthy(&self) -> bool {
        let per_mille = self.config.healthy_sample_per_mille;
        if per_mille == 0 {
            return false;
        }
        if per_mille >= 1000 {
            return true;
        }
        // xorshift64 over a shared atomic seed; cheap and dependency-free.
        let mut x = self
            .shared
            .rng
            .fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % 1000) < u64::from(per_mille)
    }

    fn run_worker(&self) {
        let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(self.config.request_timeout)
            .build()
        else {
            return;
        };
        loop {
            let operations = self.next_batch();
            let batch = self.build_batch(&operations);
            if self.send(&client, &batch) {
                self.shared.sent.fetch_add(1, Ordering::Relaxed);
            } else {
                self.shared.failed.fetch_add(1, Ordering::Relaxed);
                self.shared
                    .dropped
                    .fetch_add(operations.len() as u64, Ordering::Relaxed);
            }
            let mut state = lock(&self.shared.state);
            state.sending = false;
            drop(state);
            self.shared.signal.notify_all();
        }
    }

    /// Wait for work, gather up to the batch cap within one flush interval,
    /// then drain. `flush_now` (set by `flush`) cuts the gather window short.
    fn next_batch(&self) -> Vec<CapturedOperation> {
        let mut state = lock(&self.shared.state);
        loop {
            if !state.queue.is_empty() {
                let deadline = Instant::now() + self.config.flush_interval;
                while state.queue.is_empty() && !state.flush_now {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    let (next, wait) = self
                        .shared
                        .signal
                        .wait_timeout(state, deadline - now)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state = next;
                    if wait.timed_out() {
                        break;
                    }
                }
                state.flush_now = false;
                let take = state.queue.len().min(1);
                state.sending = true;
                return state.queue.drain(..take).collect();
            }
            state.flush_now = false;
            state = self
                .shared
                .signal
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Build one source-neutral capture-batch-v1 payload.
    fn build_batch(&self, operations: &[CapturedOperation]) -> Value {
        assert_eq!(
            operations.len(),
            1,
            "a causal capture batch must contain exactly one operation"
        );
        let operation = &operations[0];
        let batch_id = format!(
            "cb-rust-{}-{}",
            now_millis(),
            self.shared.batch_seq.fetch_add(1, Ordering::Relaxed)
        );
        let first = operation
            .events
            .first()
            .cloned()
            .unwrap_or_else(|| json!({}));
        let trace_id = first.get("traceId").and_then(Value::as_str);
        let mut events = Vec::new();
        let mut parent: Option<String> = None;
        let mut push_event = |event: Value, mono: Option<u64>| {
            let sequence = events.len() as u64 + 1;
            let event_id = format!("evt_backend-rust_{sequence}");
            let mut item = json!({
                "id": event_id,
                "sequence": sequence,
                // Real monotonic offsets from the trace's envelope stamps;
                // the ordinal fallback only applies to envelope-less traces.
                "monotonicNs": mono.unwrap_or(sequence),
                "causalParentIds": parent.iter().collect::<Vec<_>>(),
                "event": event,
            });
            if let Some(trace_id) = trace_id {
                item["traceId"] = json!(trace_id);
            }
            parent = Some(event_id);
            events.push(item);
        };
        let mono_of = |event: &Value| event.get("monoNs").and_then(Value::as_u64);
        push_event(
            json!({
                "kind": "operation-start",
                "name": operation.operation,
            }),
            mono_of(&first),
        );
        let input = first.get("input").filter(|value| !value.is_null());
        let captured_input = input.map_or_else(
            || json!({"representation": "structural", "shape": {"type": "unknown"}}),
            |value| {
                json!({
                    "representation": "replayable",
                    "value": value,
                    "redaction": "redacted-at-source",
                })
            },
        );
        push_event(
            json!({
                "kind": "trigger",
                "trigger": "http-request",
                "subject": operation.operation,
                "value": captured_input,
            }),
            mono_of(&first),
        );
        // Determinism envelope: where and when the capture happened, and a
        // seed that makes REPLAY runs deterministic. Honesty note: the seed
        // does not reproduce the app's original randomness; it pins the
        // replay's.
        push_event(
            json!({
                "kind": "checkpoint",
                "name": "determinism-envelope",
                "attributes": self.determinism_envelope(first.get("at").and_then(Value::as_u64)),
            }),
            mono_of(&first),
        );
        for source in &operation.events {
            if source.get("kind").and_then(Value::as_str) != Some("effect") {
                continue;
            }
            let effect = source
                .get("effect")
                .and_then(Value::as_str)
                .unwrap_or("backend-effect");
            let subject = source
                .get("resource")
                .or_else(|| source.get("service"))
                .and_then(Value::as_str)
                .unwrap_or(&operation.operation);
            push_event(
                json!({
                    "kind": "effect",
                    "effect": effect,
                    "subject": subject,
                    "value": {
                        "representation": "replayable",
                        "value": source,
                        "redaction": "redacted-at-source",
                    },
                }),
                mono_of(source),
            );
        }
        // Nest the raw return event exactly like the raw effect events, so
        // the batch can be projected back to a replayable backend capture.
        // The subject names the carrier: `backend_capture_from_batch` in
        // reproit-protocol keys the inversion on "operation-return".
        if let Some(returned) = operation
            .events
            .iter()
            .find(|event| event.get("kind").and_then(Value::as_str) == Some("return"))
        {
            push_event(
                json!({
                    "kind": "effect",
                    "effect": "operation-return",
                    "subject": "operation-return",
                    "value": {
                        "representation": "replayable",
                        "value": returned,
                        "redaction": "redacted-at-source",
                    },
                }),
                mono_of(returned),
            );
        }
        let succeeded = operation.events.iter().rev().find_map(|event| {
            (event.get("kind").and_then(Value::as_str) == Some("return"))
                .then(|| event.get("success").and_then(Value::as_bool))
                .flatten()
        }) == Some(true);
        let last_mono = operation.events.iter().rev().find_map(mono_of);
        push_event(
            json!({
                "kind": "operation-end",
                "name": operation.operation,
                "outcome": if succeeded { "succeeded" } else { "failed" },
            }),
            last_mono,
        );
        let marked = marked_oracle(&operation.events);
        let server_error = operation.status.filter(|status| *status >= 500);
        if marked.is_some() || server_error.is_some() {
            let oracle = marked.unwrap_or(SERVER_ERROR_ORACLE);
            let signature = format!("{oracle}:{}", operation.operation);
            // A marked agent oracle is an authored assertion (a declared
            // contract the trace itself violated); a bare 5xx stays the
            // runtime exception it always was.
            let (observation, message) = match marked {
                Some(id) => (
                    "contract-violation",
                    format!("agent oracle {id} fired on {}", operation.operation),
                ),
                None => (
                    "exception",
                    format!(
                        "backend operation {} returned HTTP {}",
                        operation.operation,
                        server_error.unwrap_or(0)
                    ),
                ),
            };
            push_event(
                json!({
                    "kind": "observation",
                    "failure": {
                        "observation": observation,
                        "authority": "runtime-diagnosis",
                        "summary": message,
                        "signature": signature,
                        "observationPoint": operation.operation,
                        "artifactIds": [],
                    },
                }),
                last_mono,
            );
        }
        let mut batch = json!({
            "version": 1,
            "batchId": batch_id,
            "projectId": self.config.app_id,
            "sessionId": trace_id.unwrap_or(&batch_id),
            "emitter": {
                "id": "backend-rust",
                "kind": "runtime-sdk",
                "component": "backend",
                "runtime": "rust",
            },
            "observedAt": now_millis().to_string(),
            "policy": {
                "consent": "application-telemetry",
                "retentionClass": "standard",
            },
            "capabilities": capabilities(operation),
            "events": events,
            "artifacts": [],
        });
        let mut deployment = serde_json::Map::new();
        if let Some(build) = &self.config.build {
            deployment.insert("version".into(), json!(build));
        }
        if let Some(commit) = &self.config.commit {
            deployment.insert("commit".into(), json!(commit));
        }
        if !deployment.is_empty() {
            batch["deployment"] = Value::Object(deployment);
        }
        batch
    }

    /// Envelope attributes for one capture batch; see [`determinism_envelope`].
    fn determinism_envelope(&self, observed_at: Option<u64>) -> Value {
        let mut seed = self
            .shared
            .rng
            .fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        envelope_with_seed(observed_at, seed)
    }

    fn send(&self, client: &reqwest::blocking::Client, batch: &Value) -> bool {
        for attempt in 0..=self.config.retry_limit {
            let response = client
                .post(&self.config.endpoint)
                .header("authorization", format!("Bearer {}", self.config.api_key))
                .json(batch)
                .send();
            match response {
                Ok(response) if response.status().is_success() => return true,
                // A definitive client-side rejection cannot improve on retry.
                Ok(response) if response.status().is_client_error() => return false,
                _ => {}
            }
            if attempt < self.config.retry_limit {
                std::thread::sleep(Duration::from_millis(200 * u64::from(attempt) + 200));
            }
        }
        false
    }
}

/// The replayable capture object (`reproit debug replay-capture` input).
/// Trailing effect events are dropped first when the payload exceeds the
/// context budget; a payload that stays oversized with only start/return
/// left is omitted entirely (`None`).
#[cfg(test)]
fn capture_payload(operation: &CapturedOperation) -> Option<(Value, usize)> {
    let mut events = operation.events.clone();
    let oracle = marked_oracle(&operation.events).unwrap_or(SERVER_ERROR_ORACLE);
    let mut dropped = 0usize;
    loop {
        let payload = json!({
            "format": CAPTURE_FORMAT,
            "version": CAPTURE_VERSION,
            "operation": operation.operation,
            "oracle": oracle,
            "events": events,
        });
        let size = serde_json::to_vec(&payload).map(|bytes| bytes.len()).ok()?;
        if size <= MAX_CAPTURE_JSON_BYTES {
            return Some((payload, dropped));
        }
        let last_effect = events
            .iter()
            .rposition(|event| event.get("kind").and_then(Value::as_str) == Some("effect"))?;
        events.remove(last_effect);
        dropped += 1;
    }
}

/// The determinism envelope: where and when the capture happened, timezone
/// (from TZ when set; Rust has no cheap IANA zone lookup, a named gap),
/// runtime identity, and a seed that makes REPLAY runs deterministic.
/// Honesty note: the seed does not reproduce the randomness the app drew in
/// production; it pins the replay's. Public so file-writing capture sinks
/// (fixtures, tests) stamp the same envelope the upload path does.
pub fn determinism_envelope(observed_at: Option<u64>) -> Value {
    let mut seed = now_millis()
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(std::process::id() as u64)
        | 1;
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    envelope_with_seed(observed_at, seed)
}

fn envelope_with_seed(observed_at: Option<u64>, seed: u64) -> Value {
    let mut attributes = serde_json::Map::from_iter([
        (
            "observedAtMs".into(),
            json!(observed_at.unwrap_or_else(now_millis)),
        ),
        ("runtime".into(), json!("rust")),
        ("os".into(), json!(std::env::consts::OS)),
        ("arch".into(), json!(std::env::consts::ARCH)),
        ("replaySeed".into(), json!(format!("{seed:016x}"))),
    ]);
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.trim().is_empty() {
            attributes.insert("tz".into(), json!(tz));
        }
    }
    if let Ok(digest) = std::env::var("REPROIT_IMAGE_DIGEST") {
        if valid_token(&digest) {
            attributes.insert("imageDigest".into(), json!(digest));
        }
    }
    Value::Object(attributes)
}

fn lock<'a>(mutex: &'a Mutex<QueueState>) -> MutexGuard<'a, QueueState> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Batch capabilities: the network claim is complete ONLY when the trace
/// actually recorded outbound exchanges, so the capsule completeness model
/// never over-claims for apps without the instrument layer.
fn capabilities(operation: &CapturedOperation) -> Value {
    let has_exchanges = operation
        .events
        .iter()
        .any(|event| event.get("exchange").is_some_and(|value| !value.is_null()));
    let mut list = vec![
        json!({"capability": "http", "completeness": "complete"}),
        json!({
            "capability": "database",
            "completeness": "partial",
            "detail": "effect records do not prove complete database state capture",
        }),
    ];
    if has_exchanges {
        list.push(json!({
            "capability": "network",
            "completeness": "complete",
            "detail": "outbound dependency exchanges recorded with responses",
        }));
    }
    Value::Array(list)
}

/// Code identity in priority order: explicit config, then the common CI and
/// platform environment. Never shells out to git.
fn resolve_commit(configured: Option<String>) -> Option<String> {
    resolve_commit_from(configured, |name| std::env::var(name).ok())
}

/// The same resolution with the environment supplied explicitly, so a test can
/// STATE it rather than inherit it. A GitHub runner always sets GITHUB_SHA and
/// a laptop never does, so a suite asserting an exact deployment shape passes
/// locally and fails in CI. The Python, Java and Ruby SDKs each hit that
/// separately; this seam is why the Rust one cannot.
fn resolve_commit_from(
    configured: Option<String>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(commit) = configured {
        if valid_token(&commit) {
            return Some(commit);
        }
    }
    for name in ["REPROIT_COMMIT", "GITHUB_SHA"] {
        if let Some(value) = lookup(name) {
            if valid_token(&value) {
                return Some(value);
            }
        }
    }
    None
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The ingest protocol token charset (`validate_token` in reproit-protocol).
pub(crate) fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

#[cfg(test)]
mod tests {

    // The environment is an input this suite STATES, never one it inherits.
    // Proven both ways so the fallback is exercised on purpose rather than by
    // the accident of a runner setting GITHUB_SHA.
    #[test]
    fn a_ci_runner_supplies_the_commit_the_config_omits() {
        let sha = "f857cb7740a5f857cb7740a5f857cb7740a5f857";
        let found = super::resolve_commit_from(None, |name| {
            (name == "GITHUB_SHA").then(|| sha.to_string())
        });
        assert_eq!(found.as_deref(), Some(sha));
    }

    #[test]
    fn an_empty_environment_yields_no_commit() {
        assert_eq!(super::resolve_commit_from(None, |_| None), None);
    }

    #[test]
    fn a_configured_commit_wins_over_the_environment() {
        let configured = "0123456789abcdef0123456789abcdef01234567";
        let found = super::resolve_commit_from(Some(configured.to_string()), |_| {
            Some("f857cb7740a5f857cb7740a5f857cb7740a5f857".to_string())
        });
        assert_eq!(found.as_deref(), Some(configured));
    }
    use super::*;
    use crate::{EffectKind, HttpInput, TraceContext};

    fn finished_trace(status: u16, success: bool) -> BackendTrace {
        let context = TraceContext {
            trace_id: "cap-1-1".into(),
            actor: None,
            action_index: 0,
            build: Some("1.2.3".into()),
            config_contract: None,
            capture_envelope: true,
        };
        let mut trace = BackendTrace::begin(
            context,
            "createOrder",
            None,
            None,
            None,
            HttpInput {
                body: Some(json!({"item": "widget", "qty": 2})),
                ..HttpInput::default()
            }
            .into_value(),
            Vec::new(),
        )
        .unwrap();
        trace
            .effect(
                EffectKind::Read,
                Some("inventory"),
                Some("widget"),
                None,
                None,
                None,
            )
            .unwrap();
        trace
            .finish(json!({"error": "boom"}), status, success, true)
            .unwrap();
        trace
    }

    /// A capture handle without the worker thread: `record` and `build_batch`
    /// are synchronous over the shared queue, which is all these tests need.
    fn test_capture() -> Capture {
        Capture {
            shared: Arc::new(Shared {
                state: Mutex::new(QueueState::default()),
                signal: Condvar::new(),
                captured: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
                sent: AtomicU64::new(0),
                failed: AtomicU64::new(0),
                rng: AtomicU64::new(1),
                trace_seq: AtomicU64::new(1),
                batch_seq: AtomicU64::new(1),
            }),
            config: Arc::new({
                let mut config = CaptureConfig::new("http://c/v1/events", "sk", "app-demo");
                config.build = Some("1.2.3".into());
                config
            }),
        }
    }

    fn batch_for(status: u16, success: bool) -> Value {
        let capture = test_capture();
        let trace = finished_trace(status, success);
        let operation = CapturedOperation {
            operation: "createOrder".into(),
            status: Some(status),
            events: trace.events().to_vec(),
        };
        capture.build_batch(&[operation])
    }

    #[test]
    fn server_error_batch_uses_the_universal_causal_contract() {
        let batch = batch_for(500, false);
        let parsed: reproit_protocol::CaptureBatch =
            serde_json::from_value(batch.clone()).expect("batch matches capture-batch-v1");
        parsed.validate().expect("batch passes protocol validation");
        let events = batch["events"].as_array().unwrap();
        assert_eq!(events.len(), 7);
        // The determinism envelope rides as a named checkpoint after the
        // trigger.
        let envelope = &events[2]["event"];
        assert_eq!(envelope["kind"], "checkpoint");
        assert_eq!(envelope["name"], "determinism-envelope");
        assert!(envelope["attributes"]["replaySeed"].is_string());
        let finding = &events[6]["event"];
        assert_eq!(finding["kind"], "observation");
        assert_eq!(
            finding["failure"]["signature"],
            format!("{SERVER_ERROR_ORACLE}:createOrder")
        );
        // Redaction happened before anything left the process boundary.
        assert_eq!(
            events[1]["event"]["value"]["value"]["body"]["item"],
            json!("widget")
        );
        // The raw return event is nested like the raw effects, under a
        // subject that names it, and round-trips through the protocol
        // projection as the replayable capture's final return event.
        let carrier = &events[4]["event"];
        assert_eq!(carrier["kind"], "effect");
        assert_eq!(carrier["subject"], "operation-return");
        let raw_return = &carrier["value"]["value"];
        assert_eq!(raw_return["kind"], "return");
        assert_eq!(raw_return["status"], 500);
        let capture = reproit_protocol::backend_capture_from_batch(&parsed)
            .expect("server-error batch projects to a replayable capture");
        assert_eq!(capture["operation"], "createOrder");
        assert_eq!(capture["oracle"], SERVER_ERROR_ORACLE);
        assert_eq!(
            capture["events"].as_array().unwrap().last().unwrap(),
            raw_return
        );
    }

    #[test]
    fn healthy_operations_ship_causal_events_without_an_observation() {
        let batch = batch_for(201, true);
        let events = batch["events"].as_array().unwrap();
        assert_eq!(events.len(), 6);
        assert!(events
            .iter()
            .all(|event| event["event"]["kind"] != "observation"));
    }

    #[test]
    fn oversized_captures_drop_trailing_effects_first() {
        let mut events = finished_trace(500, false).events().to_vec();
        let filler = "x".repeat(MAX_CAPTURE_JSON_BYTES);
        events.insert(
            2,
            json!({"kind": "effect", "effect": "write", "resource": filler}),
        );
        let operation = CapturedOperation {
            operation: "createOrder".into(),
            status: Some(500),
            events,
        };
        let (payload, dropped) = capture_payload(&operation).unwrap();
        assert_eq!(dropped, 1);
        let kept = payload["events"].as_array().unwrap();
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[1]["kind"], "effect");
        assert_eq!(kept[1]["resource"], "inventory");
    }

    fn assist_trace(capture: &Capture, oracle: &str, detail: Value) -> BackendTrace {
        let mut trace = BackendTrace::begin(
            capture.context(),
            "POST /assist",
            None,
            None,
            None,
            Value::Null,
            Vec::new(),
        )
        .unwrap();
        trace.oracle(oracle, Some(detail)).unwrap();
        trace
    }

    // Mirrors the Node reference's agent-oracle tests in test/capture.test.js.
    #[test]
    fn agent_oracle_markers_ride_the_trace_and_reject_unknown_ids() {
        let capture = test_capture();
        let mut trace = assist_trace(
            &capture,
            AGENT_GUARDRAIL_ORACLE,
            json!({"tool": "delete_order"}),
        );
        assert_eq!(
            trace.oracle("made-up-oracle", None),
            Err(crate::TraceError::InvalidOperation)
        );
        trace
            .finish(json!({"error": "guardrail"}), 500, false, true)
            .unwrap();
        assert_eq!(marked_oracle(trace.events()), Some(AGENT_GUARDRAIL_ORACLE));
    }

    #[test]
    fn a_marked_agent_operation_is_captured_even_without_a_5xx() {
        let capture = test_capture();
        let mut trace = assist_trace(
            &capture,
            AGENT_LOOP_BOUND_ORACLE,
            json!({"iterations": 9, "bound": 4}),
        );
        trace
            .finish(json!({"note": "gave up"}), 200, true, true)
            .unwrap();
        capture.record(&trace);
        assert_eq!(capture.stats().captured_operations, 1);
    }

    #[test]
    fn a_marked_failure_observation_carries_the_agent_oracle_id() {
        let capture = test_capture();
        let mut trace = assist_trace(
            &capture,
            AGENT_GUARDRAIL_ORACLE,
            json!({"tool": "delete_order"}),
        );
        trace
            .finish(json!({"error": "guardrail"}), 500, false, true)
            .unwrap();
        let operation = CapturedOperation {
            operation: "POST /assist".into(),
            status: Some(500),
            events: trace.events().to_vec(),
        };
        let batch = capture.build_batch(&[operation]);
        let parsed: reproit_protocol::CaptureBatch =
            serde_json::from_value(batch.clone()).expect("batch matches capture-batch-v1");
        parsed.validate().expect("batch passes protocol validation");
        let observation = &batch["events"].as_array().unwrap().last().unwrap()["event"];
        assert_eq!(observation["kind"], "observation");
        assert_eq!(
            observation["failure"]["signature"],
            format!("{AGENT_GUARDRAIL_ORACLE}:POST /assist")
        );
        assert_eq!(observation["failure"]["observation"], "contract-violation");
        // A marked healthy operation (no 5xx at all) still carries the
        // authored observation: the mark IS the failure assertion.
        let mut healthy = assist_trace(
            &capture,
            AGENT_LOOP_BOUND_ORACLE,
            json!({"iterations": 9, "bound": 4}),
        );
        healthy
            .finish(json!({"note": "gave up"}), 200, true, true)
            .unwrap();
        let healthy_batch = capture.build_batch(&[CapturedOperation {
            operation: "POST /assist".into(),
            status: Some(200),
            events: healthy.events().to_vec(),
        }]);
        let observation = &healthy_batch["events"].as_array().unwrap().last().unwrap()["event"];
        assert_eq!(observation["kind"], "observation");
        assert_eq!(observation["failure"]["observation"], "contract-violation");
        assert_eq!(
            observation["failure"]["signature"],
            format!("{AGENT_LOOP_BOUND_ORACLE}:POST /assist")
        );
    }

    #[test]
    fn unusable_configs_disable_capture_instead_of_failing() {
        assert!(Capture::new(CaptureConfig::new("", "sk", "app")).is_none());
        assert!(Capture::new(CaptureConfig::new("http://c", "", "app")).is_none());
        assert!(Capture::new(CaptureConfig::new("http://c", "sk", "bad app id")).is_none());
    }
}
