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


def test_server_error_batch_is_a_tagged_event_batch():
    batch = _batch_for(500, False)
    assert batch["version"] == 1
    assert batch["deployment"] == {"version": "1.2.3"}
    frames = batch["frames"]
    assert len(frames) == 4
    assert [frame["sequence"] for frame in frames] == [1, 2, 3, 4]
    finding = frames[3]["event"]
    assert finding["kind"] == "finding"
    assert finding["identity"]["oracle"] == SERVER_ERROR_ORACLE
    capture = finding["context"]["reproitCapture"]
    assert capture["format"] == CAPTURE_FORMAT
    assert capture["operation"] == "createOrder"
    assert len(capture["events"]) == 3
    # Redaction happened before anything left the process boundary.
    assert capture["events"][0]["input"]["body"]["item"] == "widget"


def test_healthy_operations_ship_backend_frames_without_a_finding():
    batch = _batch_for(201, True)
    frames = batch["frames"]
    assert len(frames) == 3
    assert all(frame["event"]["kind"] == "backend" for frame in frames)


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


def test_capture_that_cannot_fit_start_plus_return_is_omitted():
    events = [
        {"kind": "start", "operation": "op", "input": {"blob": "x" * MAX_CAPTURE_JSON_BYTES}},
        {"kind": "return", "status": 500, "success": False},
    ]
    payload, _ = _capture_payload({"operation": "op", "status": 500, "events": events})
    assert payload is None
    batch = _capture()._build_batch([{"operation": "op", "status": 500, "events": events}])
    finding = batch["frames"][-1]["event"]
    assert finding["context"]["captureOmitted"] is True
    assert "reproitCapture" not in finding["context"]


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
