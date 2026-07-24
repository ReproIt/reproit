"""Semantics parity tests against sdk/reproit-backend-rs/src/lib.rs."""

import base64
import json

import pytest

from reproit_backend_py import (
    MAX_EVENTS,
    MAX_HEADER_BYTES,
    BackendTrace,
    TraceError,
    http_input,
    selection,
    trace_context_from_headers,
)


def _context(**overrides):
    context = {
        "trace_id": "trace-a",
        "actor": None,
        "action_index": 0,
        "build": None,
        "config_contract": None,
    }
    context.update(overrides)
    return context


def _decode_header(header):
    padded = header + "=" * (-len(header) % 4)
    return json.loads(base64.urlsafe_b64decode(padded).decode("utf-8"))


def test_emits_bounded_correlated_redacted_events():
    headers = {
        "x-reproit-trace": "trace-a",
        "x-reproit-actor": "alice",
        "x-reproit-action": "7",
        "x-reproit-build": "build-a",
        "x-reproit-config-contract": "contract-a",
    }
    context = trace_context_from_headers(headers.get)
    trace = BackendTrace.begin(
        context,
        "createProject",
        tenant="org-1",
        idempotency_key="retry-secret",
        input={"name": "demo", "password": "abcdefgh"},
        selections=[selection("project.id", "projectId")],
    )
    trace.effect("write", resource="projects", key="1", tenant="org-1")
    trace.finish(
        {
            "id": 1,
            "apiKey": "sk_live_secret",
            "publishable_key": "pk_live_secret",
            "private-key": "private-secret",
            "access key": "access-secret",
            "signingKey": "signing-secret",
            "monkey": "harmless",
        },
        201,
        True,
        True,
    )
    assert len(trace.header()) < MAX_HEADER_BYTES
    events = trace.events()
    assert events[0]["actionIndex"] == 7
    assert events[0]["build"] == "build-a"
    assert events[0]["configContract"] == "contract-a"
    assert events[0]["input"]["password"]["$reproit"]["length"] == 8
    assert events[0]["idempotencyKey"] != "retry-secret"
    assert events[0]["idempotencyKey"].startswith("sha256:")
    for field in ("apiKey", "publishable_key", "private-key", "access key", "signingKey"):
        assert events[2]["output"][field]["$reproit"]["redacted"] is True
    assert events[2]["output"]["monkey"] == "harmless"
    assert events[2]["effectsComplete"] is True


def test_stays_inactive_without_a_trace_header():
    assert trace_context_from_headers(lambda name: None) is None
    assert trace_context_from_headers({"x-reproit-trace": "  "}.get) is None


def test_header_is_unpadded_base64url_of_canonical_json():
    trace = BackendTrace.begin(_context(), "op", input={"b": 1, "a": 2})
    trace.finish({"ok": True}, 200, True, True)
    header = trace.header()
    assert "=" not in header and "+" not in header and "/" not in header
    decoded = _decode_header(header)
    assert decoded == json.loads(json.dumps(trace.events()))
    raw = base64.urlsafe_b64decode(header + "=" * (-len(header) % 4)).decode("utf-8")
    assert raw.index('"a":2') < raw.index('"b":1')


def test_one_return_and_no_effects_after_return():
    trace = BackendTrace.begin(_context(), "op")
    trace.finish(None, 200, True, False)
    with pytest.raises(TraceError) as error:
        trace.effect("read")
    assert error.value.code == "AlreadyFinished"
    with pytest.raises(TraceError):
        trace.finish(None, 200, True, False)


def test_header_bounds():
    trace = BackendTrace.begin(_context(), "op")
    with pytest.raises(TraceError) as unfinished:
        trace.header()
    assert unfinished.value.code == "AlreadyFinished"
    big = BackendTrace.begin(_context(), "op")
    big.finish({"blob": "x" * MAX_HEADER_BYTES}, 200, True, True)
    with pytest.raises(TraceError) as oversized:
        big.header()
    assert oversized.value.code == "HeaderTooLarge"


def test_event_count_is_capped():
    trace = BackendTrace.begin(_context(), "op")
    for _ in range(MAX_EVENTS - 1):
        trace.effect("emit", event="tick")
    with pytest.raises(TraceError) as error:
        trace.effect("emit")
    assert error.value.code == "TooManyEvents"


def test_typed_effects_and_bounded_identifiers():
    trace = BackendTrace.begin(_context(), "op")
    with pytest.raises(TraceError):
        trace.effect("mutate")
    with pytest.raises(TraceError):
        BackendTrace.begin(_context(), "")
    with pytest.raises(TraceError):
        BackendTrace.begin(_context(), "x" * 257)


def test_effect_detail_keeps_only_before_after_payload():
    trace = BackendTrace.begin(_context(), "op")
    trace.effect(
        "write",
        resource="users",
        detail={"before": {"email": "a@b.c"}, "after": {"name": "z"}, "extra": "dropped"},
    )
    effect = trace.events()[1]
    assert effect["before"]["email"]["$reproit"]["redacted"] is True
    assert effect["after"]["name"] == "z"
    assert "extra" not in effect


def test_canonical_http_input():
    value = http_input(
        body={"name": "demo"},
        path={"project": "p1"},
        query={"tag": ["a", "b"]},
        headers={"X-Mode": "safe"},
    )
    assert value["headers"]["x-mode"] == "safe"
    assert value["query"]["tag"] == ["a", "b"]
    assert http_input(path={}, query={}, headers={}) == {}


def test_selections_validate_their_paths():
    assert selection("project.id", "projectId") is not None
    assert selection("items[].id", "rows[].id", "Widget") is not None
    assert selection("1bad", "ok") is None
    assert selection("ok", "ok", "Bad.Condition") is None
