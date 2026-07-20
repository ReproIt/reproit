"""Strict event protocol tests for the Linux SDK."""

from reproit_linux.reporter import _protocol_batch


def test_protocol_batch_maps_edges_and_findings():
    batch = _protocol_batch(
        "app",
        [
            {"kind": "edge", "from": "a", "action": "tap", "to": "b"},
            {
                "kind": "error",
                "sig": "b",
                "message": "boom",
                "path": [{"sig": "b", "action": "tap"}],
            },
        ],
        {"platform": "linux"},
        42,
        3,
    )
    assert batch["version"] == 1
    assert batch["batchId"] == "sdk-42-3"
    assert batch["frames"][0]["event"]["kind"] == "graph-edge"
    finding = batch["frames"][1]["event"]
    assert finding["kind"] == "finding"
    assert finding["context"]["platform"] == "linux"
    assert batch["evidence"] == []


def test_protocol_batch_marks_unknown_capture_records_as_defects():
    batch = _protocol_batch("app", [{"kind": "mystery"}], {}, 42, 1)
    assert batch["frames"][0]["event"] == {
        "kind": "stream-defect",
        "reason": "invalid-event",
    }
