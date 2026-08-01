"""httpx hooks for outbound-exchange capture and hermetic replay.

httpx never touches `http.client`; both its sync and async clients funnel
through one transport boundary, which is where these hooks live. Capture TEES
the response byte stream instead of draining it: the app consumes the live
stream incrementally (the LLM SSE shape survives), the observed chunk
boundaries are recorded, and the exchange lands at the moment the app sees
the end of the body, exactly where the stdlib wrapper records. A body the app
never reads records nothing, like an abandoned fetch stream in Node.

Replay serves recorded exchanges in process: no connection is made, an
unmatched call gets the same hard 599 the stdlib path serves, and a recorded
stream shape is re-served chunk for chunk.

This module is imported lazily by instrument.install() and only when the
host app has httpx installed; the SDK itself never depends on httpx.
"""

from . import replay as _replay
from .instrument import (
    MAX_TEE_BYTES,
    BodyCollector,
    _STATE,
    _record_http_exchange,
)
from .trace import current_trace


def _probe(request):
    """The live probe for one httpx request, in the shape the matcher wants."""
    content_type = request.headers.get("content-type") or ""
    probe = {"method": str(request.method).upper(), "url": str(request.url)}
    try:
        raw = request.content
    except Exception:
        raw = None
    if raw:
        probe["body"] = _replay.try_json(raw.decode("utf-8", errors="replace"), content_type)
    return probe


def _oversized(response):
    try:
        declared = response.headers.get("content-length")
        return declared is not None and int(declared) > MAX_TEE_BYTES
    except (TypeError, ValueError):
        return False


def _record(trace, request, response, collector):
    body = collector.result()
    content_type = response.headers.get("content-type") or ""
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
            "content_type": content_type,
            "stream": collector.stream("text/event-stream" in content_type),
        },
    )


def install_capture(httpx):
    """Record httpx exchanges at the transport boundary by teeing the
    response stream: chunks reach the app live and are recorded at end."""
    original_sync = httpx.HTTPTransport.handle_request
    original_async = httpx.AsyncHTTPTransport.handle_async_request

    class TeeSyncStream(httpx.SyncByteStream):
        def __init__(self, inner, record_once):
            self._inner = inner
            self._record_once = record_once

        def __iter__(self):
            collector = BodyCollector()
            for chunk in self._inner:
                try:
                    collector.push(chunk)
                except Exception:
                    _STATE["stats"]["failed_captures"] += 1
                yield chunk
            self._record_once(collector)

        def close(self):
            close = getattr(self._inner, "close", None)
            if close is not None:
                close()

    class TeeAsyncStream(httpx.AsyncByteStream):
        def __init__(self, inner, record_once):
            self._inner = inner
            self._record_once = record_once

        async def __aiter__(self):
            collector = BodyCollector()
            async for chunk in self._inner:
                try:
                    collector.push(chunk)
                except Exception:
                    _STATE["stats"]["failed_captures"] += 1
                yield chunk
            self._record_once(collector)

        async def aclose(self):
            aclose = getattr(self._inner, "aclose", None)
            if aclose is not None:
                await aclose()

    def teed(response, request, trace, stream_class):
        recorded = {"done": False}

        def record_once(collector):
            if recorded["done"]:
                return
            recorded["done"] = True
            try:
                _record(trace, request, response, collector)
            except Exception:
                _STATE["stats"]["failed_captures"] += 1

        return httpx.Response(
            status_code=response.status_code,
            headers=response.headers,
            stream=stream_class(response.stream, record_once),
            request=request,
            extensions=response.extensions,
        )

    def handle_request(self, request):
        response = original_sync(self, request)
        trace = current_trace()
        if trace is None or _oversized(response):
            return response
        try:
            return teed(response, request, trace, TeeSyncStream)
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
            return response

    async def handle_async_request(self, request):
        response = await original_async(self, request)
        trace = current_trace()
        if trace is None or _oversized(response):
            return response
        try:
            return teed(response, request, trace, TeeAsyncStream)
        except Exception:
            _STATE["stats"]["failed_captures"] += 1
            return response

    httpx.HTTPTransport.handle_request = handle_request
    httpx.AsyncHTTPTransport.handle_async_request = handle_async_request


def install_replay(httpx, session):
    """Serve recorded exchanges to httpx in process. A recorded stream shape
    is re-served chunk for chunk so consumers reading the stream observe the
    recorded boundaries."""

    class ReplaySyncStream(httpx.SyncByteStream):
        def __init__(self, chunks):
            self._chunks = chunks

        def __iter__(self):
            for chunk in self._chunks:
                yield chunk

        def close(self):
            return None

    class ReplayAsyncStream(httpx.AsyncByteStream):
        def __init__(self, chunks):
            self._chunks = chunks

        async def __aiter__(self):
            for chunk in self._chunks:
                yield chunk

        async def aclose(self):
            return None

    def served_response(request, stream_class):
        served = _replay.serve_http(session, _probe(request))
        chunks = served.get("chunks")
        if chunks is None:
            chunks = [served["body_text"].encode("utf-8")]
        return httpx.Response(
            status_code=served["status"],
            headers=served["headers"],
            stream=stream_class(chunks),
            request=request,
        )

    def handle_request(self, request):
        return served_response(request, ReplaySyncStream)

    async def handle_async_request(self, request):
        return served_response(request, ReplayAsyncStream)

    httpx.HTTPTransport.handle_request = handle_request
    httpx.AsyncHTTPTransport.handle_async_request = handle_async_request
