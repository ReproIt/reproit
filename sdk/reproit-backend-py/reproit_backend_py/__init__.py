"""Experimental Reproit backend adapter for Python (FastAPI / Starlette / ASGI).

Python port of sdk/reproit-backend-rs: a scan-time trace adapter that is inert
without `x-reproit-trace`, plus an off-by-default production capture mode.
"""

from .asgi import ReproitMiddleware
from .capture import (
    CAPTURE_FORMAT,
    CAPTURE_VERSION,
    CAPTURE_VERSION_EXCHANGES,
    SERVER_ERROR_ORACLE,
    Capture,
    determinism_envelope,
)
from .instrument import (
    MAX_EXCHANGE_BODY_BYTES,
    DivergedError,
    db_run,
    install,
    record_exchange,
    replaying,
)
from .trace import (
    MAX_EVENTS,
    MAX_HEADER_BYTES,
    BackendTrace,
    TraceError,
    canonical_json,
    current_trace,
    http_input,
    redact,
    selection,
    trace_context_from_headers,
    use_trace,
)

__all__ = [
    "BackendTrace",
    "Capture",
    "CAPTURE_FORMAT",
    "CAPTURE_VERSION",
    "CAPTURE_VERSION_EXCHANGES",
    "DivergedError",
    "MAX_EVENTS",
    "MAX_EXCHANGE_BODY_BYTES",
    "MAX_HEADER_BYTES",
    "ReproitMiddleware",
    "SERVER_ERROR_ORACLE",
    "TraceError",
    "canonical_json",
    "current_trace",
    "db_run",
    "determinism_envelope",
    "http_input",
    "install",
    "record_exchange",
    "redact",
    "replaying",
    "selection",
    "trace_context_from_headers",
    "use_trace",
]
