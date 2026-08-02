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
reproit internal debug replay-capture capture.json
```

Bounds, all fixed: queue depth 64 operations (drop-oldest on overflow), 16 operations per batch,
48 KB capture payload (trailing effect events dropped first, `captureDroppedEffects` counts
them), bounded flush interval, per-request timeout, and at most `retry_limit` (cap 5) retries;
4xx responses are never retried. Redaction runs in `begin`/`effect`/`finish`, before anything is
queued. Uploads use stdlib urllib on one daemon thread. `sdk/test/oracle_contract_test.js` pins
the `backend-server-error` tagging contract.

## Capsule parity (outbound capture + hermetic replay)

This SDK is at full capsule parity with the Node reference (`sdk/reproit-backend-node`), pinned
byte-for-byte by `sdk/test/backend_replay_parity_test.js` and the shared behavior vectors:

- Outbound exchange capture at the library layer: `http.client` (covers `requests`/`urllib3`),
  httpx sync + async, and aiohttp; streaming responses (SSE/chunked, the LLM shape) record their
  observed chunk boundaries in `response.stream` and the app still consumes the live stream.
- `wrap_psycopg(psycopg)` wraps the psycopg v3 driver: statements and results record as
  `pg`-protocol exchanges; in replay `psycopg.connect` returns an in-process stub.
- `REPROIT_REPLAY=<capture.json>` flips every hook from recorder to stub: strict per-operation
  ordinal matching, bodies modulo recorded `$reproit` placeholders, first unmatched call fails
  closed (599 / DivergedError) with the structured `REPROIT:DIVERGENCE` marker; prompt drift
  names the first differing message index for chat-shaped bodies. TZ, clock (`time.time`) and
  `random` pin from the capture envelope.

Named capability gaps (recorded here so they are never a silent downgrade):

- psycopg2 is NOT wrapped (different cursor surface); psycopg v3 is the covered driver.
- numpy's RNG is not seeded by the envelope; only the stdlib `random` module functions rebind.
- `datetime.datetime.now()` reads the C-level clock and is not pinned; `time.time`/`time_ns` are
  (the Node reference has the same shape: `Date.now` is pinned, `new Date()` internals are not).
- The stdlib `http.client` capture path drains the response in one read, so its recorded stream
  boundaries are coarse (whole-body); fine-grained boundaries come from the httpx/aiohttp hooks.
- Replayed JSON bodies re-serialize from the canonically stored capture (sorted keys, compact
  separators, identical to Node): an app comparing raw response TEXT against later raw request
  text can observe the reordering; structural matching is unaffected.

## CI capture mode (the flaky-CI wedge)

CI capture mode makes a failing TEST the capture trigger instead of an inbound request. With
`REPROIT_CI_CAPTURE=1` every test decorated through `ci.suite(...)` (pytest runner) runs inside
its own trace: the instrumented outbound clients record dependency exchanges and the determinism
envelope exactly as production capture does, and a FAILING test spools a replayable version-2
`reproit-backend-capture` capsule to a bounded on-disk spool. The test identity rides the
existing `operation` field as `test:<suite>#<test>` and the oracle is the existing
`backend-authored-invariant` id: no new wire fields, no new oracle ids.

```python
# tests/test_checkout.py
from reproit_backend_py import ci

test = ci.suite("checkout")

@test("order total applies the configured tax rate")
def test_order_total():
    assert order_total(100, CONFIG_URL) == 125
```

Run pytest with `-s` in both capture and replay runs: the `REPROIT:CI-CAPSULE` /
`REPROIT:CI-TEST` / `REPROIT:DIVERGENCE` stderr markers `reproit check` parses must not be
swallowed by pytest's output capture. This is the pytest analogue of the Node fixture's
direct-invocation note (run the file, not `node --test`, so no child process eats the markers):

```sh
REPROIT_CI_CAPTURE=1 uv run --group test python -m pytest -q -s tests/
```

This mode is fully local-first: it contacts no cloud and needs no API key. On a red job the repo
action's `test-command` mode (language-neutral: it sets the env and uploads the spool, so a
pytest command works unchanged) uploads the capsule as a job artifact and prints the repro
command; the developer then runs, on their own machine:

```sh
reproit check capsule-<digest>.json \
  --exec "uv run --group test python -m pytest -q -s tests/test_checkout.py"
```

The SDK re-executes ONLY the capsule's named test (every other decorated test is skipped) with
every recorded exchange served in process and the envelope pinned, and reports the observed
result as a `REPROIT:CI-TEST` marker the CLI maps to the four-way verdict: reproduced (1),
fixed (0), diverged (3), inconclusive. Honest limits: a plain rerun that happens to pass outside
the capsule is flaky evidence, never a Fixed verdict; only what the SDK boundary recorded is
replayed, so a race the boundary cannot see reports Inconclusive rather than a faked
reproduction.

Spool bounds, same numbers as the Node reference: `REPROIT_CI_SPOOL` names the directory
(default `.reproit/ci-spool`), `REPROIT_CI_SPOOL_MAX` caps its TOTAL bytes (default 16 MiB,
floor 4 KiB, ceiling 64 MiB); an over-cap capsule is dropped and counted in the on-disk
`dropped.count`, never silently. Fixture and gate: `examples/py-flaky-ci-fixture` (a planted
order-dependent failure invisible in a plain run) and `validation/backend/py-flaky-ci-e2e`
(six legs including the flaky-vs-fixed distinction).

## Level matrix against the Node reference

Founder rule: every capability the Node SDK (`sdk/reproit-backend-node`) has, this SDK has, and
genuinely-impossible surfaces are NAMED gaps, never silent. "At level" means the same behavior
under the same wire, pinned by the shared parity suite where bytes are comparable.

| Capability (Node reference surface)             | Python status                            |
| ----------------------------------------------- | ---------------------------------------- |
| Scan-time trace adapter, bounds, redaction      | At level (shared behavior vectors)       |
| Framework integration                           | At level: ASGI (Node: Express/Fastify)   |
| Production capture mode (sampling, bounds)      | At level                                 |
| Outbound HTTP exchange capture                  | At level: `http.client`, httpx, aiohttp  |
| Streaming chunk boundaries (SSE, LLM shape)     | At level via httpx/aiohttp; gap 1 below  |
| DB driver wrap                                  | At level: psycopg v3 (Node: pg); gap 2   |
| Hermetic replay, ordinal match, divergence      | At level (byte-pinned marker lines)      |
| Envelope pinning (TZ, clock, RNG)               | At level; gaps 3 and 4 below             |
| Agent oracle API (`trace.oracle`, `AGENT_*`)    | At level                                 |
| LLM flavor (one logical stream, `bodyDelta`)    | At level                                 |
| CI capture (test trigger, spool, result marker) | At level; runner is pytest; gap 5 below  |
| action.yml `test-command` mode                  | At level: action is language-neutral     |
| Fixture + acceptance gates                      | At level: `py-flaky-ci-e2e`, `py-hermetic-e2e` |

Named gaps behind the rows above (also listed under capsule parity):

1. The stdlib `http.client` capture path drains the response in one read, so its recorded
   stream boundaries are coarse (whole-body); fine-grained boundaries need httpx/aiohttp.
2. psycopg2 is NOT wrapped (different cursor surface); psycopg v3 is the covered driver.
3. `datetime.datetime.now()` reads the C-level clock and is not pinned; `time.time`/`time_ns`
   are (Node has the same shape: `Date.now` pinned, `new Date()` internals not).
4. numpy's RNG is not seeded by the envelope; only the stdlib `random` module rebinds.
5. pytest is the integrated runner and must run with `-s` (marker transport); unittest-style
   `TestCase` classes are not integrated, mirroring the Node reference's named node:test-only
   gap (jest later).

## Tests

```sh
cd sdk/reproit-backend-py
uv run --group test -m pytest tests/test_trace.py tests/test_capture.py   # unit, stdlib only
uv run --group test -m pytest tests/test_ci.py                            # CI capture mode
uv run --group e2e -m pytest tests/test_e2e.py                            # FastAPI + uvicorn e2e
```
