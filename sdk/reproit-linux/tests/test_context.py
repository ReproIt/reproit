"""Context contract tests for the native Linux SDK."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from reproit_linux.reporter import Reporter


def test_configured_build_identity_is_added_to_context():
    reporter = Reporter(
        "example",
        build_version="1.4.2",
        build_commit="abc123",
    )
    assert reporter.context()["build"] == {
        "version": "1.4.2",
        "commit": "abc123",
    }


def test_tester_capture_has_exact_structural_identity():
    events = []
    reporter = Reporter("example", on_event=events.append)
    reporter._cur = "deadbeef"
    reporter._path = [{"sig": "deadbeef", "action": "load"}]

    assert reporter.capture_bug() is True
    event = events[-1]
    assert event["oracle"] == "tester-capture"
    assert event["findingIdentity"]["boundary"] == "deadbeef"
    assert event["findingIdentity"]["invariant"] == "tester-observed-failure"
