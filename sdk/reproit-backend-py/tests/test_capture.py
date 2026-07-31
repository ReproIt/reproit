"""Capture-mode parity tests against sdk/reproit-backend-rs/src/capture.rs.

Cross-language batch validation against the protocol mirror lives in
sdk/test/backend_batch_test.js; here we pin the shapes and bounds directly.
"""

from reproit_backend_py import BackendTrace, Capture, CAPTURE_FORMAT, SERVER_ERROR_ORACLE
from reproit_backend_py.capture import MAX_CAPTURE_JSON_BYTES, _capture_payload


def _capture(**overrides):
    config = {"endpoint": "http://c/v1/events", "api_key": "sk", "app_id": "app-demo"}
    config.update(overrides)
    return Capture.create(**config)


def _finished_trace(status, success):
    capture = _capture(build="1.2.3")
    trace = BackendTrace.begin(
        capture.context(), "createOrder", input={"body": {"item": "widget", "qty": 2}}
    )
    trace.effect("read", resource="inventory", key="widget")
    trace.finish({"error": "boom"}, status, success, True)
    return trace


def _batch_for(status, success):
    capture = _capture(build="1.2.3")
    trace = _finished_trace(status, success)
    operation = {"operation": "createOrder", "status": status, "events": list(trace.events())}
    return capture._build_batch([operation])


def test_a_ci_runner_supplies_the_commit_the_config_omits(monkeypatch):
    # The env fallback used to be exercised only by accident, when GITHUB_SHA
    # leaked into the suite and broke the exact-shape assertions below. Pin it
    # on purpose instead: a deployment carries the identity its runner knows.
    sha = "f857cb7740a5f857cb7740a5f857cb7740a5f857"
    monkeypatch.setenv("GITHUB_SHA", sha)
    assert _batch_for(500, False)["deployment"] == {"version": "1.2.3", "commit": sha}


def test_server_error_batch_uses_the_universal_causal_contract():
    batch = _batch_for(500, False)
    assert batch["version"] == 1
    assert batch["projectId"] == "app-demo"
    assert batch["deployment"] == {"version": "1.2.3"}
    events = batch["events"]
    assert [item["sequence"] for item in events] == list(range(1, len(events) + 1))
    finding = events[-1]["event"]
    assert finding["kind"] == "observation"
    assert finding["failure"]["signature"] == SERVER_ERROR_ORACLE + ":createOrder"
    # Redaction happened before anything left the process boundary.
    assert events[1]["event"]["value"]["value"]["body"]["item"] == "widget"
    # The determinism envelope rides as a named checkpoint after the trigger.
    envelope = events[2]["event"]
    assert envelope["kind"] == "checkpoint"
    assert envelope["name"] == "determinism-envelope"
    assert isinstance(envelope["attributes"]["replaySeed"], str)
    assert isinstance(envelope["attributes"]["observedAtMs"], int)
    # The raw return event is nested like the raw effects, under a subject
    # that names the carrier for the protocol projection.
    carrier = events[4]["event"]
    assert carrier["kind"] == "effect"
    assert carrier["subject"] == "operation-return"
    raw_return = carrier["value"]["value"]
    assert raw_return["kind"] == "return"
    assert raw_return["status"] == 500


def test_healthy_operations_ship_causal_events_without_an_observation():
    batch = _batch_for(201, True)
    assert [item["event"]["kind"] for item in batch["events"]] == [
        "operation-start", "trigger", "checkpoint", "effect", "effect", "operation-end"
    ]


def test_oversized_captures_drop_trailing_effects_first():
    events = list(_finished_trace(500, False).events())
    filler = "x" * MAX_CAPTURE_JSON_BYTES
    events.insert(2, {"kind": "effect", "effect": "write", "resource": filler})
    payload, dropped = _capture_payload(
        {"operation": "createOrder", "status": 500, "events": events}
    )
    assert dropped == 1
    kept = payload["events"]
    assert len(kept) == 3
    assert kept[1]["kind"] == "effect"
    assert kept[1]["resource"] == "inventory"


def test_legacy_capture_payload_that_cannot_fit_is_still_detected():
    events = [
        {"kind": "start", "operation": "op", "input": {"blob": "x" * MAX_CAPTURE_JSON_BYTES}},
        {"kind": "return", "status": 500, "success": False},
    ]
    payload, _ = _capture_payload({"operation": "op", "status": 500, "events": events})
    assert payload is None


def test_unusable_configs_disable_capture_instead_of_failing():
    assert Capture.create("", "sk", "app") is None
    assert Capture.create("http://c", "", "app") is None
    assert Capture.create("http://c", "sk", "bad app id") is None
    assert Capture.create("http://c", "sk", "app", build="bad build") is None


def test_record_samples_failures_only_by_default():
    capture = _capture()
    open_trace = BackendTrace.begin(capture.context(), "op")
    capture.record(open_trace)
    healthy = BackendTrace.begin(capture.context(), "op")
    healthy.finish(None, 200, True, True)
    capture.record(healthy)
    assert capture.stats()["captured_operations"] == 0
    failed = BackendTrace.begin(capture.context(), "op")
    failed.finish(None, 200, False, True)
    capture.record(failed)
    assert capture.stats()["captured_operations"] == 1
    assert capture.flush(5.0) is True
    stats = capture.stats()
    # http://c is unreachable: the batch fails and its operation is dropped.
    assert stats["failed_batches"] == 1
    assert stats["dropped_operations"] == 1
