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
