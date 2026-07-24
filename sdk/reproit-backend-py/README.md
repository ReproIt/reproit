# ReproIt backend adapter for Python

This package is an internal validation surface, not a published compatibility API. It is inactive
unless a trusted request contains `x-reproit-trace`. It is a port of the Rust reference adapter
(`sdk/reproit-backend-rs`) with the same bounds, redaction, and wire format.

Framework integrations pass their header lookup into `trace_context_from_headers`, start an
operation with `BackendTrace.begin`, record only effects actually observed by the adapter, then
call `finish` and return `header()` as `x-reproit-events`. Set `effects_complete` only when the
adapter observed every persistent effect in the operation. Tenant and resource identifiers must be
non-secret structural identifiers.

The adapter enforces bounded identifiers, 256 events, a 60 KB encoded header, typed effects, one
return, no effects after return, hashed idempotency identity, and recursive structural redaction.
GraphQL callers may attach parser-produced `selection` mappings; never infer selections from
response content.

## FastAPI / Starlette middleware

`ReproitMiddleware` is a pure ASGI middleware: it builds the canonical decoded input (JSON body up
to 64 KB, decoded query values, lowercased headers), begins the trace, and finishes it when the
response starts, attaching `x-reproit-events` on scan-time requests. Handlers record observed
effects through `request.state.reproit`:

```python
from fastapi import FastAPI, Request
from reproit_backend_py import Capture, ReproitMiddleware

capture = Capture.create(
    "https://cloud.example.com/v1/events",  # ingest endpoint
    "sk_live_...",                          # project API key (Authorization: Bearer)
    "app-id",                               # Cloud project app id
    build="1.4.2",                          # optional deployment identity
)
app = FastAPI()
app.add_middleware(ReproitMiddleware, capture=capture)  # capture=None: scan-time only

@app.post("/orders")
async def create_order(request: Request):
    trace = getattr(request.state, "reproit", None)
    if trace is not None:
        trace.effect("write", resource="orders", key="1")
    ...
```

Every adapter path fails closed: an instrumentation defect never breaks the request.

## Production capture mode (off by default)

Capture mode uploads finished traces to Cloud ingest without requiring `x-reproit-trace`. It is
config-gated: nothing leaves the process unless the host constructs a `Capture`.
`Capture.create(...)` returns `None` (capture disabled, host unaffected) when the config is
unusable. `capture.record(trace)` never blocks, never raises, and never surfaces errors.

Sampling: operations whose return reports `success == False` or HTTP 5xx are always captured;
healthy operations are captured only under `healthy_sample_per_mille` (default 0, backend frames
only, no finding). A 5xx capture is posted as an event-batch-v1 batch: every trace event as a
`backend` frame plus one `finding` frame tagged with the first-class `backend-server-error`
oracle id, whose `context.reproitCapture` object carries the full redacted start/effects/return
sequence for deterministic local replay:

```sh
# fetch the finding from /v1/errors/:app, save context.reproitCapture as capture.json, then:
reproit debug replay-capture capture.json
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), 16 operations per batch,
48 KB capture payload (trailing effect events dropped first, `captureDroppedEffects` counts
them), bounded flush interval, per-request timeout, and at most `retry_limit` (cap 5) retries;
4xx responses are never retried. Redaction runs in `begin`/`effect`/`finish`, before anything is
queued. Uploads use stdlib urllib on one daemon thread. `sdk/test/oracle_contract_test.js` pins
the `backend-server-error` tagging contract.

## Tests

```sh
cd sdk/reproit-backend-py
uv run --group test -m pytest tests/test_trace.py tests/test_capture.py   # unit, stdlib only
uv run --group e2e -m pytest tests/test_e2e.py                            # FastAPI + uvicorn e2e
```
