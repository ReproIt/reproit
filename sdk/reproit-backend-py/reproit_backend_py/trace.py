"""Experimental, framework-neutral backend instrumentation.

Python port of sdk/reproit-backend-rs/src/lib.rs. Scan-time: services activate
this adapter only when a trusted request carries `x-reproit-trace`. The
resulting response header (`x-reproit-events`) contains bounded, trace-bound,
structurally redacted events. Production: the optional, config-gated capture
mode (capture.py) self-samples finished traces. It is not a public
compatibility surface while backend contracts remain experimental.

Wire parity with the Rust adapter: events serialize as compact JSON with
recursively sorted keys (serde_json's BTreeMap order), and the header is
unpadded base64url of that encoding.
"""

import base64
import hashlib
import itertools
import json
import re

MAX_EVENTS = 256
MAX_HEADER_BYTES = 60000
EFFECT_KINDS = ("read", "write", "delete", "emit", "call")

_SEQUENCE = itertools.count(1)
_PATH_SEGMENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_SECRET_PARTS = (
    "password",
    "passwd",
    "secret",
    "token",
    "authorization",
    "cookie",
    "email",
    "phone",
    "apikey",
    "publishablekey",
    "privatekey",
    "accesskey",
    "signingkey",
    "idempotencykey",
)


class TraceError(Exception):
    """Codes: InvalidOperation, AlreadyFinished, TooManyEvents, HeaderTooLarge."""

    def __init__(self, code):
        super().__init__("reproit trace rejected input: " + code)
        self.code = code


def _bounded(value, maximum):
    if not isinstance(value, str):
        return None
    value = value.strip()
    if not value or len(value) > maximum:
        return None
    return value


def trace_context_from_headers(get):
    """`get(name)` returns the request header value (or None). Returns None
    when no valid `x-reproit-trace` is present: the adapter stays inert."""
    trace_id = _bounded(get("x-reproit-trace"), 128)
    if trace_id is None:
        return None
    raw_action = get("x-reproit-action")
    action_index = 0
    if isinstance(raw_action, str):
        try:
            parsed = int(raw_action.strip())
            if 0 <= parsed <= 0xFFFFFFFF:
                action_index = parsed
        except ValueError:
            pass
    return {
        "trace_id": trace_id,
        "actor": _bounded(get("x-reproit-actor"), 32),
        "action_index": action_index,
        "build": _bounded(get("x-reproit-build"), 128),
        "config_contract": _bounded(get("x-reproit-config-contract"), 128),
    }


def _valid_path(path):
    if not isinstance(path, str) or not path:
        return False
    for segment in path.split("."):
        name = segment[:-2] if segment.endswith("[]") else segment
        if not _PATH_SEGMENT.match(name):
            return False
    return True


def selection(schema_path, response_path, type_condition=None):
    """GraphQL selection mapping (parser-produced only); None when invalid."""
    if not _valid_path(schema_path) or not _valid_path(response_path):
        return None
    value = {"schemaPath": schema_path, "responsePath": response_path}
    if type_condition is not None:
        if not _valid_path(type_condition) or "." in type_condition or "[]" in type_condition:
            return None
        value["typeCondition"] = type_condition
    return value


def http_input(body=None, path=None, query=None, headers=None):
    """Canonical decoded OpenAPI input. Framework adapters must provide decoded
    values (including lists for repeated query/header parameters), never raw
    query strings whose serialization style is ambiguous."""
    value = {}
    if body is not None:
        value["body"] = body
    for name, fields in (("path", path), ("query", query), ("headers", headers)):
        if not fields:
            continue
        if name == "headers":
            fields = {key.lower(): field for key, field in fields.items()}
        value[name] = dict(fields)
    return value


def canonical_json(value):
    """Compact JSON with recursively sorted keys: byte-identical to the Rust
    adapter's serde_json (BTreeMap) encoding of the same events."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _identity(value):
    digest = hashlib.sha256(value.encode("utf-8")).digest()
    return "sha256:" + digest[:12].hex()


def _secret_field(name):
    folded = "".join(ch for ch in name if ch.isascii() and ch.isalnum()).lower()
    return any(part in folded for part in _SECRET_PARTS)


def redact(value):
    """Recursive structural redaction: secret-named fields become `$reproit`
    metadata stubs (type + length), everything else recurses."""
    if isinstance(value, dict):
        return {
            key: _metadata(field) if _secret_field(str(key)) else redact(field)
            for key, field in value.items()
        }
    if isinstance(value, (list, tuple)):
        return [redact(item) for item in value]
    return value


def _metadata(value):
    kind, length = "null", None
    if isinstance(value, bool):
        kind = "boolean"
    elif isinstance(value, int):
        kind = "integer"
    elif isinstance(value, float):
        kind = "number"
    elif isinstance(value, str):
        kind, length = "string", len(value)
    elif isinstance(value, (list, tuple)):
        kind, length = "array", len(value)
    elif isinstance(value, dict):
        kind = "object"
    return {"$reproit": {"redacted": True, "type": kind, "length": length}}


class BackendTrace:
    """One traced operation: a start event, observed effects, one return."""

    def __init__(self, common):
        self._common = common
        self._events = []
        self.finished = False

    @classmethod
    def begin(
        cls,
        context,
        operation,
        span_id=None,
        tenant=None,
        idempotency_key=None,
        input=None,
        selections=None,
    ):
        name = _bounded(str(operation), 256)
        if name is None:
            raise TraceError("InvalidOperation")
        span = _bounded(str(span_id or context["trace_id"] + ":" + name), 128)
        if span is None:
            raise TraceError("InvalidOperation")
        common = {
            "traceId": context["trace_id"],
            "spanId": span,
            "actionIndex": context["action_index"],
            "operation": name,
        }
        if context.get("actor"):
            common["actor"] = context["actor"]
        if context.get("build"):
            common["build"] = context["build"]
        if context.get("config_contract"):
            common["configContract"] = context["config_contract"]
        bounded_tenant = _bounded(str(tenant), 128) if tenant is not None else None
        if bounded_tenant is not None:
            common["tenant"] = bounded_tenant
        if idempotency_key is not None:
            common["idempotencyKey"] = _identity(str(idempotency_key))
        if selections:
            common["selections"] = list(selections)[:MAX_EVENTS]
        trace = cls(common)
        trace._push("start", {"input": redact(input)})
        return trace

    def effect(self, kind, resource=None, key=None, tenant=None, event=None, detail=None):
        if self.finished:
            raise TraceError("AlreadyFinished")
        if kind not in EFFECT_KINDS:
            raise TraceError("InvalidOperation")
        fields = {"effect": kind}
        for name, value in (
            ("resource", resource),
            ("key", key),
            ("effectTenant", tenant),
            ("event", event),
        ):
            if value is not None:
                fields[name] = str(value)[:256]
        if detail is not None:
            redacted = redact(detail)
            if isinstance(redacted, dict):
                for field in ("before", "after", "payload"):
                    if field in redacted:
                        fields[field] = redacted[field]
        self._push("effect", fields)

    def finish(self, output, status, success, effects_complete):
        if self.finished:
            raise TraceError("AlreadyFinished")
        self._push(
            "return",
            {
                "output": redact(output),
                "status": status,
                "success": bool(success),
                "effectsComplete": bool(effects_complete),
            },
        )
        self.finished = True

    def header(self):
        if not self.finished:
            raise TraceError("AlreadyFinished")
        raw = canonical_json(self._events).encode("utf-8")
        encoded = base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")
        if len(encoded) > MAX_HEADER_BYTES:
            raise TraceError("HeaderTooLarge")
        return encoded

    def events(self):
        return self._events

    def _push(self, kind, fields):
        if len(self._events) >= MAX_EVENTS:
            raise TraceError("TooManyEvents")
        event = dict(self._common)
        event["sequence"] = next(_SEQUENCE)
        event["kind"] = kind
        event.update(fields)
        self._events.append(event)
