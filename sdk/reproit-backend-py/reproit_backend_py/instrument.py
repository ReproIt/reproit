"""Outbound-exchange capture and hermetic replay for reproit-backend-py.

`install()` patches `http.client.HTTPConnection`, the single stdlib chokepoint
every mainstream Python HTTP client funnels through (`urllib.request`,
`requests`, and `urllib3`), so any dependency call made while a request trace
is ambient is recorded on that trace as an `effect` event carrying an
`exchange`: the request the app sent and the response the dependency
returned. Databases have no equivalent stdlib chokepoint and this SDK takes
no driver dependency, so `db_run(text, values, live)` is the explicit
boundary, the same decision the Rust adapter made for the same reason.

`httpx` does NOT go through `http.client`: it carries its own `httpcore`
transport, so the stdlib hook cannot see it. `install()` therefore also hooks
`httpx.HTTPTransport.handle_request` and
`httpx.AsyncHTTPTransport.handle_async_request` when httpx is importable.
This is an OPTIONAL path: the import is attempted at install time and its
absence is not an error, so the SDK keeps its zero-dependency runtime.

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
    # True when httpx was importable at install time and its transport is
    # hooked; False means the host app has no httpx and none was needed.
    "httpx": False,
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
            "response": dict(
                {"status": response["status"]},
                **dict(
                    _bounded_headers(response["headers"]),
                    **_bounded_body(response["body"], response["content_type"]),
                ),
            ),
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
            _record_http_exchange(
                trace,
                probe,
                {
                    "status": response.status,
                    "headers": headers,
                    "body": body,
                    "content_type": response.getheader("content-type") or "",
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


def _httpx_module():
    """The httpx module when the host app has it installed, else None. The
    import is attempted once at install time so the SDK never depends on it."""
    try:
        import httpx
    except ImportError:
        return None
    return httpx


def _httpx_probe(request):
    """The live probe for one httpx request, in the shape the matcher wants."""
    content_type = request.headers.get("content-type") or ""
    probe = {"method": str(request.method).upper(), "url": str(request.url)}
    try:
        raw = request.content
    except Exception:
        raw = None
    if raw:
        probe["body"] = _replay.try_json(
            raw.decode("utf-8", errors="replace"), content_type
        )
    return probe


def _install_httpx_capture(httpx):
    """Record httpx exchanges at the transport boundary, the one place both
    the sync and async clients funnel through."""
    original_sync = httpx.HTTPTransport.handle_request
    original_async = httpx.AsyncHTTPTransport.handle_async_request

    def rebuilt(response, request, body):
        # The real response is drained by read(), so hand the caller an
        # identical one built over the bytes we hold.
        return httpx.Response(
            status_code=response.status_code,
            headers=response.headers,
            content=body,
            request=request,
            extensions=response.extensions,
        )

    def record(request, response, body):
        trace = current_trace()
        if trace is None:
            return
        try:
            raw_request = request.content
        except Exception:
            raw_request = None
        _record_http_exchange(
            trace,
            {
                "method": str(request.method).upper(),
                "path": request.url.raw_path.decode("ascii", errors="replace"),
                "url": str(request.url),
                "host": request.url.host,
                "headers": dict(request.headers),
                "body": raw_request,
                "content_type": request.headers.get("content-type") or "",
            },
            {
                "status": response.status_code,
                "headers": list(response.headers.items()),
                "body": body,
                "content_type": response.headers.get("content-type") or "",
            },
        )

    def oversized(response):
        try:
            declared = response.headers.get("content-length")
            return declared is not None and int(declared) > MAX_TEE_BYTES
        except (TypeError, ValueError):
            return False

    def handle_request(self, request):
        response = original_sync(self, request)
        if current_trace() is None or oversized(response):
            return response
        try:
            body = response.read()
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
            return response
        try:
            record(request, response, body)
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
        return rebuilt(response, request, body)

    async def handle_async_request(self, request):
        response = await original_async(self, request)
        if current_trace() is None or oversized(response):
            return response
        try:
            body = await response.aread()
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
            return response
        try:
            record(request, response, body)
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
        return rebuilt(response, request, body)

    httpx.HTTPTransport.handle_request = handle_request
    httpx.AsyncHTTPTransport.handle_async_request = handle_async_request


def _install_httpx_replay(httpx, session):
    """Serve recorded exchanges to httpx in process: no connection is made,
    and an unmatched call gets the same hard 599 the stdlib path serves."""

    def served_response(request):
        served = _replay.serve_http(session, _httpx_probe(request))
        return httpx.Response(
            status_code=served["status"],
            headers=served["headers"],
            content=served["body_text"].encode("utf-8"),
            request=request,
        )

    def handle_request(self, request):
        return served_response(request)

    async def handle_async_request(self, request):
        return served_response(request)

    httpx.HTTPTransport.handle_request = handle_request
    httpx.AsyncHTTPTransport.handle_async_request = handle_async_request


def install():
    """Install the outbound hooks once, process wide. Idempotent. With
    REPROIT_REPLAY set the hooks serve the named capture instead of recording,
    and the process clock zone and RNG pin to the capture envelope.

    httpx is hooked only when the host app has it installed; its absence is
    not an error and leaves the runtime stdlib pure."""
    if _STATE["installed"]:
        return _STATE
    replay_path = os.environ.get("REPROIT_REPLAY")
    httpx = _httpx_module()
    if replay_path:
        session = _replay.ReplaySession.load(replay_path)
        _STATE["session"] = session
        _replay.pin_envelope(session.envelope)
        _install_replay(session)
        if httpx is not None:
            _install_httpx_replay(httpx, session)
    else:
        _install_capture()
        if httpx is not None:
            _install_httpx_capture(httpx)
    _STATE["httpx"] = httpx is not None
    _STATE["installed"] = True
    return _STATE


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
