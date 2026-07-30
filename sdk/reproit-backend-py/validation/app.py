"""Money-test fixture: a FastAPI app with the reproit SDK whose /quote
operation 500s because an upstream pricing service returns {"prices": null}
and the handler indexes into it.

MODE=capture boots the upstream plus the app, fires the failing request, and
writes a version 2 reproit-backend-capture (exchanges plus envelope) to
CAPTURE_OUT. Default (server) mode boots ONLY the app on $PORT; with
REPROIT_REPLAY set the SDK serves the recorded exchanges, so neither the
upstream nor the database exists. FIXED=1 applies the fix.
"""

import json
import os
import sys
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from fastapi import FastAPI  # noqa: E402
from fastapi.responses import JSONResponse  # noqa: E402

from reproit_backend_py import (  # noqa: E402
    BackendTrace,
    ReproitMiddleware,
    canonical_json,
    db_run,
    determinism_envelope,
    install,
)
from reproit_backend_py.capture import CAPTURE_FORMAT  # noqa: E402

install()

UPSTREAM_PORT = 19981
CAPTURE_PORT = 19980


def _live_query():
    """Stands in for a real driver call. In replay this is never invoked, so
    reaching it would prove the hermetic boundary leaked."""
    if os.environ.get("REPROIT_REPLAY"):
        raise AssertionError("live database reached during hermetic replay")
    return {"command": "SELECT", "rowCount": 1, "rows": [{"id": 7, "symbol": "ACME"}]}


def build_app(capture):
    app = FastAPI()
    app.add_middleware(ReproitMiddleware, capture=capture)

    @app.get("/quote")
    def quote(symbol: str = "ACME"):
        try:
            db_run(
                "SELECT id, symbol FROM issuers WHERE symbol = $1",
                [symbol],
                _live_query,
            )
            url = "http://127.0.0.1:%d/prices?tier=gold" % UPSTREAM_PORT
            with urllib.request.urlopen(url) as response:
                body = json.loads(response.read())
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
            "trace_id": "cap-money-py-1",
            "actor": None,
            "action_index": 0,
            "build": "money-fixture",
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
