// The environment is an input this suite STATES, never one it inherits.
// Proven both ways so the fallback is exercised on purpose rather than by
// the accident of a runner setting GITHUB_SHA.
#[test]
fn a_ci_runner_supplies_the_commit_the_config_omits() {
    let sha = "f857cb7740a5f857cb7740a5f857cb7740a5f857";
    let found =
        super::resolve_commit_from(None, |name| (name == "GITHUB_SHA").then(|| sha.to_string()));
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
        replay_seed: Some("00ff00ff00ff00ff".into()),
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
        .exchange(
            EffectKind::Read,
            Some("inventory"),
            Some("widget"),
            json!({
                "request": {"key": "widget"},
                "response": {"available": true},
            }),
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
            let mut config = CaptureConfig::new("http://c/v1/capture-batches", "sk", "app-demo");
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
    let compilation = reproit_protocol::compile_capture_failure(
        &parsed,
        &parsed.observed_at,
        reproit_protocol::CaptureAssessmentScope::Portable,
    )
    .expect("batch compiles")
    .expect("batch has a failure");
    assert_eq!(
        compilation.assessment.status,
        reproit_protocol::AssessmentStatus::Eligible
    );
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
