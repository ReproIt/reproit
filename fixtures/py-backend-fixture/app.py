"""Money-test fixture for Python capsule parity: a FastAPI app with the
reproit SDK whose /quote operation 500s because an upstream pricing service
returns {"prices": null} and the handler indexes into it. The upstream call
goes through httpx (the transport hook) and the database call through a
psycopg-shaped driver wrapped by `wrap_psycopg` (the same fake-driver idiom
the Node hermetic fixture uses: a driver that MUST never be reached during
hermetic replay).

MODE=capture boots the upstream plus the app, fires the failing request, and
writes a version 2 reproit-backend-capture (exchanges plus envelope) to
CAPTURE_OUT. Default (server) mode boots ONLY the app on $PORT; with
REPROIT_REPLAY set the SDK serves the recorded exchanges in process, so
neither the upstream nor the database exists. FIXED=1 applies the fix.
"""

import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
        "sdk",
        "reproit-backend-py",
    ),
)

from fastapi import FastAPI  # noqa: E402
from fastapi.responses import JSONResponse  # noqa: E402

from reproit_backend_py import (  # noqa: E402
    BackendTrace,  # noqa: F401  (imported for parity with the SDK surface)
    ReproitMiddleware,
    canonical_json,
    determinism_envelope,
    install,
    wrap_psycopg,
)
from reproit_backend_py.capture import CAPTURE_FORMAT  # noqa: E402

install()

import httpx  # noqa: E402  (after install: parity with app boot order)

UPSTREAM_PORT = 19986
CAPTURE_PORT = 19985


class _FakeCursor:
    """psycopg-shaped cursor that MUST never be reached for real: in capture
    mode a canned result stands in for a live database; in replay mode the
    SDK serves the recorded exchange through the connect stub instead."""

    def __init__(self):
        self.description = None
        self.rowcount = -1
        self.statusmessage = None
        self._result = []

    def execute(self, query, params=None):
        if os.environ.get("MODE") != "capture":
            raise AssertionError("live database reached during hermetic replay")
        self._result = [(7, "ACME")]
        self.description = (("id",), ("symbol",))
        self.rowcount = 1
        self.statusmessage = "SELECT 1"
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

    def close(self):
        return None


class _FakeConnection:
    def cursor(self):
        return _FakeCursor()

    def commit(self):
        return None

    def close(self):
        return None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return None


class _FakePsycopg:
    Cursor = _FakeCursor

    def connect(self, *args, **kwargs):
        if os.environ.get("MODE") != "capture":
            raise AssertionError("live database dialed during hermetic replay")
        return _FakeConnection()


psycopg = wrap_psycopg(_FakePsycopg())


def build_app(capture):
    app = FastAPI()
    app.add_middleware(ReproitMiddleware, capture=capture)

    @app.get("/quote")
    def quote(symbol: str = "ACME"):
        try:
            with psycopg.connect("postgresql://db.internal/quotes") as connection:
                cursor = connection.cursor()
                cursor.execute(
                    "SELECT id, symbol FROM issuers WHERE symbol = %s", [symbol]
                )
                issuer = cursor.fetchone()
                if issuer is None:
                    return JSONResponse({"error": "unknown symbol"}, status_code=404)
            url = "http://127.0.0.1:%d/prices?tier=gold" % UPSTREAM_PORT
            body = httpx.get(url).json()
            prices = body.get("prices")
            if os.environ.get("FIXED") == "1" and not isinstance(prices, list):
                return {"first": None, "note": "no prices available"}
            return {"first": prices[0]}
        except Exception:
            return JSONResponse({"error": "internal"}, status_code=500)

    return app


class _Upstream(BaseHTTPRequestHandler):
    def do_GET(self):
        payload = json.dumps({"prices": None}).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):
        return None


class _FileCapture:
    """Capture sink that writes the replayable payload to disk instead of
    uploading it, so the fixture needs no cloud."""

    def context(self):
        return {
            "trace_id": "cap-money-py-fixture-1",
            "actor": None,
            "action_index": 0,
            "build": "py-money-fixture",
            "config_contract": None,
            "capture_envelope": True,
        }

    def record(self, trace):
        events = list(trace.events())
        payload = {
            "format": CAPTURE_FORMAT,
            "version": 2,
            "operation": events[0]["operation"],
            "oracle": "backend-server-error",
            "envelope": determinism_envelope(events[0].get("at")),
            "events": events,
        }
        with open(os.environ["CAPTURE_OUT"], "w", encoding="utf-8") as handle:
            handle.write(canonical_json(payload))


def main():
    import urllib.error
    import urllib.request

    import uvicorn

    if os.environ.get("MODE") == "capture":
        upstream = ThreadingHTTPServer(("127.0.0.1", UPSTREAM_PORT), _Upstream)
        threading.Thread(target=upstream.serve_forever, daemon=True).start()
        app = build_app(_FileCapture())
        config = uvicorn.Config(app, host="127.0.0.1", port=CAPTURE_PORT, log_level="error")
        server = uvicorn.Server(config)
        thread = threading.Thread(target=server.run, daemon=True)
        thread.start()
        while not server.started:
            threading.Event().wait(0.05)
        try:
            url = "http://127.0.0.1:%d/quote?symbol=ACME" % CAPTURE_PORT
            with urllib.request.urlopen(url) as response:
                print("capture fixture status", response.status)
        except urllib.error.HTTPError as error:
            print("capture fixture status", error.code)
        server.should_exit = True
        thread.join(timeout=10)
        upstream.shutdown()
        return

    port = int(os.environ.get("PORT", CAPTURE_PORT))
    uvicorn.run(build_app(None), host="127.0.0.1", port=port, log_level="error")


if __name__ == "__main__":
    main()
