"""Outbound-exchange capture and hermetic replay for reproit-backend-py.

`install()` patches `http.client.HTTPConnection`, the single stdlib chokepoint
every mainstream Python HTTP client funnels through (`urllib.request`,
`requests`, and `urllib3`), so any dependency call made while a request trace
is ambient is recorded on that trace as an `effect` event carrying an
`exchange`: the request the app sent and the response the dependency
returned. Databases have no equivalent stdlib chokepoint and this SDK takes
no driver dependency, so `db_run(text, values, live)` is the explicit
boundary, the same decision the Rust adapter made for the same reason.

`httpx` and `aiohttp` do NOT go through `http.client`: each carries its own
transport, so the stdlib hook cannot see them. `install()` therefore also
hooks them when importable (httpx_client.py, aiohttp_client.py), preserving
streaming: chunk boundaries are observed as the app consumes the body, the
LLM SSE shape. These are OPTIONAL paths: the imports are attempted at install
time and their absence is not an error, so the SDK keeps its zero-dependency
runtime. `wrap_psycopg(psycopg)` wraps the psycopg (v3) driver the same way
the Node reference wraps pg.

The exchange is what deterministic local replay stubs, so responses are
captured verbatim up to a fixed inline budget; an over-budget body keeps its
byte count and sha256 and is marked truncated (replay fails closed on it with
a named reason instead of guessing). Capture fails closed the other way: an
instrumentation defect must never break the host app's request.

With `REPROIT_REPLAY` naming a capture payload the same hooks SERVE the
recorded exchanges: no socket is opened and no database is reached, so the
application re-executes exactly what production saw. See replay.py.

Python port of sdk/reproit-backend-node/instrument.js.
"""

import hashlib
import http.client
import io
import json
import os

from . import replay as _replay
from .trace import current_trace

# Inline body budget per exchange side. Beyond it the body is dropped and only
# provable identity (byte count + sha256) remains.
MAX_EXCHANGE_BODY_BYTES = 8 * 1024
# Recorded headers are capped to keep events bounded.
MAX_EXCHANGE_HEADERS = 32
# Rows recorded per database result; beyond it the result is marked truncated.
MAX_DB_ROWS = 64
# Stream chunk boundaries recorded per exchange (SSE / chunked responses, the
# LLM streaming shape). Beyond it the boundaries are marked truncated and
# replay fails closed rather than serve a wrong stream shape.
MAX_STREAM_CHUNKS = 128
# A response this large is passed through UNCAPTURED rather than buffered, so
# a large download inside a traced request cannot balloon the host's memory.
# Replay of such a call diverges (fails closed) instead of inventing bytes.
MAX_TEE_BYTES = 8 * 1024 * 1024

_STATE = {
    "installed": False,
    # Hermetic replay session, present only when REPROIT_REPLAY names a
    # capture payload. In that mode the hooks SERVE recorded exchanges
    # instead of recording live ones.
    "session": None,
    "stats": {"captured_exchanges": 0, "truncated_bodies": 0, "failed_captures": 0},
    # True when the named client library was importable at install time and
    # its transport is hooked; False means the host app does not have it and
    # no hook was needed.
    "httpx": False,
    "aiohttp": False,
}


class DivergedError(RuntimeError):
    """A call the capture never saw. Raised by the database boundary; the
    HTTP boundary reports the same condition as a 599 response."""


def stats():
    return dict(_STATE["stats"])


def replaying():
    return _STATE["session"] is not None


def _bounded_body(body, content_type):
    """Bound one exchange body. JSON bodies are parsed so structural redaction
    in the trace layer sees fields, not text."""
    if body is None:
        return {}
    # A BodyCollector that overflowed already reduced the body to provable
    # identity; pass it through instead of stringifying the identity dict.
    if isinstance(body, dict) and body.get("truncated") is True:
        return body
    raw = body if isinstance(body, (bytes, bytearray)) else str(body).encode("utf-8")
    if len(raw) == 0:
        return {}
    if len(raw) > MAX_EXCHANGE_BODY_BYTES:
        _STATE["stats"]["truncated_bodies"] += 1
        return {
            "bodyBytes": len(raw),
            "bodySha256": hashlib.sha256(raw).hexdigest(),
            "truncated": True,
        }
    text = raw.decode("utf-8", errors="replace")
    if isinstance(content_type, str) and "application/json" in content_type:
        try:
            return {"body": json.loads(text)}
        except ValueError:
            # Declared JSON that does not parse is recorded as text below.
            pass
    return {"body": text}


def _bounded_headers(headers):
    # The cap is defined over NAME SORTED order, never arrival order: Go capped
    # a randomized map first and recorded a different subset each run. A dict
    # here has a stable order per run, but it is the client's arbitrary order,
    # so two callers sending the same request recorded two different subsets.
    items = headers.items() if hasattr(headers, "items") else headers
    lowered = sorted(
        ((str(name).lower(), str(value)) for name, value in items),
        key=lambda pair: pair[0],
    )
    bounded = dict(lowered[:MAX_EXCHANGE_HEADERS])
    return {"headers": bounded} if bounded else {}


def _header_value(headers, name):
    for key, value in headers.items() if hasattr(headers, "items") else headers:
        if str(key).lower() == name:
            return str(value)
    return None


class BodyCollector:
    """Collect a stream's chunks up to one byte past the inline budget; enough
    to know the true size class without holding unbounded memory. The sha256
    runs over EVERY byte so truncated identity stays provable. Chunk
    boundaries are recorded as observed byte lengths, bounded by
    MAX_STREAM_CHUNKS; boundaries past the cap are counted, never guessed.

    Python port of instrument.js bodyCollector."""

    def __init__(self):
        self._chunks = []
        self._boundaries = []
        self._bytes = 0
        self._dropped_boundaries = 0
        self._hash = hashlib.sha256()

    def push(self, chunk):
        raw = chunk if isinstance(chunk, (bytes, bytearray)) else str(chunk).encode("utf-8")
        self._bytes += len(raw)
        self._hash.update(raw)
        if len(self._boundaries) < MAX_STREAM_CHUNKS:
            self._boundaries.append(len(raw))
        else:
            self._dropped_boundaries += 1
        if self._bytes <= MAX_EXCHANGE_BODY_BYTES:
            self._chunks.append(bytes(raw))

    def result(self):
        """The collected body: None when empty, provable identity when over
        budget, the raw bytes otherwise."""
        if self._bytes == 0:
            return None
        if self._bytes > MAX_EXCHANGE_BODY_BYTES:
            _STATE["stats"]["truncated_bodies"] += 1
            return {
                "bodyBytes": self._bytes,
                "bodySha256": self._hash.hexdigest(),
                "truncated": True,
            }
        return b"".join(self._chunks)

    def stream(self, is_event_stream):
        """Chunk boundaries as observed byte lengths. Recorded when the
        response is a stream (SSE always; anything else only when it actually
        arrived in more than one chunk, since a single-chunk body replays
        identically without them)."""
        if not self._boundaries:
            return None
        if not is_event_stream and len(self._boundaries) < 2 and self._dropped_boundaries == 0:
            return None
        if self._dropped_boundaries > 0:
            return {"chunks": list(self._boundaries), "truncated": True}
        return {"chunks": list(self._boundaries)}


def record_exchange(trace, kind, resource, key, exchange):
    """Attach one bounded exchange to a trace. Public so an application can
    record a boundary this module does not hook."""
    try:
        trace.effect(kind, resource=resource, key=key, exchange=exchange)
        _STATE["stats"]["captured_exchanges"] += 1
    except Exception:
        # The trace may have finished or overflowed; the host request goes on.
        _STATE["stats"]["failed_captures"] += 1


def _record_http_exchange(trace, request, response):
    response_body = _bounded_body(response["body"], response["content_type"])
    response_value = dict(
        {"status": response["status"]},
        **dict(_bounded_headers(response["headers"]), **response_body),
    )
    # Stream shape (SSE / chunked): observed chunk boundaries, so the whole
    # stream is ONE logical exchange and replay can re-serve it chunk for
    # chunk. A truncated inline body already fails closed, so boundaries are
    # only kept for bodies recorded verbatim.
    stream = response.get("stream")
    if stream and response_body.get("truncated") is not True:
        response_value["stream"] = stream
    record_exchange(
        trace,
        "call",
        request["host"],
        request["method"] + " " + request["path"],
        {
            "protocol": "http",
            "request": dict(
                {"method": request["method"], "url": request["url"]},
                **dict(
                    _bounded_headers(request["headers"]),
                    **_bounded_body(request["body"], request["content_type"]),
                ),
            ),
            "response": response_value,
        },
    )


class _FakeSocket:
    """Enough of a socket for `http.client.HTTPResponse` to parse bytes we
    already hold. Returning a REAL HTTPResponse (rather than a duck type)
    keeps every downstream client working exactly as it does over a wire."""

    def __init__(self, raw):
        self._raw = raw

    def makefile(self, *args, **kwargs):
        return io.BufferedReader(io.BytesIO(self._raw))

    def close(self):
        return None


def _synthesized_response(status, reason, headers, body):
    """Build a real HTTPResponse over the given bytes."""
    lines = ["HTTP/1.1 %d %s" % (status, reason or "OK")]
    for name, value in headers:
        lowered = str(name).lower()
        if lowered in ("content-length", "transfer-encoding", "content-encoding"):
            continue
        lines.append("%s: %s" % (name, value))
    lines.append("content-length: %d" % len(body))
    raw = ("\r\n".join(lines) + "\r\n\r\n").encode("latin-1") + body
    response = http.client.HTTPResponse(_FakeSocket(raw), method="GET")
    response.begin()
    return response


def _connection_url(connection, path):
    scheme = "https" if isinstance(connection, http.client.HTTPSConnection) else "http"
    host = connection.host
    port = connection.port
    default_port = 443 if scheme == "https" else 80
    authority = host if port in (None, default_port) else "%s:%d" % (host, port)
    return "%s://%s%s" % (scheme, authority, path)


def _install_capture():
    original_request = http.client.HTTPConnection.request
    original_getresponse = http.client.HTTPConnection.getresponse

    def request(self, method, url, body=None, headers=None, **kwargs):
        try:
            if current_trace() is not None:
                raw_body = body
                if isinstance(raw_body, str):
                    raw_body = raw_body.encode("utf-8")
                if not isinstance(raw_body, (bytes, bytearray)):
                    # File-like and iterable bodies are not buffered; the
                    # exchange records the request without content rather
                    # than consuming a stream the client still needs.
                    raw_body = None
                self._reproit_probe = {
                    "method": str(method).upper(),
                    "path": url,
                    "url": _connection_url(self, url),
                    "host": self.host,
                    "headers": dict(headers or {}),
                    "body": raw_body,
                    "content_type": _header_value(headers or {}, "content-type") or "",
                }
            else:
                self._reproit_probe = None
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
            self._reproit_probe = None
        return original_request(self, method, url, body=body, headers=headers, **kwargs)

    def getresponse(self):
        response = original_getresponse(self)
        probe = getattr(self, "_reproit_probe", None)
        trace = current_trace()
        if probe is None or trace is None:
            return response
        self._reproit_probe = None
        try:
            declared = response.getheader("content-length")
            if declared is not None and int(declared) > MAX_TEE_BYTES:
                return response
        except (TypeError, ValueError):
            pass
        try:
            body = response.read()
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
            return response
        try:
            headers = list(response.getheaders())
            content_type = response.getheader("content-type") or ""
            # The stdlib hook drains the body in one read, so the observed
            # stream shape is coarse: SSE records its boundaries as observed
            # HERE (one drained chunk). Fine-grained boundaries live on the
            # httpx and aiohttp paths, which see chunks as the app consumes.
            stream = None
            if "text/event-stream" in content_type and body:
                collector = BodyCollector()
                collector.push(body)
                stream = collector.stream(True)
            _record_http_exchange(
                trace,
                probe,
                {
                    "status": response.status,
                    "headers": headers,
                    "body": body,
                    "content_type": content_type,
                    "stream": stream,
                },
            )
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
        # The real response is drained, so hand the caller an identical one.
        return _synthesized_response(response.status, response.reason, headers, body)

    http.client.HTTPConnection.request = request
    http.client.HTTPConnection.getresponse = getresponse


def _install_replay(session):
    def connect(self):
        # Hermetic: no socket is ever opened.
        self.sock = None

    def request(self, method, url, body=None, headers=None, **kwargs):
        raw_body = body
        if isinstance(raw_body, str):
            raw_body = raw_body.encode("utf-8")
        if not isinstance(raw_body, (bytes, bytearray)):
            raw_body = None
        content_type = _header_value(headers or {}, "content-type") or ""
        probe = {"method": str(method).upper(), "url": _connection_url(self, url)}
        if raw_body:
            probe["body"] = _replay.try_json(
                raw_body.decode("utf-8", errors="replace"), content_type
            )
        self._reproit_served = _replay.serve_http(session, probe)

    def getresponse(self):
        served = getattr(self, "_reproit_served", None)
        if served is None:
            served = _replay.serve_http(session, {"method": "GET", "url": "unknown"})
        self._reproit_served = None
        return _synthesized_response(
            served["status"],
            "Reproit Diverged" if served["status"] == 599 else "OK",
            list(served["headers"].items()),
            served["body_text"].encode("utf-8"),
        )

    def close(self):
        self.sock = None

    http.client.HTTPConnection.connect = connect
    http.client.HTTPConnection.request = request
    http.client.HTTPConnection.getresponse = getresponse
    http.client.HTTPConnection.close = close
    http.client.HTTPSConnection.connect = connect


def _optional_module(name):
    """A client library the host app may or may not have installed. Absence
    is not an error: the import is attempted once at install time and the
    SDK never depends on it, so the runtime stays stdlib pure."""
    try:
        return __import__(name)
    except ImportError:
        return None


def install():
    """Install the outbound hooks once, process wide. Idempotent. With
    REPROIT_REPLAY set the hooks serve the named capture instead of recording,
    and the process clock, zone, and RNG pin to the capture envelope.

    httpx and aiohttp are hooked only when the host app has them installed
    (they carry their own transports and never touch http.client); psycopg is
    wrapped explicitly via `wrap_psycopg`, like the Node reference's wrapPg."""
    if _STATE["installed"]:
        return _STATE
    replay_path = os.environ.get("REPROIT_REPLAY")
    httpx = _optional_module("httpx")
    aiohttp = _optional_module("aiohttp")
    from . import aiohttp_client, httpx_client

    if replay_path:
        session = _replay.ReplaySession.load(replay_path)
        _STATE["session"] = session
        _replay.pin_envelope(session.envelope)
        _install_replay(session)
        if httpx is not None:
            httpx_client.install_replay(httpx, session)
        if aiohttp is not None:
            aiohttp_client.install_replay(aiohttp, session)
    else:
        _install_capture()
        if httpx is not None:
            httpx_client.install_capture(httpx)
        if aiohttp is not None:
            aiohttp_client.install_capture(aiohttp)
    _STATE["httpx"] = httpx is not None
    _STATE["aiohttp"] = aiohttp is not None
    _STATE["installed"] = True
    return _STATE


def wrap_psycopg(psycopg):
    """Wrap the psycopg (v3) driver: statements and their results are recorded
    as `pg` exchanges on the ambient trace, and with REPROIT_REPLAY set the
    recorded results are served in process (psycopg.connect returns a stub,
    so no server is dialed). Accepts the real module or any module-shaped
    object exposing the same Cursor/connect surface. psycopg2 is NOT covered:
    its cursor surface differs and wrapping it is a named capability gap."""
    from . import psycopg_client

    return psycopg_client.wrap(psycopg)


def _db_effect_kind(text):
    """Reads stay reads so state oracles keep their meaning; everything else
    is a write."""
    verb = str(text or "").lstrip()[:8].upper()
    return "read" if verb.startswith("SELECT") or verb.startswith("SHOW") else "write"


def _db_outcome(result):
    if not isinstance(result, dict):
        return {"rowCount": 0}
    rows = result.get("rows")
    rows = list(rows) if isinstance(rows, (list, tuple)) else []
    outcome = {
        "command": result.get("command"),
        "rowCount": result.get("rowCount", len(rows)),
        "rows": rows[:MAX_DB_ROWS],
    }
    if len(rows) > MAX_DB_ROWS:
        outcome["truncated"] = True
    return outcome


def db_run(text, values=None, live=None):
    """The explicit database boundary. `live()` performs the real query and
    returns `{"command":..., "rowCount":..., "rows":[...]}`.

    Capture records the statement and its result on the ambient trace; replay
    serves the recorded result and NEVER calls `live`, so no database is
    reached. A statement the capture never saw raises DivergedError."""
    session = _STATE["session"]
    probe = {"text": str(text)}
    if values:
        probe["values"] = list(values)
    if session is not None:
        recorded = session.match("db", probe)
        if recorded is None:
            raise DivergedError("reproit: database call diverged from the capture")
        outcome = recorded.get("response") or {}
        error = outcome.get("error")
        if error:
            raise RuntimeError(str(error.get("message") or "recorded database error"))
        return {
            "command": outcome.get("command"),
            "rowCount": outcome.get("rowCount", 0),
            "rows": list(outcome.get("rows") or []),
        }
    if live is None:
        raise ValueError("db_run needs a live callable outside replay mode")
    trace = current_trace()
    try:
        result = live()
    except Exception as error:
        if trace is not None:
            record_exchange(
                trace,
                _db_effect_kind(text),
                "db",
                str(text)[:256],
                {
                    "protocol": "db",
                    "request": probe,
                    "response": {
                        "error": {
                            "message": str(error),
                            "code": getattr(error, "pgcode", None),
                        }
                    },
                },
            )
        raise
    if trace is not None:
        record_exchange(
            trace,
            _db_effect_kind(text),
            "db",
            str(text)[:256],
            {"protocol": "db", "request": probe, "response": _db_outcome(result)},
        )
    return result
