#!/usr/bin/env python3
"""Hosted strict-ingest and replay-package benchmark for the release gate."""

import http.client
import json
import math
import os
import statistics
import time
import urllib.parse
import uuid

BASE = os.environ["REPROIT_BENCH_BASE"].rstrip("/")
APP = os.environ["REPROIT_BENCH_APP"]
PROJECT_KEY = os.environ["REPROIT_BENCH_PROJECT_KEY"]
PUBLISHABLE_KEY = os.environ["REPROIT_BENCH_PUBLISHABLE_KEY"]
OUT = os.environ["REPROIT_BENCH_OUT"]
BATCHES = int(os.environ.get("REPROIT_BENCH_BATCHES", "20"))
ERRORS_PER_BATCH = int(os.environ.get("REPROIT_BENCH_ERRORS_PER_BATCH", "25"))
P95_CEILING_MS = float(os.environ.get("REPROIT_BENCH_P95_MS", "5000"))
MAX_CEILING_MS = float(os.environ.get("REPROIT_BENCH_MAX_MS", "10000"))
SENTINEL = f"release-gate-private-{uuid.uuid4().hex}"

parsed = urllib.parse.urlparse(BASE)
connection_type = (
    http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
)
connection = connection_type(parsed.hostname, parsed.port, timeout=15)


def request_json(method, path, token, body=None):
    encoded = None if body is None else json.dumps(body, separators=(",", ":")).encode()
    headers = {
        "authorization": f"Bearer {token}",
        "accept": "application/json",
        "user-agent": "reproit-production-release-gate",
    }
    if encoded is not None:
        headers["content-type"] = "application/json"
    started = time.perf_counter()
    connection.request(method, parsed.path.rstrip("/") + path, body=encoded, headers=headers)
    response = connection.getresponse()
    raw = response.read()
    elapsed_ms = (time.perf_counter() - started) * 1000
    payload = json.loads(raw or b"{}")
    if response.status < 200 or response.status >= 300:
        raise RuntimeError(f"{method} {path} failed ({response.status}): {payload}")
    return payload, elapsed_ms


def percentile(values, percentile_value):
    ordered = sorted(values)
    index = max(0, math.ceil((percentile_value / 100) * len(ordered)) - 1)
    return ordered[index]


path = [
    {"signature": "home", "action": "tap:key:testid:contract-crash", "label": None},
]


def event_batch(batch_label, finding_count):
    batch_id = f"release-gate-{batch_label}-{uuid.uuid4().hex}"
    frames = []
    sequence = 1
    for occurrence in range(finding_count):
        run_id = f"release-gate-{batch_label}-{occurrence}-{uuid.uuid4().hex}"
        frames.append(
            {
                "runId": run_id,
                "sequence": sequence,
                "scope": {"domain": "shared"},
                "event": {
                    "kind": "graph-edge",
                    "from": "home",
                    "action": "tap:key:testid:contract-crash",
                    "to": "contract-crash",
                },
            }
        )
        sequence += 1
        frames.append(
            {
                "runId": run_id,
                "sequence": sequence,
                "scope": {"domain": "shared"},
                "event": {
                    "kind": "finding",
                    "signature": "crash:ReproitContractError:production-gate",
                    "message": "TypeError: ReproitContractError",
                    "identity": {
                        "oracle": "crash",
                        "invariant": "no-exception",
                        "kind": "exception",
                        "message": "TypeError: ReproitContractError",
                        "frame": "contract-crash",
                        "trigger": "tap:key:testid:contract-crash",
                        "boundary": "contract-crash",
                    },
                    "path": path,
                    "context": {
                        "email": {"$reproit": {"redacted": True}},
                        "nested": {
                            "refreshToken": {"$reproit": {"redacted": True}},
                            "displayName": "Release Gate",
                        },
                        "fingerprint": [
                            {
                                "field": "checkout-name",
                                "len": 18,
                                "bytes": 18,
                                "charset": "ascii",
                                "scripts": ["Latin"],
                            }
                        ],
                        "fpVersion": 2,
                    },
                },
            }
        )
        sequence += 1
    batch = {
        "version": 1,
        "batchId": batch_id,
        "appId": APP,
        "deployment": {"version": "production-release-gate"},
        "frames": frames,
        "evidence": [],
    }
    if SENTINEL in json.dumps(batch, separators=(",", ":")):
        raise RuntimeError("raw sensitive sentinel escaped into the strict event batch")
    return batch

# Warm the scale-to-zero service and persistent connection with a valid strict
# protocol batch. A graph edge is clean traffic, not a finding.
for index in range(2):
    warmup = event_batch(f"warmup-{index}", 1)
    warmup["frames"] = warmup["frames"][:1]
    request_json(
        "POST",
        "/v1/events",
        PUBLISHABLE_KEY,
        warmup,
    )

latencies = []
started_all = time.perf_counter()
for batch_index in range(BATCHES):
    response, elapsed_ms = request_json(
        "POST",
        "/v1/events",
        PUBLISHABLE_KEY,
        event_batch(str(batch_index), ERRORS_PER_BATCH),
    )
    if response.get("ingested", {}).get("errors") != ERRORS_PER_BATCH:
        raise RuntimeError(f"unexpected ingest response: {response}")
    latencies.append(elapsed_ms)
elapsed_all = time.perf_counter() - started_all

buckets, bucket_list_ms = request_json("GET", f"/v1/apps/{APP}/buckets", PROJECT_KEY)
bucket = next(
    (
        item
        for item in buckets.get("items", [])
        if item.get("crashSig") == "crash:ReproitContractError:production-gate"
        or "ReproitContractError" in json.dumps(item)
    ),
    None,
)
if not bucket:
    raise RuntimeError(f"contract bucket missing: {buckets}")
bucket_id = bucket["bucketId"]
package, package_ms = request_json(
    "GET",
    f"/v1/apps/{APP}/buckets/{bucket_id}",
    os.environ.get("REPROIT_CLOUD_KEY", PROJECT_KEY),
)
encoded_package = json.dumps(package, separators=(",", ":"))
if SENTINEL in encoded_package:
    raise RuntimeError("raw sensitive sentinel escaped into the production replay package")
if package.get("context", {}).get("email", {}).get("$reproit", {}).get("redacted") is not True:
    raise RuntimeError(f"email redaction marker was not preserved: {package.get('context')}")
if (
    package.get("context", {})
    .get("nested", {})
    .get("refreshToken", {})
    .get("$reproit", {})
    .get("redacted")
    is not True
):
    raise RuntimeError(f"nested token redaction marker was not preserved: {package.get('context')}")

p95 = percentile(latencies, 95)
maximum = max(latencies)
if p95 > P95_CEILING_MS:
    raise RuntimeError(f"hosted ingest p95 {p95:.1f}ms exceeds {P95_CEILING_MS:.1f}ms")
if maximum > MAX_CEILING_MS:
    raise RuntimeError(f"hosted ingest max {maximum:.1f}ms exceeds {MAX_CEILING_MS:.1f}ms")

result = {
    "base": BASE,
    "batches": BATCHES,
    "errorsPerBatch": ERRORS_PER_BATCH,
    "occurrences": BATCHES * ERRORS_PER_BATCH,
    "ingestP50Ms": round(statistics.median(latencies), 2),
    "ingestP95Ms": round(p95, 2),
    "ingestMaxMs": round(maximum, 2),
    "ingestOccurrencesPerSecond": round((BATCHES * ERRORS_PER_BATCH) / elapsed_all, 2),
    "bucketListMs": round(bucket_list_ms, 2),
    "packageMs": round(package_ms, 2),
    "bucketId": bucket_id,
    "redaction": {
        "rawSentinelAbsent": True,
        "emailMetadataPreserved": True,
        "nestedTokenMetadataPreserved": True,
    },
}
with open(OUT, "w") as output:
    json.dump(result, output, indent=2, sort_keys=True)
    output.write("\n")
print(json.dumps(result, indent=2, sort_keys=True))
