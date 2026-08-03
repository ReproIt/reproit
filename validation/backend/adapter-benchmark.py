"""What mounting the Python ASGI adapter actually costs a request.

The same method as adapter-benchmark.mjs and adapter-benchmark-go, so the
three numbers are comparable: a real uvicorn server over a real socket, driven
in four shapes with keep-alive on, measured in ALTERNATING rounds.

    baseline  the app alone
    inactive  adapter mounted, request carries no trace context (the shape
              almost every production request has)
    active    adapter mounted, request carries `x-reproit-trace`
    control   a second baseline, measured apart from the first

HTTP, socket and JSON costs are present in all four, so subtracting the
baseline leaves the adapter. The gap between the two baselines is the method's
own noise floor, reported so nobody reads a number smaller than it as signal:
a single pass per shape once put an inactive adapter at a NEGATIVE cost, which
is drift, not a result.

The app is a bare ASGI callable rather than FastAPI, because a router's own
per-request work is the same in all four shapes but large enough to shrink the
delta this exists to show.

Run it from sdk/reproit-backend-py so the SDK and uvicorn are importable:
    uv run --group e2e python ../../validation/backend/adapter-benchmark.py
"""

import http.client
import json
import os
import socket
import sys
import threading
import time

sys.path.insert(
    0,
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../sdk/reproit-backend-py"),
)

import uvicorn  # noqa: E402

from reproit_backend_py import ReproitMiddleware  # noqa: E402

RUNS = int(os.environ.get("REPROIT_ADAPTER_BENCH_RUNS") or 3000)
ROUNDS = int(os.environ.get("REPROIT_ADAPTER_BENCH_ROUNDS") or 5)
WARMUP = min(500, RUNS // 4)

# Ceilings, not targets, and sized for a shared CI runner rather than a
# developer laptop. A gate that flakes gets ignored, and an ignored gate
# measures nothing; these sit far above the local numbers so ordinary
# contention cannot fail a build, while an adapter that started doing real
# per-request work still would. Python's are wider than Node's and Go's
# because the interpreter's own per-request variance is wider.
NOISE_CEILING_MICROS = 250.0
INACTIVE_CEILING_MICROS = 250.0
ACTIVE_CEILING_MICROS = 800.0

BODY = json.dumps({"account": {"id": 42, "ok": True}}).encode()


async def app(scope, receive, send):
    """The handler alone: read the request, answer JSON."""
    while True:
        message = await receive()
        if not message.get("more_body"):
            break
    await send(
        {
            "type": "http.response.start",
            "status": 200,
            "headers": [
                (b"content-type", b"application/json"),
                (b"content-length", str(len(BODY)).encode()),
            ],
        }
    )
    await send({"type": "http.response.body", "body": BODY})


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


class Server:
    """A uvicorn server on its own thread, started and stopped explicitly so
    every shape is measured against a fresh process-local server."""

    def __init__(self, served):
        self.port = free_port()
        config = uvicorn.Config(
            served,
            host="127.0.0.1",
            port=self.port,
            log_level="error",
            access_log=False,
        )
        self.server = uvicorn.Server(config)
        self.thread = threading.Thread(target=self.server.run, daemon=True)

    def __enter__(self):
        self.thread.start()
        deadline = time.monotonic() + 30
        while not self.server.started:
            if time.monotonic() > deadline:
                raise RuntimeError("uvicorn did not start within 30s")
            time.sleep(0.01)
        return self

    def __exit__(self, *_):
        self.server.should_exit = True
        self.thread.join(timeout=30)


def measure(mounted, traced):
    """One shape, as microseconds per request. Keep-alive is on and a single
    connection is used, because otherwise connection setup dominates and the
    adapter disappears into the noise, which would flatter the result rather
    than measure it."""
    served = ReproitMiddleware(app) if mounted else app
    headers = {"x-reproit-trace": "bench-trace"} if traced else {}
    with Server(served) as server:
        connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=30)
        try:

            def fire():
                connection.request("GET", "/account?id=42", headers=headers)
                connection.getresponse().read()

            for _ in range(WARMUP):
                fire()
            started = time.perf_counter()
            for _ in range(RUNS):
                fire()
            return (time.perf_counter() - started) * 1_000_000 / RUNS
        finally:
            connection.close()


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def main():
    samples = {"baseline": [], "inactive": [], "active": [], "control": []}
    for _ in range(ROUNDS):
        samples["baseline"].append(measure(False, False))
        samples["inactive"].append(measure(True, False))
        samples["active"].append(measure(True, True))
        samples["control"].append(measure(False, False))

    baseline = median(samples["baseline"])
    inactive = median(samples["inactive"])
    active = median(samples["active"])
    # Two identical shapes measured apart: whatever separates them is noise,
    # so a smaller difference cannot be called a cost.
    noise_floor = abs(median(samples["control"]) - baseline)
    inactive_cost = inactive - baseline
    active_cost = active - baseline

    print(
        json.dumps(
            {
                "language": "python",
                "runs": RUNS,
                "rounds": ROUNDS,
                "noiseFloorMicros": round(noise_floor, 2),
                "baselineMicros": round(baseline, 2),
                "inactiveMicros": round(inactive, 2),
                "activeMicros": round(active, 2),
                "inactiveCostMicros": round(inactive_cost, 2),
                "activeCostMicros": round(active_cost, 2),
                "inactiveBelowNoiseFloor": inactive_cost < noise_floor,
            }
        )
    )

    failures = []
    if noise_floor >= NOISE_CEILING_MICROS:
        failures.append(
            "the method's own noise is %.2fus, too loud for this run to mean anything"
            % noise_floor
        )
    if inactive_cost >= INACTIVE_CEILING_MICROS:
        failures.append(
            "inactive adapter adds %.2fus per request, over the %.0fus ceiling"
            % (inactive_cost, INACTIVE_CEILING_MICROS)
        )
    if active_cost >= ACTIVE_CEILING_MICROS:
        failures.append(
            "active adapter adds %.2fus per request, over the %.0fus ceiling"
            % (active_cost, ACTIVE_CEILING_MICROS)
        )
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
