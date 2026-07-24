//! Production capture mode: config-gated self-sampling upload of finished
//! operation traces to the Reproit Cloud ingest endpoint (`/v1/events`).
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
use serde_json::{json, Map, Value};
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

/// Bounds. Queue overflow drops the OLDEST pending operation; an oversized
/// capture payload drops trailing effect events before it drops itself.
const MAX_QUEUE_OPERATIONS: usize = 64;
const MAX_BATCH_OPERATIONS: usize = 16;
const MAX_CAPTURE_JSON_BYTES: usize = 48 * 1024;
const MIN_FLUSH_INTERVAL_MS: u64 = 100;
const MAX_RETRY_LIMIT: u8 = 5;

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Full ingest URL, e.g. `https://cloud.example.com/v1/events`.
    pub endpoint: String,
    /// Project API key, sent as `Authorization: Bearer`.
    pub api_key: String,
    /// Cloud project app id the batches are posted under.
    pub app_id: String,
    /// Optional build/version identity stamped on batches and contexts.
    pub build: Option<String>,
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
        if !error && !self.sample_healthy() {
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
                while state.queue.len() < MAX_BATCH_OPERATIONS && !state.flush_now {
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
                let take = state.queue.len().min(MAX_BATCH_OPERATIONS);
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

    /// Build one event-batch-v1 payload: every captured event ships as a
    /// `backend` frame, and each 5xx operation additionally ships a `finding`
    /// frame tagged `backend-server-error` whose context carries the full
    /// replayable capture object.
    fn build_batch(&self, operations: &[CapturedOperation]) -> Value {
        let batch_id = format!(
            "cap-{}-{}",
            now_millis(),
            self.shared.batch_seq.fetch_add(1, Ordering::Relaxed)
        );
        let mut frames = Vec::new();
        let mut sequence = 0u64;
        let frame = |sequence: &mut u64, event: Value| {
            *sequence += 1;
            json!({
                "runId": batch_id,
                "sequence": *sequence,
                "scope": { "domain": "shared" },
                "event": event,
            })
        };
        for operation in operations {
            for event in &operation.events {
                frames.push(frame(
                    &mut sequence,
                    json!({ "kind": "backend", "evidence": event }),
                ));
            }
            let Some(status) = operation.status.filter(|status| *status >= 500) else {
                continue;
            };
            let signature = format!("backend:{}", operation.operation);
            let message = format!(
                "backend operation {} returned HTTP {status}",
                operation.operation
            );
            let mut context = Map::new();
            context.insert("capture".into(), json!("reproit-backend-rs"));
            if let Some(build) = &self.config.build {
                context.insert("build".into(), json!({ "version": build }));
            }
            match capture_payload(operation) {
                Some((payload, dropped_effects)) => {
                    context.insert("reproitCapture".into(), payload);
                    if dropped_effects > 0 {
                        context.insert("captureDroppedEffects".into(), json!(dropped_effects));
                    }
                }
                None => {
                    context.insert("captureOmitted".into(), json!(true));
                }
            }
            frames.push(frame(
                &mut sequence,
                json!({
                    "kind": "finding",
                    "signature": signature,
                    "message": message,
                    "identity": {
                        "oracle": SERVER_ERROR_ORACLE,
                        "invariant": "backend:server-error",
                        "kind": "server-error",
                        "message": message,
                        "frame": "",
                        "trigger": signature,
                        "boundary": signature,
                    },
                    "path": [],
                    "context": context,
                }),
            ));
        }
        let mut batch = json!({
            "version": 1,
            "batchId": batch_id,
            "appId": self.config.app_id,
            "frames": frames,
            "evidence": [],
        });
        if let Some(build) = &self.config.build {
            batch["deployment"] = json!({ "version": build });
        }
        batch
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
fn capture_payload(operation: &CapturedOperation) -> Option<(Value, usize)> {
    let mut events = operation.events.clone();
    let mut dropped = 0usize;
    loop {
        let payload = json!({
            "format": CAPTURE_FORMAT,
            "version": CAPTURE_VERSION,
            "operation": operation.operation,
            "oracle": SERVER_ERROR_ORACLE,
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

fn lock<'a>(mutex: &'a Mutex<QueueState>) -> MutexGuard<'a, QueueState> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The ingest protocol token charset (`validate_token` in reproit-protocol).
fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectKind, HttpInput, TraceContext};

    fn finished_trace(status: u16, success: bool) -> BackendTrace {
        let context = TraceContext {
            trace_id: "cap-1-1".into(),
            actor: None,
            action_index: 0,
            build: Some("1.2.3".into()),
            config_contract: None,
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

    fn batch_for(status: u16, success: bool) -> Value {
        let capture = Capture {
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
        };
        let trace = finished_trace(status, success);
        let operation = CapturedOperation {
            operation: "createOrder".into(),
            status: Some(status),
            events: trace.events().to_vec(),
        };
        capture.build_batch(&[operation])
    }

    #[test]
    fn server_error_batch_is_a_valid_tagged_event_batch() {
        let batch = batch_for(500, false);
        let parsed: reproit_protocol::EventBatch =
            serde_json::from_value(batch.clone()).expect("batch matches event-batch-v1");
        parsed.validate().expect("batch passes protocol validation");
        let frames = batch["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 4);
        let finding = &frames[3]["event"];
        assert_eq!(finding["kind"], "finding");
        assert_eq!(finding["identity"]["oracle"], SERVER_ERROR_ORACLE);
        let capture = &finding["context"]["reproitCapture"];
        assert_eq!(capture["format"], CAPTURE_FORMAT);
        assert_eq!(capture["operation"], "createOrder");
        assert_eq!(capture["events"].as_array().unwrap().len(), 3);
        // Redaction happened before anything left the process boundary.
        assert_eq!(
            capture["events"][0]["input"]["body"]["item"],
            json!("widget")
        );
    }

    #[test]
    fn healthy_operations_ship_backend_frames_without_a_finding() {
        let batch = batch_for(201, true);
        let frames = batch["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 3);
        assert!(frames
            .iter()
            .all(|frame| frame["event"]["kind"] == "backend"));
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

    #[test]
    fn unusable_configs_disable_capture_instead_of_failing() {
        assert!(Capture::new(CaptureConfig::new("", "sk", "app")).is_none());
        assert!(Capture::new(CaptureConfig::new("http://c", "", "app")).is_none());
        assert!(Capture::new(CaptureConfig::new("http://c", "sk", "bad app id")).is_none());
    }
}
