"""Experimental Reproit backend adapter for Python (FastAPI / Starlette / ASGI).

Python port of sdk/reproit-backend-rs: a scan-time trace adapter that is inert
without `x-reproit-trace`, plus an off-by-default production capture mode.
"""

from .asgi import ReproitMiddleware
from .capture import CAPTURE_FORMAT, CAPTURE_VERSION, SERVER_ERROR_ORACLE, Capture
from .trace import (
    MAX_EVENTS,
    MAX_HEADER_BYTES,
    BackendTrace,
    TraceError,
    canonical_json,
    http_input,
    redact,
    selection,
    trace_context_from_headers,
)

__all__ = [
    "BackendTrace",
    "Capture",
    "CAPTURE_FORMAT",
    "CAPTURE_VERSION",
    "MAX_EVENTS",
    "MAX_HEADER_BYTES",
    "ReproitMiddleware",
    "SERVER_ERROR_ORACLE",
    "TraceError",
    "canonical_json",
    "http_input",
    "redact",
    "selection",
    "trace_context_from_headers",
]
