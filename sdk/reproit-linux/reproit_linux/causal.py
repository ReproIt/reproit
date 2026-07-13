"""Fail-closed causal capture/replay for Python's process-wide urllib client."""

import io
import json
import os
import re
import threading
import urllib.parse
import urllib.request

_SECRET = re.compile(
    r"password|passwd|secret|token|authorization|cookie|email|phone|"
    r"api[-_. ]?key|publishable[-_. ]?key|private[-_. ]?key|"
    r"access[-_. ]?key|signing[-_. ]?key",
    re.I,
)
_LOCK = threading.RLock()


def _redact(value):
    if isinstance(value, list):
        return [_redact(item) for item in value]
    if isinstance(value, dict):
        return {
            str(key): (
                "<reproit:string:length=%d>" % len(child)
                if _SECRET.search(str(key)) and isinstance(child, str)
                else "<reproit:secret>"
                if _SECRET.search(str(key))
                else _redact(child)
            )
            for key, child in sorted(value.items(), key=lambda item: str(item[0]))
        }
    return value


def _url(raw):
    parsed = urllib.parse.urlsplit(raw)
    query = urllib.parse.urlencode(
        sorted(urllib.parse.parse_qsl(parsed.query, keep_blank_values=True))
    )
    return urllib.parse.urlunsplit(
        (parsed.scheme.lower(), parsed.netloc.lower(), parsed.path, query, parsed.fragment)
    )


def _body(raw, headers):
    if not raw:
        return None
    content_type = next(
        (value for key, value in headers.items() if key.lower() == "content-type"), ""
    )
    if "json" in content_type.lower():
        try:
            return _redact(json.loads(raw.decode("utf-8")))
        except Exception:
            return "<reproit:invalid-json>"
    return "<reproit:body:length=%d>" % len(raw)


class _Response:
    def __init__(self, raw, status, headers, url):
        self._stream = io.BytesIO(raw)
        self.status = status
        self.code = status
        self.headers = headers
        self.url = url

    def read(self, amt=-1):
        return self._stream.read(amt)

    def getcode(self):
        return self.status

    def geturl(self):
        return self.url

    def close(self):
        self._stream.close()

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()


def _action_index():
    try:
        with open(os.environ.get("REPROIT_ACTION_FILE", ""), encoding="utf-8") as handle:
            return int(handle.read().strip() or "0")
    except Exception:
        return 0


def _write_capabilities(path):
    if not path:
        return
    try:
        with open(path, encoding="utf-8") as handle:
            value = json.load(handle)
    except Exception:
        value = {}
    value["http"] = {"status": "captured", "detail": "Python urllib process hook"}
    value["http_replay"] = {
        "status": "captured",
        "detail": "Python urllib fail-closed replay",
    }
    try:
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(value, handle, sort_keys=True, separators=(",", ":"))
    except Exception:
        pass


def install_causal_urllib(exclude_prefix=None):
    """Install once during a Reproit run and return an idempotent restore callback."""
    network = os.environ.get("REPROIT_NETWORK_FILE")
    capsule_path = os.environ.get("REPROIT_CAPSULE")
    if not network and not capsule_path:
        return lambda: None
    original = urllib.request.urlopen
    capsule = None
    if capsule_path:
        try:
            with open(capsule_path, encoding="utf-8") as handle:
                capsule = json.load(handle)
        except Exception:
            capsule = {"exchanges": []}
    exchanges = (capsule or {}).get("exchanges", [])
    used = set()
    state = {"action": -1, "ordinal": 0}

    def wrapped(value, *args, **kwargs):
        request = value if isinstance(value, urllib.request.Request) else urllib.request.Request(value)
        url = request.full_url
        if exclude_prefix and url.startswith(exclude_prefix):
            return original(value, *args, **kwargs)
        action = _action_index()
        with _LOCK:
            if state["action"] != action:
                state.update(action=action, ordinal=0)
            ordinal = state["ordinal"]
            state["ordinal"] += 1
        method = request.get_method().upper()
        actor = os.environ.get("REPROIT_DEVICE", "a")
        if capsule is not None:
            with _LOCK:
                match = next(
                    (
                        (index, exchange)
                        for index, exchange in enumerate(exchanges)
                        if index not in used
                        and exchange.get("required")
                        and exchange.get("actor") == actor
                        and exchange.get("actionIndex", exchange.get("action_index")) == action
                        and str(exchange.get("method", "")).upper() == method
                        and _url(str(exchange.get("url", ""))) == _url(url)
                    ),
                    None,
                )
                if match:
                    used.add(match[0])
            if not match:
                raise RuntimeError("CAPSULE:MISS %s %s action=%d" % (method, url, action))
            exchange = match[1]
            body = exchange.get("responseBody", exchange.get("response_body", ""))
            raw = body.encode("utf-8") if isinstance(body, str) else json.dumps(body).encode("utf-8")
            return _Response(raw, int(exchange.get("status", 200)), exchange.get("responseHeaders", exchange.get("response_headers", {})), url)

        response = original(value, *args, **kwargs)
        raw = response.read()
        response_headers = dict(response.headers.items())
        request_headers = dict(request.header_items())
        safe = lambda headers: {
            key: "<reproit:secret>" if _SECRET.search(key) else val
            for key, val in headers.items()
        }
        exchange = {
            "id": "%s-%d-%d" % (actor, action, ordinal),
            "actor": actor,
            "actionIndex": action,
            "ordinal": ordinal,
            "protocol": urllib.parse.urlsplit(url).scheme,
            "method": method,
            "url": url,
            "requestHeaders": safe(request_headers),
            "requestBody": _body(request.data, request_headers),
            "status": response.status,
            "responseHeaders": safe(response_headers),
            "responseBody": _body(raw, response_headers),
            "required": True,
        }
        try:
            with open(network, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(exchange, sort_keys=True, separators=(",", ":")) + "\n")
        except Exception:
            pass
        return _Response(raw, response.status, response_headers, url)

    urllib.request.urlopen = wrapped
    _write_capabilities(os.environ.get("REPROIT_CAPABILITIES_FILE"))
    return lambda: setattr(urllib.request, "urlopen", original)
