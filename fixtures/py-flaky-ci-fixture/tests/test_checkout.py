"""Planted order-dependent test failure that fires only under CI-like
conditions, for the flaky-CI wedge (Track 3), Python edition.

The first test runs ONLY on the CI legacy matrix (CI_LEGACY_MATRIX=1) and
leaks state into the shared config service: it switches the service to its
legacy response format, which returns the tax rate as a percent string. The
second test then computes a wrong total and fails. A plain local run never
takes the legacy branch, so the suite passes and the failure looks
unreproducible ("flaky"). The capsule spooled by the CI run carries the
recorded legacy response, so `reproit check <capsule> --exec "uv run
--project sdk/reproit-backend-py --group test python -m pytest -q -s
tests/test_checkout.py"` re-executes the exact failing run anywhere.

Run pytest with `-s`: the stderr markers `reproit check` parses must not be
swallowed by pytest's output capture (the pytest analogue of the Node
fixture's direct-invocation note).
"""

import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

FIXTURE_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(FIXTURE_DIR))
sys.path.insert(0, str(FIXTURE_DIR.parent.parent / "sdk" / "reproit-backend-py"))

from reproit_backend_py import ci  # noqa: E402

from order import order_total  # noqa: E402

PORT = 19995
CONFIG_URL = "http://127.0.0.1:%d" % PORT

# The shared config service both tests talk to. Stateful on purpose: the
# legacy-format test leaks its toggle into it. Never started under replay,
# where the SDK serves the recorded exchanges in process and any real socket
# attempt would surface as a divergence, not a connection.
_STATE = {"legacy": False}

if not os.environ.get("REPROIT_REPLAY"):

    class _ConfigService(BaseHTTPRequestHandler):
        def do_POST(self):
            if self.path == "/format/legacy":
                _STATE["legacy"] = True
                self.send_response(204)
                self.end_headers()
                return
            self.send_response(404)
            self.end_headers()

        def do_GET(self):
            answer = (
                {"rate": "25", "unit": "percent"} if _STATE["legacy"] else {"rate": 0.25}
            )
            body = json.dumps(answer).encode("utf-8")
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            return None

    _server = ThreadingHTTPServer(("127.0.0.1", PORT), _ConfigService)
    threading.Thread(target=_server.serve_forever, daemon=True).start()

test = ci.suite("checkout")


@test("legacy config format toggles")
def test_legacy_config_format_toggles():
    # CI-only: this is the state leak that makes the next test order
    # dependent. A local run never takes this branch.
    if os.environ.get("CI_LEGACY_MATRIX") != "1":
        return
    import urllib.request

    request = urllib.request.Request(CONFIG_URL + "/format/legacy", method="POST")
    with urllib.request.urlopen(request) as response:
        assert response.status == 204


@test("order total applies the configured tax rate")
def test_order_total_applies_the_configured_tax_rate():
    total = order_total(100, CONFIG_URL)
    assert total == 125, "order total %s != 125" % total
