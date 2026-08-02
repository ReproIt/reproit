"""CI capture mode (ci.py): a failing test spools a test-trigger capsule, a
replay run re-executes only the named test and reports the structured result
marker, and the spool cap drops loudly. Each scenario runs the ci decorator
in a child pytest process because capture/replay mode is decided by env at
suite() time and instrument.install() rewires process-wide clients.

Python port of sdk/reproit-backend-node/test/ci.test.js.
"""

import json
import os
import subprocess
import sys

import pytest

from reproit_backend_py import ci

# One upstream call, one assertion that fails unless FIXED=1. The upstream
# stub only boots outside replay, exactly like a real suite's dependencies.
FIXTURE = '''
import json
import os
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from reproit_backend_py import ci

if not os.environ.get("REPROIT_REPLAY"):
    class Upstream(BaseHTTPRequestHandler):
        def do_GET(self):
            body = json.dumps({"n": 7}).encode("utf-8")
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            return None

    server = ThreadingHTTPServer(("127.0.0.1", 19994), Upstream)
    threading.Thread(target=server.serve_forever, daemon=True).start()

test = ci.suite("unit")


@test("asserts the upstream answer")
def test_asserts_the_upstream_answer():
    with urllib.request.urlopen("http://127.0.0.1:19994/n") as response:
        body = json.loads(response.read().decode("utf-8"))
    expected = 7 if os.environ.get("FIXED") == "1" else 8
    assert body["n"] == expected, "%s != %s" % (body["n"], expected)
'''


def run(tmp_path, env):
    """Run the fixture suite in a child pytest with `-s` (the ci module's
    runner note: default output capture would swallow the stderr markers)."""
    test_file = tmp_path / "test_fixture.py"
    test_file.write_text(FIXTURE, encoding="utf-8")
    child_env = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith("REPROIT_") and name != "FIXED"
    }
    child_env.update(env)
    return subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "-s", "-p", "no:cacheprovider", str(test_file)],
        capture_output=True,
        text=True,
        env=child_env,
        timeout=120,
    )


def capsule_files(spool):
    return sorted(path for path in spool.iterdir() if path.name.startswith("capsule-"))


def test_a_failing_test_spools_a_test_trigger_capsule_with_the_exchange(tmp_path):
    spool = tmp_path / "spool"
    result = run(tmp_path, {"REPROIT_CI_CAPTURE": "1", "REPROIT_CI_SPOOL": str(spool)})
    assert result.returncode != 0
    assert ci.SPOOL_MARKER in result.stderr, result.stderr
    files = capsule_files(spool)
    assert len(files) == 1
    capsule = json.loads(files[0].read_text(encoding="utf-8"))
    assert capsule["format"] == "reproit-backend-capture"
    assert capsule["version"] == 2
    assert capsule["operation"] == "test:unit#asserts the upstream answer"
    assert capsule["oracle"] == ci.TEST_FAILURE_ORACLE
    assert isinstance(capsule["envelope"]["replaySeed"], str)
    exchanges = [event for event in capsule["events"] if event.get("exchange")]
    assert len(exchanges) == 1
    assert exchanges[0]["exchange"]["response"]["body"]["n"] == 7
    returned = capsule["events"][-1]
    assert returned["success"] is False
    assert "7 != 8" in str(returned["output"]["error"]), returned["output"]["error"]


def test_replay_reruns_the_named_test_and_reports_failed_then_passed(tmp_path):
    spool = tmp_path / "spool"
    captured = run(tmp_path, {"REPROIT_CI_CAPTURE": "1", "REPROIT_CI_SPOOL": str(spool)})
    assert captured.returncode != 0
    files = capsule_files(spool)
    assert files
    file = str(files[0])
    # No upstream exists in either replay run; the SDK serves the recording.
    failed = run(tmp_path, {"REPROIT_REPLAY": file})
    assert failed.returncode != 0
    assert ci.RESULT_MARKER in failed.stderr, failed.stderr
    failed_line = next(
        line for line in failed.stderr.splitlines() if line.startswith(ci.RESULT_MARKER)
    )
    failed_report = json.loads(failed_line[len(ci.RESULT_MARKER) :])
    assert failed_report["status"] == "failed"
    assert failed_report["operation"] == "test:unit#asserts the upstream answer"
    assert "7 != 8" in str(failed_report["failure"])
    passed = run(tmp_path, {"REPROIT_REPLAY": file, "FIXED": "1"})
    assert passed.returncode == 0, passed.stderr
    assert '"status":"passed"' in passed.stderr, passed.stderr


def test_a_full_spool_drops_the_capsule_and_counts_the_drop(tmp_path):
    spool = tmp_path / "spool"
    spool.mkdir()
    # Pre-fill the spool to the floor cap so the next capsule cannot fit.
    (spool / "existing.json").write_text("x" * (4 * 1024), encoding="utf-8")
    result = run(
        tmp_path,
        {
            "REPROIT_CI_CAPTURE": "1",
            "REPROIT_CI_SPOOL": str(spool),
            "REPROIT_CI_SPOOL_MAX": str(4 * 1024),
        },
    )
    assert result.returncode != 0
    assert capsule_files(spool) == []
    dropped = (spool / "dropped.count").read_text(encoding="utf-8")
    assert int(dropped) == 1


def test_without_capture_or_replay_env_the_decorator_is_inert(tmp_path):
    result = run(tmp_path, {})
    assert result.returncode != 0
    assert ci.SPOOL_MARKER not in result.stderr
    assert ci.RESULT_MARKER not in result.stderr


def test_unknown_suite_options_are_rejected_not_ignored():
    with pytest.raises(TypeError, match="unknown option"):
        ci.suite("s", retries=2)
