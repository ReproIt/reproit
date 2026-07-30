"""Hermetic replay tests: with REPROIT_REPLAY set, the wrapped clients serve
recorded exchanges in process (no sockets, no database), divergence fails
closed with the structured marker, and the envelope pins the clock zone and
RNG.

`install()` patches process-wide state, so this module runs the assertions in
a child interpreter with REPROIT_REPLAY set rather than fighting the capture
hooks the sibling test module installs.
"""

import json
import os
import subprocess
import sys
import tempfile

CAPTURE = {
    "format": "reproit-backend-capture",
    "version": 2,
    "operation": "GET /quote",
    "oracle": "backend-server-error",
    "envelope": {
        "observedAtMs": 1753747200000,
        "tz": "UTC",
        "runtime": "python 3.13.0",
        "replaySeed": "00ff00ff00ff00ff",
    },
    "events": [
        {
            "traceId": "cap-r-1",
            "spanId": "cap-r-1:GET /quote",
            "actionIndex": 0,
            "operation": "GET /quote",
            "sequence": 1,
            "kind": "start",
            "input": {"query": {"symbol": "ACME"}},
            "at": 1753747200000,
            "monoNs": 0,
        },
        {
            "traceId": "cap-r-1",
            "spanId": "cap-r-1:GET /quote",
            "actionIndex": 0,
            "operation": "GET /quote",
            "sequence": 2,
            "kind": "effect",
            "effect": "read",
            "resource": "db",
            "key": "SELECT id FROM issuers WHERE symbol = $1",
            "exchange": {
                "protocol": "db",
                "request": {
                    "text": "SELECT id FROM issuers WHERE symbol = $1",
                    "values": ["ACME"],
                },
                "response": {"command": "SELECT", "rowCount": 1, "rows": [{"id": 7}]},
            },
            "at": 1753747200004,
            "monoNs": 4000000,
        },
        {
            "traceId": "cap-r-1",
            "spanId": "cap-r-1:GET /quote",
            "actionIndex": 0,
            "operation": "GET /quote",
            "sequence": 3,
            "kind": "effect",
            "effect": "call",
            "resource": "pricing.internal",
            "key": "GET /prices",
            "exchange": {
                "protocol": "http",
                "request": {
                    "method": "GET",
                    "url": "http://pricing.internal/prices?tier=gold",
                },
                "response": {
                    "status": 200,
                    "headers": {"content-type": "application/json"},
                    "body": {"prices": None},
                },
            },
            "at": 1753747200009,
            "monoNs": 9000000,
        },
        {
            "traceId": "cap-r-1",
            "spanId": "cap-r-1:GET /quote",
            "actionIndex": 0,
            "operation": "GET /quote",
            "sequence": 4,
            "kind": "return",
            "output": {"error": "internal"},
            "status": 500,
            "success": False,
            "effectsComplete": True,
            "at": 1753747200012,
            "monoNs": 12000000,
        },
    ],
}

# The child asserts inside the replaying interpreter and prints one JSON line.
CHILD = r"""
import json, os, random, sys, urllib.error, urllib.request
from reproit_backend_py import db_run, install, replaying

install()
result = {"replaying": replaying(), "tz": os.environ.get("TZ")}


def fetch(url):
    # A served 599 is a real HTTP status, so urllib raises for it exactly as
    # it would for any upstream error. Both arms report what the app saw.
    try:
        with urllib.request.urlopen(url) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


rows = db_run("SELECT id FROM issuers WHERE symbol = $1", ["ACME"])
result["rows"] = rows["rows"]

status, body = fetch("http://pricing.internal/prices?tier=gold")
result["status"] = status
result["body"] = json.loads(body) if body else None

# An unmatched call is a divergence: hard 599, never a fuzzy match.
result["divergedStatus"] = fetch("http://pricing.internal/unknown")[0]

result["draws"] = [random.random(), random.random()]
print(json.dumps(result))
"""


def _run_child(capture):
    with tempfile.TemporaryDirectory() as directory:
        path = os.path.join(directory, "capture.json")
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(capture, handle)
        environment = dict(os.environ, REPROIT_REPLAY=path)
        return subprocess.run(
            [sys.executable, "-c", CHILD],
            cwd=os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            capture_output=True,
            text=True,
            env=environment,
            timeout=60,
        )


def test_replay_serves_recorded_exchanges_without_dependencies():
    completed = _run_child(CAPTURE)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    assert result["replaying"] is True
    # The envelope pinned the clock zone.
    assert result["tz"] == "UTC"
    # The database answered with no database present.
    assert result["rows"] == [{"id": 7}]
    # The upstream answered with no network present.
    assert result["status"] == 200
    assert result["body"] == {"prices": None}
    # An unmatched call fails closed with the named status.
    assert result["divergedStatus"] == 599
    # The seeded RNG produced a deterministic stream in range.
    assert all(0.0 <= draw < 1.0 for draw in result["draws"])
    # The divergence is reported as a structured marker on stderr.
    marker = next(
        line for line in completed.stderr.splitlines() if line.startswith("REPROIT:DIVERGENCE ")
    )
    report = json.loads(marker[len("REPROIT:DIVERGENCE ") :])
    assert report["protocol"] == "http"
    assert report["got"]["method"] == "GET"
    # Consumption is counted across every protocol, matching the Node
    # reference: the database and the upstream call were both served first.
    assert report["consumed"] == 2
    assert report["total"] == 2


def test_the_seeded_stream_is_identical_across_runs():
    first = json.loads(_run_child(CAPTURE).stdout.strip().splitlines()[-1])
    second = json.loads(_run_child(CAPTURE).stdout.strip().splitlines()[-1])
    assert first["draws"] == second["draws"]


def test_a_truncated_body_fails_closed_instead_of_guessing():
    capture = json.loads(json.dumps(CAPTURE))
    exchange = capture["events"][2]["exchange"]["response"]
    exchange.pop("body")
    exchange.update({"bodyBytes": 99999, "bodySha256": "ab" * 32, "truncated": True})
    completed = _run_child(capture)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    # Serving a guessed body would be a silent lie; the app sees a hard 599
    # and the reason is named in the payload.
    assert result["status"] == 599
    assert result["body"] == {"reproit": "truncated-exchange-body"}


def test_an_unsupported_capture_version_is_refused():
    capture = json.loads(json.dumps(CAPTURE))
    capture["version"] = 99
    completed = _run_child(capture)
    assert completed.returncode != 0
    assert "unsupported capture version" in completed.stderr
