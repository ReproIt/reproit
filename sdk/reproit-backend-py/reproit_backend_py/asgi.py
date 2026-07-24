"""ASGI middleware for FastAPI / Starlette (any ASGI 3 app).

Scan-time: inert unless the request carries `x-reproit-trace`; the finished
trace is returned as the `x-reproit-events` response header. Production: pass
a Capture and every request is traced and handed to the sampler instead.
Handlers record observed effects via `request.state.reproit` (Starlette) or
`scope["state"]["reproit"]`. Every adapter path fails closed: instrumentation
errors never reach the host app.

Bodies are buffered up to a fixed cap so the start/return events carry the
decoded JSON payloads; larger or non-JSON bodies are traced without content.
Path parameters are matched after middleware runs, so they are not part of
the canonical input here.
"""

import json
import urllib.parse

from .trace import BackendTrace, http_input, trace_context_from_headers

MAX_BODY_BYTES = 64 * 1024


def _decode_json(body, content_type, complete):
    if not complete or not body or "application/json" not in content_type:
        return None
    try:
        return json.loads(body.decode("utf-8"))
    except (ValueError, UnicodeDecodeError):
        return None


def _query_values(query_string):
    parsed = urllib.parse.parse_qs(query_string.decode("latin-1"), keep_blank_values=True)
    return {key: values[0] if len(values) == 1 else values for key, values in parsed.items()}


def _header_values(raw_headers):
    headers = {}
    for name, value in raw_headers:
        key = name.decode("latin-1").lower()
        text = value.decode("latin-1")
        if key in headers:
            prior = headers[key]
            headers[key] = prior + [text] if isinstance(prior, list) else [prior, text]
        else:
            headers[key] = text
    return headers


class ReproitMiddleware:
    """app = FastAPI(); app.add_middleware(ReproitMiddleware, capture=capture)"""

    def __init__(self, app, capture=None, operation=None, effects_complete=False):
        self.app = app
        self.capture = capture
        self.operation = operation
        self.effects_complete = bool(effects_complete)

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return
        try:
            headers = _header_values(scope.get("headers") or [])

            def first(name):
                value = headers.get(name)
                return value[0] if isinstance(value, list) else value

            scan_context = trace_context_from_headers(first)
            context = scan_context
            if context is None and self.capture is not None:
                context = self.capture.context()
            if context is None:
                await self.app(scope, receive, send)
                return

            # Buffer the request body (bounded) so the start event carries it,
            # then replay the buffered messages to the app.
            body = b""
            complete = True
            buffered = []
            while True:
                message = await receive()
                buffered.append(message)
                if message["type"] != "http.request":
                    break
                body += message.get("body", b"")
                if len(body) > MAX_BODY_BYTES:
                    complete = False
                if not message.get("more_body", False):
                    break

            operation = (
                self.operation(scope)
                if callable(self.operation)
                else scope.get("method", "GET") + " " + scope.get("path", "/")
            )
            trace = BackendTrace.begin(
                context,
                operation,
                input=http_input(
                    body=_decode_json(body, first("content-type") or "", complete),
                    query=_query_values(scope.get("query_string", b"")),
                    headers=headers,
                ),
            )
            scope.setdefault("state", {})["reproit"] = trace
        except Exception:
            # Fail closed: an instrumentation defect must not break the request.
            await self.app(scope, receive, send)
            return

        replay = list(buffered)

        async def wrapped_receive():
            if replay:
                return replay.pop(0)
            return await receive()

        # Hold the response start and body (bounded) so the return event and
        # the `x-reproit-events` header are complete before headers flush.
        held = {"start": None, "body": b"", "chunks": [], "complete": True, "done": False}

        async def release(output_known):
            if held["done"]:
                return
            held["done"] = True
            start = held["start"]
            try:
                if not trace.finished:
                    status = start["status"] if start else 500
                    content_type = ""
                    for name, value in start.get("headers") or [] if start else []:
                        if name.decode("latin-1").lower() == "content-type":
                            content_type = value.decode("latin-1")
                    output = _decode_json(
                        held["body"], content_type, held["complete"] and output_known
                    )
                    trace.finish(output, status, status < 500, self.effects_complete)
                    if scan_context is not None and start is not None:
                        start["headers"] = list(start.get("headers") or []) + [
                            (b"x-reproit-events", trace.header().encode("ascii"))
                        ]
                    elif self.capture is not None:
                        self.capture.record(trace)
            except Exception:
                # Oversized or over-long traces drop their header; ship anyway.
                pass
            if start is not None:
                await send(start)
            for chunk in held["chunks"]:
                await send(chunk)
            held["chunks"] = []

        async def wrapped_send(message):
            if held["done"]:
                await send(message)
                return
            if message["type"] == "http.response.start":
                held["start"] = dict(message)
                return
            if message["type"] == "http.response.body":
                held["chunks"].append(message)
                held["body"] += message.get("body", b"")
                if len(held["body"]) > MAX_BODY_BYTES:
                    held["complete"] = False
                if message.get("more_body", False) and held["complete"]:
                    return
                await release(output_known=True)
                return
            await release(output_known=False)
            await send(message)

        try:
            await self.app(scope, wrapped_receive, wrapped_send)
        finally:
            await release(output_known=False)
