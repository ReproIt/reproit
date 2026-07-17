"""Runner-only urllib capture/replay using PTY-safe side files."""

import io
import json
import os
import re
import urllib.request
import urllib.parse

_SECRET = re.compile(
    r"password|passwd|secret|token|authorization|cookie|email|phone|"
    r"api[-_. ]?key|publishable[-_. ]?key|private[-_. ]?key|"
    r"access[-_. ]?key|signing[-_. ]?key",
    re.I,
)


def _redact(value):
    if isinstance(value, list):
        return [_redact(v) for v in value]
    if isinstance(value, dict):
        out = {}
        for key in sorted(value):
            child = value[key]
            if _SECRET.search(str(key)):
                out[str(key)] = (
                    "<reproit:string:length=%d>" % len(child)
                    if isinstance(child, str)
                    else "<reproit:secret>"
                )
            else:
                out[str(key)] = _redact(child)
        return out
    return value


def _canonical(raw):
    parsed = urllib.parse.urlsplit(raw)
    query = urllib.parse.urlencode(
        sorted(urllib.parse.parse_qsl(parsed.query, keep_blank_values=True))
    )
    return urllib.parse.urlunsplit(
        (parsed.scheme, parsed.netloc, parsed.path, query, parsed.fragment)
    )


class _MemoryResponse:
    def __init__(self, body, status, headers, url):
        self._body = io.BytesIO(body)
        self.status = status
        self.code = status
        self.headers = headers
        self.url = url

    def read(self, amt=-1):
        return self._body.read(amt)

    def getcode(self):
        return self.status

    def geturl(self):
        return self.url

    def close(self):
        self._body.close()

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()


def install_causal_urllib(exclude_prefix=None):
    network = os.environ.get("REPROIT_NETWORK_FILE")
    capsule_path = os.environ.get("REPROIT_CAPSULE")
    if not network and not capsule_path:
        return lambda: None
    original = urllib.request.urlopen
    capsule = None
    if capsule_path:
        try:
            with open(capsule_path, "r", encoding="utf-8") as handle:
                capsule = json.load(handle)
        except Exception:
            capsule = {"exchanges": []}
    exchanges = (capsule or {}).get("exchanges", [])
    used = set()
    prior_action = [0]
    ordinal = [0]

    def action_index():
        try:
            with open(os.environ.get("REPROIT_ACTION_FILE", ""), "r", encoding="utf-8") as handle:
                return int(handle.read().strip() or "0")
        except Exception:
            return 0

    def wrapped(request, *args, **kwargs):
        req = (
            request
            if isinstance(request, urllib.request.Request)
            else urllib.request.Request(request)
        )
        url = req.full_url
        if exclude_prefix and url.startswith(exclude_prefix):
            return original(request, *args, **kwargs)
        action = action_index()
        if action != prior_action[0]:
            prior_action[0] = action
            ordinal[0] = 0
        this_ordinal = ordinal[0]
        ordinal[0] += 1
        method = req.get_method().upper()
        actor = os.environ.get("REPROIT_DEVICE", "a")
        if capsule is not None:
            for index, exchange in enumerate(exchanges):
                if (
                    index not in used
                    and exchange.get("required")
                    and exchange.get("actor") == actor
                    and exchange.get("actionIndex") == action
                    and str(exchange.get("method", "")).upper() == method
                    and _canonical(str(exchange.get("url", ""))) == _canonical(url)
                ):
                    used.add(index)
                    body = exchange.get("responseBody", "")
                    raw = body.encode() if isinstance(body, str) else json.dumps(body).encode()
                    return _MemoryResponse(
                        raw,
                        int(exchange.get("status", 200)),
                        exchange.get("responseHeaders", {}),
                        url,
                    )
            raise RuntimeError("CAPSULE:MISS %s %s action=%d" % (method, url, action))

        response = original(request, *args, **kwargs)
        raw = response.read()
        headers = dict(response.headers.items())
        try:
            response_body = (
                _redact(json.loads(raw.decode()))
                if "json" in headers.get("Content-Type", headers.get("content-type", ""))
                else "<reproit:body:length=%d>" % len(raw)
            )
        except Exception:
            response_body = "<reproit:invalid-json>"
        request_body = None
        if req.data:
            try:
                request_body = _redact(json.loads(req.data.decode()))
            except Exception:
                request_body = "<reproit:body:length=%d>" % len(req.data)
        safe_headers = {
            k: ("<reproit:secret>" if _SECRET.search(k) else v) for k, v in req.header_items()
        }
        safe_response_headers = {
            k: ("<reproit:secret>" if _SECRET.search(k) else v) for k, v in headers.items()
        }
        exchange = {
            "id": "%s-%d-%d" % (actor, action, this_ordinal),
            "actor": actor,
            "actionIndex": action,
            "ordinal": this_ordinal,
            "protocol": urllib.parse.urlsplit(url).scheme,
            "method": method,
            "url": _canonical(url),
            "requestHeaders": safe_headers,
            "status": response.status,
            "responseHeaders": safe_response_headers,
            "responseBody": response_body,
            "required": True,
        }
        if request_body is not None:
            exchange["requestBody"] = request_body
        try:
            with open(network, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(exchange, sort_keys=True, separators=(",", ":")) + "\n")
        except Exception:
            pass
        return _MemoryResponse(raw, response.status, headers, url)

    urllib.request.urlopen = wrapped
    _merge_capabilities(capsule is not None)
    return lambda: setattr(urllib.request, "urlopen", original)


def _merge_capabilities(_replay):
    path = os.environ.get("REPROIT_CAPABILITIES_FILE")
    if not path:
        return
    try:
        with open(path, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except Exception:
        value = {}
    value["http"] = {"status": "captured", "detail": "Python urllib"}
    value["http_replay"] = {"status": "captured", "detail": "Python urllib fail-closed replay"}
    try:
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(value, handle, sort_keys=True, separators=(",", ":"))
    except Exception:
        pass
