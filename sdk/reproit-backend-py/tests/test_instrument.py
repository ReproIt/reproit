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


def test_batch_declares_the_recorded_database_boundary():
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
    assert "database" in capabilities
    assert "network" not in capabilities

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


def test_httpx_sync_exchange_is_captured(upstream):
    """httpx carries its own httpcore transport and never touches
    http.client, so the stdlib hook cannot see it. The transport hook must."""
    httpx = pytest.importorskip("httpx")
    assert instrument_module._STATE["httpx"] is True, "install() should hook httpx"
    trace = _trace()
    token = use_trace(trace)
    try:
        response = httpx.get(upstream + "/prices?tier=gold")
        body = response.json()
    finally:
        clear_trace(token)
    # The application still sees the real bytes after the tee.
    assert body["prices"] == [1, 2]
    assert response.status_code == 200
    exchange = _exchanges(trace)[0]
    assert exchange["protocol"] == "http"
    assert exchange["request"]["method"] == "GET"
    assert exchange["request"]["url"].endswith("/prices?tier=gold")
    assert exchange["response"]["status"] == 200
    assert exchange["response"]["body"]["prices"] == [1, 2]
    # Redaction reaches inside an httpx-captured body exactly as it does the
    # stdlib path.
    assert exchange["response"]["body"]["apiKey"]["$reproit"]["redacted"] is True


def test_httpx_request_bodies_are_captured(upstream):
    httpx = pytest.importorskip("httpx")
    trace = _trace()
    token = use_trace(trace)
    try:
        httpx.post(upstream + "/convert", json={"amount": 5})
    finally:
        clear_trace(token)
    exchange = _exchanges(trace)[0]
    assert exchange["request"]["method"] == "POST"
    assert exchange["request"]["body"] == {"amount": 5}


def test_httpx_async_exchange_is_captured(upstream):
    httpx = pytest.importorskip("httpx")
    import asyncio

    async def fetch():
        async with httpx.AsyncClient() as client:
            return await client.get(upstream + "/prices?tier=gold")

    trace = _trace()
    token = use_trace(trace)
    try:
        response = asyncio.run(fetch())
    finally:
        clear_trace(token)
    assert response.json()["prices"] == [1, 2]
    exchange = _exchanges(trace)[0]
    assert exchange["protocol"] == "http"
    assert exchange["response"]["body"]["prices"] == [1, 2]


def test_httpx_outside_a_trace_records_nothing(upstream):
    httpx = pytest.importorskip("httpx")
    before = instrument_module.stats()["captured_exchanges"]
    httpx.get(upstream + "/prices")
    assert instrument_module.stats()["captured_exchanges"] == before


class _SseUpstream(BaseHTTPRequestHandler):
    """Streams three SSE frames with real flushes and pauses, so the client
    observes more than one chunk on the wire."""

    frames = [
        b'event: message_start\ndata: {"type":"message_start"}\n\n',
        b'data: {"type":"content_block_delta","delta":{"text":"Hello"}}\n\n',
        b'data: {"type":"message_stop"}\n\n',
    ]

    def do_GET(self):
        import time as _time

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        for frame in type(self).frames:
            self.wfile.write(b"%x\r\n" % len(frame) + frame + b"\r\n")
            self.wfile.flush()
            _time.sleep(0.02)
        self.wfile.write(b"0\r\n\r\n")

    def log_message(self, *args):
        return None


@pytest.fixture
def sse_upstream():
    server = ThreadingHTTPServer(("127.0.0.1", 0), _SseUpstream)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield "http://127.0.0.1:%d" % server.server_address[1]
    server.shutdown()
    server.server_close()


def test_httpx_sse_records_one_exchange_with_chunk_boundaries(sse_upstream):
    """The LLM streaming shape: the whole stream is ONE logical exchange, the
    app consumes it live, and the observed chunk boundaries are recorded."""
    httpx = pytest.importorskip("httpx")
    trace = _trace()
    token = use_trace(trace)
    consumed = []
    try:
        with httpx.Client() as client:
            with client.stream("GET", sse_upstream + "/v1/messages") as response:
                for chunk in response.iter_raw():
                    consumed.append(chunk)
    finally:
        clear_trace(token)
    body = b"".join(_SseUpstream.frames)
    assert b"".join(consumed) == body
    exchange = _exchanges(trace)[0]
    assert exchange["response"]["body"] == body.decode("utf-8")
    stream = exchange["response"]["stream"]
    assert stream["chunks"] == [len(chunk) for chunk in consumed]
    assert len(stream["chunks"]) > 1
    assert sum(stream["chunks"]) == len(body)
    assert stream.get("truncated") is not True


def test_httpx_single_chunk_non_sse_records_no_stream_shape(upstream):
    httpx = pytest.importorskip("httpx")
    trace = _trace()
    token = use_trace(trace)
    try:
        httpx.get(upstream + "/plain")
    finally:
        clear_trace(token)
    assert "stream" not in _exchanges(trace)[0]["response"]


def test_aiohttp_exchange_is_captured(upstream):
    """aiohttp never touches http.client either; the ClientSession hook must
    record the exchange while the app still reads the real bytes."""
    aiohttp = pytest.importorskip("aiohttp")
    import asyncio

    async def fetch():
        async with aiohttp.ClientSession() as session:
            async with session.post(
                upstream + "/convert",
                json={"amount": 5},
                headers={"Content-Type": "application/json"},
            ) as response:
                return response.status, await response.json()

    trace = _trace()
    token = use_trace(trace)
    try:
        status, body = asyncio.run(fetch())
    finally:
        clear_trace(token)
    assert status == 200
    assert body["prices"] == [1, 2]
    exchange = _exchanges(trace)[0]
    assert exchange["protocol"] == "http"
    assert exchange["request"]["method"] == "POST"
    assert exchange["request"]["body"] == {"amount": 5}
    assert exchange["response"]["status"] == 200
    assert exchange["response"]["body"]["prices"] == [1, 2]
    # Redaction reaches inside aiohttp-captured bodies too.
    assert exchange["response"]["body"]["apiKey"]["$reproit"]["redacted"] is True


class _FakeCursor:
    """psycopg-shaped cursor: enough surface for the wrap to patch and for
    the canned result to flow through it."""

    rows = [(7, "ACME")]
    fail = False

    def __init__(self):
        self.description = None
        self.rowcount = -1
        self.statusmessage = None
        self._result = []

    def execute(self, query, params=None):
        if type(self).fail:
            error = RuntimeError("relation missing")
            error.sqlstate = "42P01"
            raise error
        self._result = list(type(self).rows)
        self.description = (("id",), ("symbol",))
        self.rowcount = len(self._result)
        self.statusmessage = "SELECT %d" % len(self._result)
        return self

    def fetchone(self):
        return self._result.pop(0) if self._result else None

    def fetchmany(self, size=0):
        taken = self._result[: max(0, size)]
        del self._result[: max(0, size)]
        return taken

    def fetchall(self):
        taken = list(self._result)
        self._result = []
        return taken


class _FakePsycopg:
    """Module-shaped stand-in with the surface wrap_psycopg patches."""

    def __init__(self):
        self.Cursor = _FakeCursor

    def connect(self, *args, **kwargs):
        raise AssertionError("capture tests never open a connection")


def test_wrap_psycopg_records_rows_and_reserves_them_to_the_app():
    from reproit_backend_py import wrap_psycopg

    fake = wrap_psycopg(_FakePsycopg())
    trace = _trace()
    token = use_trace(trace)
    try:
        cursor = fake.Cursor()
        cursor.execute("SELECT id, symbol FROM issuers WHERE symbol = %s", ["ACME"])
        # The app's own fetch still sees exactly the driver's rows even
        # though the wrap drained them to record the exchange.
        assert cursor.fetchone() == (7, "ACME")
        assert cursor.fetchone() is None
    finally:
        clear_trace(token)
    exchange = _exchanges(trace)[0]
    assert exchange["protocol"] == "pg"
    assert exchange["request"]["text"].startswith("SELECT id, symbol")
    assert exchange["request"]["values"] == ["ACME"]
    assert exchange["response"]["command"] == "SELECT"
    assert exchange["response"]["rowCount"] == 1
    assert exchange["response"]["rows"] == [[7, "ACME"]]


def test_wrap_psycopg_records_errors_with_sqlstate():
    from reproit_backend_py import wrap_psycopg

    fake = wrap_psycopg(_FakePsycopg())
    _FakeCursor.fail = True
    trace = _trace()
    token = use_trace(trace)
    try:
        with pytest.raises(RuntimeError):
            fake.Cursor().execute("SELECT boom")
    finally:
        _FakeCursor.fail = False
        clear_trace(token)
    exchange = _exchanges(trace)[0]
    assert exchange["response"]["error"]["message"] == "relation missing"
    assert exchange["response"]["error"]["code"] == "42P01"
