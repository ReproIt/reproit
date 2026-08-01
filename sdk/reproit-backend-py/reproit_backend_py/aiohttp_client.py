"""aiohttp hooks for outbound-exchange capture and hermetic replay.

aiohttp carries its own connector and never touches `http.client`, so the
stdlib hook cannot see it. Every convenience method (`session.get`, `post`,
`request`) funnels through `ClientSession._request`, which is where these
hooks live. Capture TEES the response's content stream: the app consumes the
live stream incrementally (the LLM SSE shape survives), observed chunk
boundaries are recorded, and the exchange lands when the app reaches the end
of the body. A body the app never reads records nothing.

Replay patches `_request` to return an in-process stand-in served from the
recorded exchanges: no connector, no DNS, no socket. The stand-in covers the
read surface applications actually use (`read`, `text`, `json`, `content`
iteration, context-manager use, `release`, `raise_for_status`); exotic
surfaces (connection object, trailers) are deliberately absent so an app
depending on them fails loudly instead of half-replaying.

This module is imported lazily by instrument.install() and only when the
host app has aiohttp installed; the SDK itself never depends on aiohttp.
"""

import json

from . import replay as _replay
from .instrument import (
    MAX_TEE_BYTES,
    BodyCollector,
    _STATE,
    _record_http_exchange,
)
from .trace import current_trace


class _TeeContent:
    """Wrap an aiohttp StreamReader: every byte handed to the app is pushed
    into the collector, and the exchange records once at EOF. Only the read
    surface is wrapped; attribute access falls through to the real reader."""

    def __init__(self, inner, collector, record_once):
        self._inner = inner
        self._collector = collector
        self._record_once = record_once

    def _push(self, chunk):
        try:
            if chunk:
                self._collector.push(chunk)
            if self._inner.at_eof():
                self._record_once(self._collector)
        except Exception:
            _STATE["stats"]["failed_captures"] += 1

    async def read(self, n=-1):
        chunk = await self._inner.read(n)
        self._push(chunk)
        return chunk

    async def readany(self):
        chunk = await self._inner.readany()
        self._push(chunk)
        return chunk

    async def readexactly(self, n):
        chunk = await self._inner.readexactly(n)
        self._push(chunk)
        return chunk

    async def readline(self):
        chunk = await self._inner.readline()
        self._push(chunk)
        return chunk

    def at_eof(self):
        return self._inner.at_eof()

    def __aiter__(self):
        return self._iterate()

    async def _iterate(self):
        async for chunk in self._inner:
            self._push(chunk)
            yield chunk
        self._push(b"")

    def iter_any(self):
        return self._iter_calls(self._inner.iter_any())

    def iter_chunked(self, n):
        return self._iter_calls(self._inner.iter_chunked(n))

    async def _iter_calls(self, iterator):
        async for chunk in iterator:
            self._push(chunk)
            yield chunk
        self._push(b"")

    def __getattr__(self, name):
        return getattr(self._inner, name)


def _request_meta(response, body):
    info = response.request_info
    url = info.url
    headers = dict(info.headers)
    return {
        "method": str(info.method).upper(),
        "path": url.path_qs,
        "url": str(url),
        "host": url.host,
        "headers": headers,
        "body": body,
        "content_type": _lower_get(headers, "content-type"),
    }


def _lower_get(headers, name):
    for key, value in headers.items():
        if str(key).lower() == name:
            return str(value)
    return ""


def _outbound_body(kwargs):
    """The request body when it is recordable: explicit bytes/str data or the
    json= convenience. Streams and form objects pass through unrecorded."""
    if kwargs.get("json") is not None:
        return json.dumps(kwargs["json"], separators=(",", ":")).encode("utf-8")
    data = kwargs.get("data")
    if isinstance(data, (bytes, bytearray)):
        return bytes(data)
    if isinstance(data, str):
        return data.encode("utf-8")
    return None


def _oversized(response):
    try:
        declared = response.headers.get("Content-Length")
        return declared is not None and int(declared) > MAX_TEE_BYTES
    except (TypeError, ValueError):
        return False


def install_capture(aiohttp):
    original = aiohttp.ClientSession._request

    async def _request(self, method, str_or_url, **kwargs):
        response = await original(self, method, str_or_url, **kwargs)
        trace = current_trace()
        if trace is None or _oversized(response):
            return response
        try:
            request_body = _outbound_body(kwargs)
            content_type = response.headers.get("Content-Type") or ""
            recorded = {"done": False}

            def record_once(collector):
                if recorded["done"]:
                    return
                recorded["done"] = True
                try:
                    _record_http_exchange(
                        trace,
                        _request_meta(response, request_body),
                        {
                            "status": response.status,
                            "headers": list(response.headers.items()),
                            "body": collector.result(),
                            "content_type": content_type,
                            "stream": collector.stream("text/event-stream" in content_type),
                        },
                    )
                except Exception:
                    _STATE["stats"]["failed_captures"] += 1

            response.content = _TeeContent(response.content, BodyCollector(), record_once)
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
        return response

    aiohttp.ClientSession._request = _request


class _ReplayContent:
    """The recorded stream, re-served chunk for chunk."""

    def __init__(self, chunks):
        self._chunks = list(chunks)
        self._index = 0
        self._buffer = b""

    def at_eof(self):
        return not self._buffer and self._index >= len(self._chunks)

    async def read(self, n=-1):
        if n is None or n < 0:
            rest = self._buffer + b"".join(self._chunks[self._index :])
            self._buffer = b""
            self._index = len(self._chunks)
            return rest
        if not self._buffer and self._index < len(self._chunks):
            self._buffer = self._chunks[self._index]
            self._index += 1
        taken, self._buffer = self._buffer[:n], self._buffer[n:]
        return taken

    async def readany(self):
        if self._buffer:
            taken, self._buffer = self._buffer, b""
            return taken
        if self._index >= len(self._chunks):
            return b""
        chunk = self._chunks[self._index]
        self._index += 1
        return chunk

    def __aiter__(self):
        return self._iterate()

    async def _iterate(self):
        while not self.at_eof():
            yield await self.readany()

    def iter_any(self):
        return self._iterate()

    def iter_chunked(self, n):
        return self._iterate()


class _ReplayResponse:
    """Duck-typed ClientResponse over one served exchange. Deliberately
    minimal: the read surface plus lifecycle, nothing that would pretend a
    live connection exists."""

    def __init__(self, method, url, served):
        self.method = method
        self.url = url
        self.status = served["status"]
        self.reason = "Reproit Diverged" if served["status"] == 599 else "OK"
        self.headers = dict(served["headers"])
        chunks = served.get("chunks")
        self._body = served["body_text"].encode("utf-8")
        self.content = _ReplayContent(chunks if chunks is not None else [self._body])

    @property
    def ok(self):
        return self.status < 400

    async def read(self):
        return self._body

    async def text(self, encoding="utf-8"):
        return self._body.decode(encoding, errors="replace")

    async def json(self, **kwargs):
        return json.loads(self._body.decode("utf-8"))

    def raise_for_status(self):
        if self.status >= 400:
            raise RuntimeError("replayed response status %d" % self.status)

    def release(self):
        return None

    def close(self):
        return None

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        self.release()


def install_replay(aiohttp, session):
    async def _request(self, method, str_or_url, **kwargs):
        content_type = _lower_get(dict(kwargs.get("headers") or {}), "content-type")
        if kwargs.get("json") is not None:
            content_type = content_type or "application/json"
        probe = {"method": str(method).upper(), "url": str(str_or_url)}
        body = _outbound_body(kwargs)
        if body:
            probe["body"] = _replay.try_json(
                body.decode("utf-8", errors="replace"), content_type
            )
        served = _replay.serve_http(session, probe)
        return _ReplayResponse(str(method).upper(), str(str_or_url), served)

    aiohttp.ClientSession._request = _request
