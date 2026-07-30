"""Outbound-exchange capture tests: the instrumented http.client hook and the
explicit database boundary must attach request AND response to the ambient
trace, bounded and redacted, and the batch must declare the network
capability only when exchanges were actually recorded.

These exercise the capture side. Replay lives in test_replay.py, which needs a
fresh interpreter because both install into the same process-wide hook.
"""

import hashlib
import json
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from reproit_backend_py import BackendTrace, Capture, db_run, install
from reproit_backend_py import instrument as instrument_module
from reproit_backend_py.trace import clear_trace, use_trace

install()


class _Upstream(BaseHTTPRequestHandler):
    payload = b'{"prices": [1, 2], "apiKey": "sk-live-secret"}'
    content_type = "application/json"

    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", type(self).content_type)
        self.send_header("Content-Length", str(len(type(self).payload)))
        self.end_headers()
        self.wfile.write(type(self).payload)

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.do_GET()

    def log_message(self, *args):
        return None


@pytest.fixture
def upstream():
    server = ThreadingHTTPServer(("127.0.0.1", 0), _Upstream)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield "http://127.0.0.1:%d" % server.server_address[1]
    server.shutdown()
    server.server_close()


def _trace():
    context = {
        "trace_id": "cap-x-1",
        "actor": None,
        "action_index": 0,
        "build": None,
        "config_contract": None,
        "capture_envelope": True,
    }
    return BackendTrace.begin(context, "GET /quote", input={"query": {"symbol": "ACME"}})


def _exchanges(trace):
    return [event["exchange"] for event in trace.events() if event.get("exchange")]


def test_http_exchange_records_request_and_response(upstream):
    trace = _trace()
    token = use_trace(trace)
    try:
        with urllib.request.urlopen(upstream + "/prices?tier=gold") as response:
            body = json.loads(response.read())
    finally:
        clear_trace(token)
    # The application still sees the real bytes.
    assert body["prices"] == [1, 2]
    exchange = _exchanges(trace)[0]
    assert exchange["protocol"] == "http"
    assert exchange["request"]["method"] == "GET"
    assert exchange["request"]["url"].endswith("/prices?tier=gold")
    assert exchange["response"]["status"] == 200
    assert exchange["response"]["body"]["prices"] == [1, 2]
    # Structural redaction applies INSIDE captured exchange bodies.
    assert exchange["response"]["body"]["apiKey"]["$reproit"]["redacted"] is True


def test_request_bodies_are_captured(upstream):
    trace = _trace()
    token = use_trace(trace)
    try:
        request = urllib.request.Request(
            upstream + "/convert",
            data=json.dumps({"amount": 5}).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(request) as response:
            response.read()
    finally:
        clear_trace(token)
    exchange = _exchanges(trace)[0]
    assert exchange["request"]["method"] == "POST"
    assert exchange["request"]["body"] == {"amount": 5}


def test_oversized_bodies_keep_provable_identity_only(upstream):
    big = b"x" * (instrument_module.MAX_EXCHANGE_BODY_BYTES + 1)
    _Upstream.payload = big
    _Upstream.content_type = "text/plain"
    try:
        trace = _trace()
        token = use_trace(trace)
        try:
            with urllib.request.urlopen(upstream + "/blob") as response:
                assert len(response.read()) == len(big)
        finally:
            clear_trace(token)
    finally:
        _Upstream.payload = b'{"prices": [1, 2], "apiKey": "sk-live-secret"}'
        _Upstream.content_type = "application/json"
    response_side = _exchanges(trace)[0]["response"]
    assert response_side["truncated"] is True
    assert response_side["bodyBytes"] == len(big)
    assert response_side["bodySha256"] == hashlib.sha256(big).hexdigest()
    assert "body" not in response_side


def test_untraced_calls_are_not_recorded(upstream):
    trace = _trace()
    # No ambient trace installed: the hook must stay out of the way.
    with urllib.request.urlopen(upstream + "/prices") as response:
        response.read()
    assert _exchanges(trace) == []


def test_db_run_records_rows_and_errors():
    trace = _trace()
    token = use_trace(trace)
    try:
        result = db_run(
            "SELECT id, name FROM issuers WHERE symbol = $1",
            ["ACME"],
            lambda: {"command": "SELECT", "rowCount": 1, "rows": [{"id": 7, "name": "ACME"}]},
        )
        assert result["rows"] == [{"id": 7, "name": "ACME"}]
        with pytest.raises(RuntimeError):
            db_run("SELECT boom", None, _raise)
    finally:
        clear_trace(token)
    exchanges = _exchanges(trace)
    assert exchanges[0]["protocol"] == "db"
    assert exchanges[0]["request"]["values"] == ["ACME"]
    assert exchanges[0]["response"]["rows"] == [{"id": 7, "name": "ACME"}]
    assert exchanges[1]["response"]["error"]["message"] == "relation missing"


def _raise():
    raise RuntimeError("relation missing")


def test_batch_declares_network_only_when_exchanges_exist():
    capture = Capture.create("http://c/v1/capture-batches", "sk", "app-demo")
    trace = _trace()
    token = use_trace(trace)
    try:
        db_run("SELECT 1", None, lambda: {"command": "SELECT", "rowCount": 0, "rows": []})
    finally:
        clear_trace(token)
    trace.finish({"error": "boom"}, 500, False, True)
    batch = capture._build_batch(
        [{"operation": "GET /quote", "status": 500, "events": list(trace.events())}]
    )
    capabilities = {item["capability"] for item in batch["capabilities"]}
    assert "network" in capabilities

    bare = _trace()
    bare.finish({"error": "boom"}, 500, False, True)
    bare_batch = capture._build_batch(
        [{"operation": "GET /quote", "status": 500, "events": list(bare.events())}]
    )
    assert "network" not in {item["capability"] for item in bare_batch["capabilities"]}


def test_capture_mode_stamps_the_envelope_and_scan_mode_does_not():
    captured = _trace()
    assert isinstance(captured.events()[0]["at"], int)
    assert isinstance(captured.events()[0]["monoNs"], int)
    scan = BackendTrace.begin(
        {
            "trace_id": "trace-a",
            "actor": None,
            "action_index": 0,
            "build": None,
            "config_contract": None,
        },
        "createOrder",
        input={"item": "widget"},
    )
    assert "at" not in scan.events()[0]
    assert "monoNs" not in scan.events()[0]


def test_commit_identity_resolves_from_config_then_environment():
    assert Capture.resolve_commit("abc123", {}) == "abc123"
    assert Capture.resolve_commit(None, {"REPROIT_COMMIT": "def456"}) == "def456"
    assert Capture.resolve_commit(None, {"GITHUB_SHA": "ghi789"}) == "ghi789"
    assert Capture.resolve_commit(None, {}) is None
    assert Capture.resolve_commit(None, {"REPROIT_COMMIT": "bad commit"}) is None
