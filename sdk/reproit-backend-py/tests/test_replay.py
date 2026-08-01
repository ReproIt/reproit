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


# httpx never touches http.client, so replay must serve it at the transport
# boundary too. This child proves the same capsule answers an httpx client
# with no network present, and that an unmatched httpx call fails closed.
HTTPX_CHILD = r"""
import json, sys
from reproit_backend_py import install, replaying

install()
import httpx

result = {"replaying": replaying()}
response = httpx.get("http://pricing.internal/prices?tier=gold")
result["status"] = response.status_code
result["body"] = response.json()
result["diverged"] = httpx.get("http://pricing.internal/unknown").status_code
print(json.dumps(result))
"""


def _run_httpx_child(capture):
    with tempfile.TemporaryDirectory() as directory:
        path = os.path.join(directory, "capture.json")
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(capture, handle)
        return subprocess.run(
            [sys.executable, "-c", HTTPX_CHILD],
            cwd=os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            capture_output=True,
            text=True,
            env=dict(os.environ, REPROIT_REPLAY=path),
            timeout=60,
        )


def _exchange_event(sequence, exchange):
    """Minimal exchange-bearing event: replay only reads kind and exchange."""
    return {
        "operation": "GET /quote",
        "sequence": sequence,
        "kind": "effect",
        "effect": "call",
        "exchange": exchange,
    }


# LLM-shaped traffic, mirroring the Node reference suite: a streamed SSE
# completion, a tool-call loop whose recorded order interleaves another
# operation, a chat exchange the prompt-drift test tampers against, and the
# pg exchanges the psycopg stub serves.
LLM_CAPTURE = {
    "format": "reproit-backend-capture",
    "version": 2,
    "operation": "GET /quote",
    "oracle": "backend-server-error",
    "envelope": {
        "observedAtMs": 1753747200000,
        "tz": "UTC",
        "replaySeed": "00ff00ff00ff00ff",
    },
    "events": [
        _exchange_event(
            1,
            {
                "protocol": "http",
                "request": {"method": "GET", "url": "http://llm.internal/stream"},
                "response": {
                    "status": 200,
                    "headers": {"content-type": "text/event-stream"},
                    "body": "data: a\n\ndata: b\n\ndata: c\n\n",
                    "stream": {"chunks": [9, 9, 9]},
                },
            },
        ),
        _exchange_event(
            2,
            {
                "protocol": "http",
                "request": {
                    "method": "POST",
                    "url": "http://llm.internal/v1/messages",
                    "body": {"model": "m", "messages": [{"role": "user", "content": "q"}]},
                },
                "response": {
                    "status": 200,
                    "headers": {"content-type": "application/json"},
                    "body": {"reply": "r0"},
                },
            },
        ),
        _exchange_event(
            3,
            {
                "protocol": "http",
                "request": {
                    "method": "POST",
                    "url": "http://tools.internal/run",
                    "body": {"tool": "x"},
                },
                "response": {
                    "status": 200,
                    "headers": {"content-type": "application/json"},
                    "body": {"ok": True},
                },
            },
        ),
        _exchange_event(
            4,
            {
                "protocol": "http",
                "request": {
                    "method": "POST",
                    "url": "http://llm.internal/v1/messages",
                    "body": {
                        "model": "m",
                        "messages": [
                            {"role": "user", "content": "q"},
                            {"role": "assistant", "content": "r0"},
                            {"role": "user", "content": "tool: ok"},
                        ],
                    },
                },
                "response": {
                    "status": 200,
                    "headers": {"content-type": "application/json"},
                    "body": {"reply": "r1"},
                },
            },
        ),
        _exchange_event(
            5,
            {
                "protocol": "http",
                "request": {
                    "method": "POST",
                    "url": "http://llm.internal/v1/chat",
                    "body": {
                        "messages": [
                            {"role": "user", "content": "hello"},
                            {"role": "assistant", "content": "hi"},
                            {"role": "user", "content": "weather?"},
                        ]
                    },
                },
                "response": {
                    "status": 200,
                    "headers": {"content-type": "application/json"},
                    "body": {"reply": "sunny"},
                },
            },
        ),
        _exchange_event(
            6,
            {
                "protocol": "pg",
                "request": {
                    "text": "SELECT id, symbol FROM issuers WHERE symbol = %s",
                    "values": ["ACME"],
                },
                "response": {"command": "SELECT", "rowCount": 1, "rows": [[7, "ACME"]]},
            },
        ),
        {
            "operation": "GET /quote",
            "sequence": 7,
            "kind": "return",
            "output": {"error": "internal"},
            "status": 500,
            "success": False,
            "effectsComplete": True,
        },
    ],
}


# Per-operation ordinals: recorded order is messages[0], tool, messages[1];
# the live code asks for both messages calls FIRST. Each operation is served
# in its own recorded order without a cross-operation divergence. Then the
# prompt-drift case: a chat body whose third message differs must diverge
# naming message index 2.
LLM_CHILD = r"""
import json, sys, urllib.error, urllib.request
from reproit_backend_py import install

install()
result = {}


def post(url, body):
    data = json.dumps(body).encode("utf-8")
    request = urllib.request.Request(
        url, data=data, headers={"Content-Type": "application/json"}, method="POST"
    )
    try:
        with urllib.request.urlopen(request) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read())


result["first"] = post(
    "http://llm.internal/v1/messages",
    {"model": "m", "messages": [{"role": "user", "content": "q"}]},
)
result["second"] = post(
    "http://llm.internal/v1/messages",
    {
        "model": "m",
        "messages": [
            {"role": "user", "content": "q"},
            {"role": "assistant", "content": "r0"},
            {"role": "user", "content": "tool: ok"},
        ],
    },
)
result["tool"] = post("http://tools.internal/run", {"tool": "x"})
result["drift"] = post(
    "http://llm.internal/v1/chat",
    {
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi"},
            {"role": "user", "content": "DIFFERENT QUESTION"},
        ]
    },
)
print(json.dumps(result))
"""


def _run_named_child(source, capture):
    with tempfile.TemporaryDirectory() as directory:
        path = os.path.join(directory, "capture.json")
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(capture, handle)
        return subprocess.run(
            [sys.executable, "-c", source],
            cwd=os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            capture_output=True,
            text=True,
            env=dict(os.environ, REPROIT_REPLAY=path),
            timeout=60,
        )


def test_tool_call_loops_match_per_operation_ordinals_across_interleaving():
    completed = _run_named_child(LLM_CHILD, LLM_CAPTURE)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    assert result["first"] == [200, {"reply": "r0"}]
    assert result["second"] == [200, {"reply": "r1"}]
    assert result["tool"] == [200, {"ok": True}]


def test_prompt_drift_names_the_first_differing_message_index():
    completed = _run_named_child(LLM_CHILD, LLM_CAPTURE)
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    assert result["drift"][0] == 599
    marker = next(
        line for line in completed.stderr.splitlines() if line.startswith("REPROIT:DIVERGENCE ")
    )
    report = json.loads(marker[len("REPROIT:DIVERGENCE ") :])
    assert report["bodyDelta"] == {
        "kind": "message",
        "firstDifferingMessage": 2,
        "recordedMessages": 3,
        "liveMessages": 3,
    }
    # The marker is byte-comparable with the Node reference: compact JSON,
    # and the delta text greppable exactly as the llm-agent gate greps it.
    assert '"firstDifferingMessage":2' in marker


def test_unknown_body_shapes_fall_back_to_the_first_differing_byte_offset():
    from reproit_backend_py import replay as replay_module

    delta = replay_module.body_delta({"prompt": "summarize A"}, {"prompt": "summarize B"})
    assert delta["kind"] == "byte"
    # The same offset the Node reference pins: the first differing byte of
    # the compact JSON encodings.
    assert delta["offset"] == len('{"prompt":"summarize ')
    assert replay_module.body_delta({"a": 1}, {"a": 1}) is None


# The recorded SSE stream re-serves chunk for chunk through httpx, whose
# stream API exposes the chunk boundaries the app observes.
SSE_CHILD = r"""
import json
from reproit_backend_py import install

install()
import httpx

chunks = []
with httpx.Client() as client:
    with client.stream("GET", "http://llm.internal/stream") as response:
        status = response.status_code
        for chunk in response.iter_raw():
            chunks.append(chunk.decode("utf-8"))
print(json.dumps({"status": status, "chunks": chunks}))
"""


def test_recorded_sse_streams_reserve_chunk_for_chunk():
    completed = _run_named_child(SSE_CHILD, LLM_CAPTURE)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    assert result["status"] == 200
    assert result["chunks"] == ["data: a\n\n", "data: b\n\n", "data: c\n\n"]


def test_truncated_stream_boundaries_fail_closed():
    capture = json.loads(json.dumps(LLM_CAPTURE))
    capture["events"][0]["exchange"]["response"]["stream"]["truncated"] = True
    completed = _run_named_child(SSE_CHILD, capture)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    # Serving a guessed stream shape would be a silent lie; hard 599 instead.
    assert result["status"] == 599


# The envelope pins the process clock to the capture moment, so replayed code
# reading time.time() observes the recorded moment, not the machine's.
CLOCK_CHILD = r"""
import json, time
from reproit_backend_py import install

install()
print(json.dumps({"now": time.time(), "now_ns": time.time_ns()}))
"""


def test_the_envelope_pins_the_clock_to_the_capture_moment():
    completed = _run_named_child(CLOCK_CHILD, LLM_CAPTURE)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    observed = LLM_CAPTURE["envelope"]["observedAtMs"] / 1000.0
    assert abs(result["now"] - observed) < 60.0
    assert abs(result["now_ns"] / 1e9 - observed) < 60.0


# psycopg replay: the wrapped driver's connect returns an in-process stub, so
# application code runs its normal connect/cursor/execute/fetch path with the
# database stopped, and a statement the capture never saw raises.
PSYCOPG_CHILD = r"""
import json
from reproit_backend_py import DivergedError, install, wrap_psycopg

install()


class FakeCursor:
    def execute(self, query, params=None):
        raise AssertionError("live database reached during hermetic replay")

    def fetchone(self):
        raise AssertionError("live database reached during hermetic replay")

    fetchmany = fetchall = fetchone


class FakePsycopg:
    Cursor = FakeCursor

    def connect(self, *args, **kwargs):
        raise AssertionError("live database dialed during hermetic replay")


pg = wrap_psycopg(FakePsycopg())
result = {}
with pg.connect("postgresql://db.internal/quotes") as connection:
    cursor = connection.cursor()
    cursor.execute("SELECT id, symbol FROM issuers WHERE symbol = %s", ["ACME"])
    result["row"] = list(cursor.fetchone())
    result["rest"] = cursor.fetchall()
    try:
        cursor.execute("SELECT * FROM tables_the_capture_never_saw")
        result["diverged"] = False
    except DivergedError:
        result["diverged"] = True
print(json.dumps(result))
"""


def test_psycopg_replay_serves_rows_with_the_database_stopped():
    completed = _run_named_child(PSYCOPG_CHILD, LLM_CAPTURE)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    # Recorded [7, "ACME"] serves as the driver-shaped tuple row.
    assert result["row"] == [7, "ACME"]
    assert result["rest"] == []
    assert result["diverged"] is True
    marker = next(
        line for line in completed.stderr.splitlines() if line.startswith("REPROIT:DIVERGENCE ")
    )
    assert json.loads(marker[len("REPROIT:DIVERGENCE ") :])["protocol"] == "pg"


# aiohttp replay: the session hook returns an in-process stand-in; a recorded
# stream shape is observable through content iteration.
AIOHTTP_CHILD = r"""
import asyncio, json
from reproit_backend_py import install

install()
import aiohttp


async def main():
    result = {}
    async with aiohttp.ClientSession() as session:
        async with session.get("http://llm.internal/stream") as response:
            result["status"] = response.status
            chunks = []
            async for chunk in response.content.iter_any():
                chunks.append(chunk.decode("utf-8"))
            result["chunks"] = chunks
        async with session.post(
            "http://llm.internal/v1/messages",
            json={"model": "m", "messages": [{"role": "user", "content": "q"}]},
        ) as response:
            result["reply"] = await response.json()
        async with session.get("http://llm.internal/unknown") as response:
            result["diverged"] = response.status
    return result


print(json.dumps(asyncio.run(main())))
"""


def test_aiohttp_is_served_from_the_capsule_with_no_network():
    completed = _run_named_child(AIOHTTP_CHILD, LLM_CAPTURE)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    assert result["status"] == 200
    assert result["chunks"] == ["data: a\n\n", "data: b\n\n", "data: c\n\n"]
    assert result["reply"] == {"reply": "r0"}
    assert result["diverged"] == 599


def test_httpx_is_served_from_the_capsule_with_no_network():
    capture = json.loads(json.dumps(CAPTURE))
    # Drop the database exchange so the httpx call is the next unconsumed
    # entry; this child makes no database call.
    capture["events"] = [
        event
        for event in capture["events"]
        if (event.get("exchange") or {}).get("protocol") != "db"
    ]
    completed = _run_httpx_child(capture)
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout.strip().splitlines()[-1])
    assert result["replaying"] is True
    # pricing.internal does not resolve: this answer can only come from the
    # capsule.
    assert result["status"] == 200
    assert result["body"] == {"prices": None}
    # An unmatched httpx call fails closed exactly like the stdlib path.
    assert result["diverged"] == 599
    marker = next(
        line for line in completed.stderr.splitlines() if line.startswith("REPROIT:DIVERGENCE ")
    )
    assert json.loads(marker[len("REPROIT:DIVERGENCE ") :])["protocol"] == "http"
