"""Bounded, interleaved per-dependency capture benchmark for Python."""
import json
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "../../sdk/reproit-backend-py"))
from reproit_backend_py import BackendTrace  # noqa: E402

RUNS = int(os.environ.get("REPROIT_DEPENDENCY_BENCH_RUNS", "300"))
ROUNDS = int(os.environ.get("REPROIT_DEPENDENCY_BENCH_ROUNDS", "7"))
DEPENDENCIES = 64
CONTEXT = {"trace_id": "dependency-benchmark", "action_index": 1}
EXCHANGE = {
    "request": {"method": "GET", "url": "http://pricing.test/quote?tier=gold"},
    "response": {"status": 200, "body": {"price": 42, "currency": "USD"}},
}


def measure(captured):
    started = time.perf_counter()
    for _ in range(RUNS):
        trace = BackendTrace.begin(CONTEXT, "dependencyBenchmark")
        if captured:
            for index in range(DEPENDENCIES):
                trace.effect("call", resource="pricing", key=str(index), exchange=EXCHANGE)
    return (time.perf_counter() - started) * 1_000_000 / (RUNS * DEPENDENCIES)


samples = {"baseline": [], "captured": [], "control": []}
for _ in range(ROUNDS):
    samples["baseline"].append(measure(False))
    samples["captured"].append(measure(True))
    samples["control"].append(measure(False))
median = lambda values: sorted(values)[len(values) // 2]
baseline = median(samples["baseline"])
cost = median(samples["captured"]) - baseline
noise = abs(median(samples["control"]) - baseline)
if noise >= 20 or cost >= 100:
    raise SystemExit(
        f"python dependency benchmark outside ceiling: noise={noise:.2f} cost={cost:.2f}"
    )
print(json.dumps({
    "language": "python", "runs": RUNS, "rounds": ROUNDS,
    "dependenciesPerTrace": DEPENDENCIES, "noiseFloorMicros": round(noise, 2),
    "captureCostMicros": round(cost, 2), "ceilingMicros": 100,
}))
