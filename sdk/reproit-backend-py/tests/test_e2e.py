"""Functional end-to-end test: a real FastAPI app served by uvicorn with a
planted 500, real HTTP requests via urllib, and a local stub ingest server.
Asserts the capture batch arrives correctly tagged with the causal event
sequence, and that a scan-time request round-trips x-reproit-events.

Run explicitly: uv run --group e2e -m pytest tests/test_e2e.py
"""

import base64
import json
import socket
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

fastapi = pytest.importorskip("fastapi")
uvicorn = pytest.importorskip("uvicorn")

from reproit_backend_py import SERVER_ERROR_ORACLE, Capture, ReproitMiddleware


class _StubIngest(BaseHTTPRequestHandler):
    received = []

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        type(self).received.append(
            {"authorization": self.headers.get("Authorization"), "batch": json.loads(body)}
        )
        payload = b'{"accepted":true}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):
        pass


def _free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def _request(url, method="GET", body=None, headers=None):
    request = urllib.request.Request(url, data=body, headers=headers or {}, method=method)
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return response.status, dict(response.headers), response.read()
    except urllib.error.HTTPError as error:
        return error.code, dict(error.headers), error.read()


def test_fastapi_planted_500_ships_a_tagged_finding_batch():
    _StubIngest.received = []
    ingest = ThreadingHTTPServer(("127.0.0.1", 0), _StubIngest)
    threading.Thread(target=ingest.serve_forever, daemon=True).start()
    ingest_url = "http://127.0.0.1:%d/v1/events" % ingest.server_address[1]

    capture = Capture.create(
        ingest_url, "sk_live_test", "app-e2e", build="9.9.9", flush_interval_ms=100
    )
    assert capture is not None

    app = fastapi.FastAPI()
    app.add_middleware(ReproitMiddleware, capture=capture)

    @app.get("/ok")
    async def ok():
        return {"ok": True}

    @app.post("/boom")
    async def boom(request: fastapi.Request):
        trace = getattr(request.state, "reproit", None)
        assert trace is not None
        trace.effect("write", resource="orders", key="1")
        return fastapi.responses.JSONResponse(status_code=500, content={"error": "boom"})

    port = _free_port()
    config = uvicorn.Config(app, host="127.0.0.1", port=port, log_level="error")
    server = uvicorn.Server(config)
    threading.Thread(target=server.run, daemon=True).start()
    deadline = time.monotonic() + 10
    while not server.started:
        assert time.monotonic() < deadline, "uvicorn did not start"
        time.sleep(0.02)
    base = "http://127.0.0.1:%d" % port

    try:
        status, _, _ = _request(
            base + "/boom",
            method="POST",
            body=json.dumps({"item": "widget", "apiKey": "sk_live_leak"}).encode(),
            headers={"Content-Type": "application/json"},
        )
        assert status == 500
        assert capture.flush(5.0) is True

        assert len(_StubIngest.received) == 1
        assert _StubIngest.received[0]["authorization"] == "Bearer sk_live_test"
        batch = _StubIngest.received[0]["batch"]
        assert batch["version"] == 1
        assert batch["projectId"] == "app-e2e"
        assert batch["deployment"] == {"version": "9.9.9"}
        events = batch["events"]
        assert [item["event"]["kind"] for item in events] == [
            "operation-start",
            "trigger",
            "checkpoint",
            "effect",
            "effect",
            "operation-end",
            "observation",
        ]
        finding = events[6]["event"]
        assert finding["failure"]["signature"] == SERVER_ERROR_ORACLE + ":POST /boom"
        # The determinism envelope rides as a named checkpoint.
        assert events[2]["event"]["name"] == "determinism-envelope"
        assert isinstance(events[2]["event"]["attributes"]["replaySeed"], str)
        assert events[3]["event"]["subject"] == "orders"
        # The raw return event ships as the operation-return effect carrier.
        carrier = events[4]["event"]
        assert carrier["subject"] == "operation-return"
        raw_return = carrier["value"]["value"]
        assert raw_return["kind"] == "return"
        assert raw_return["status"] == 500
        assert raw_return["success"] is False
        # The secret-shaped input field was structurally redacted before upload.
        input_body = events[1]["event"]["value"]["value"]["body"]
        assert input_body["apiKey"]["$reproit"]["redacted"] is True
        assert input_body["item"] == "widget"

        # Scan-time request: header round-trip, no capture of the healthy call.
        status, headers, _ = _request(
            base + "/ok",
            headers={"x-reproit-trace": "trace-e2e", "x-reproit-actor": "alice"},
        )
        assert status == 200
        header = headers.get("x-reproit-events")
        assert header, "expected an x-reproit-events response header"
        events = json.loads(base64.urlsafe_b64decode(header + "=" * (-len(header) % 4)))
        assert events[0]["traceId"] == "trace-e2e"
        assert events[0]["actor"] == "alice"
        assert events[-1]["kind"] == "return"
        assert events[-1]["status"] == 200
        assert capture.stats()["captured_operations"] == 1
    finally:
        server.should_exit = True
        ingest.shutdown()
