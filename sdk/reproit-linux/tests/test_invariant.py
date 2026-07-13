#!/usr/bin/env python3
"""Dogfood the app-invariant oracle for the Linux SDK, both directions.

No display or a11y bus is needed: record_snapshot signs a hand-built Node tree,
which is exactly the path observe() funnels through, so the invariant evaluation
+ REPROIT_INVARIANT marker emission is exercised end to end. The AT-SPI runner
(crates/reproit/src/backends/atspi.rs) scrapes that marker off the child's stderr
and re-emits it as EXPLORE:INVARIANT.

Run:
    python3 sdk/reproit-linux/tests/test_invariant.py
"""

import contextlib
import io
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SDK_ROOT = os.path.dirname(HERE)
sys.path.insert(0, SDK_ROOT)

import reproit_linux  # noqa: E402
from reproit_linux.signature import Node  # noqa: E402


def _fresh_sdk():
    # A fresh instance (not the module singleton) so tests do not couple. on_event
    # swallows the telemetry batch so only our marker reaches the captured stderr.
    sdk = reproit_linux._ReproIt()
    sdk.init(app_id="t", on_event=lambda ev: None)
    return sdk


def _marker_lines(text):
    return [
        json.loads(ln[len("REPROIT_INVARIANT ") :])
        for ln in text.splitlines()
        if ln.startswith("REPROIT_INVARIANT ")
    ]


def _raise():
    raise ValueError("kaboom")


def test_violations_reported_under_the_fuzzer():
    os.environ["REPROIT_UNDER_FUZZER"] = "1"
    sdk = _fresh_sdk()
    sdk.invariant("holds", lambda: True)
    sdk.invariant("neg-balance", lambda: {"ok": False, "message": "balance < 0"})
    sdk.invariant("falsy", lambda: False)
    sdk.invariant("raises", _raise)
    cap = io.StringIO()
    with contextlib.redirect_stderr(cap):
        sig = sdk.record_snapshot(Node("window"), action="tap")
    markers = _marker_lines(cap.getvalue())
    assert len(markers) == 1, "one marker line for the settle: %r" % markers
    m = markers[0]
    assert m["sig"] == sig, "the marker carries the SDK's own sig"
    ids = {it["id"]: it["message"] for it in m["items"]}
    # Only the three violations; the holding invariant never appears.
    assert set(ids) == {"neg-balance", "falsy", "raises"}, ids
    assert ids["neg-balance"] == "balance < 0"
    assert ids["falsy"] == ""  # a bare falsy is a violation with an empty message
    assert ids["raises"] == "kaboom"
    print("PASS violations_reported_under_the_fuzzer")


def test_clean_state_is_silent():
    os.environ["REPROIT_UNDER_FUZZER"] = "1"
    sdk = _fresh_sdk()
    sdk.invariant("holds", lambda: True)
    sdk.invariant("also-holds", lambda: {"ok": True})
    cap = io.StringIO()
    with contextlib.redirect_stderr(cap):
        sdk.record_snapshot(Node("window"), action="tap")
    assert _marker_lines(cap.getvalue()) == [], "a satisfied registry emits nothing"
    print("PASS clean_state_is_silent")


def test_inert_without_the_gate():
    os.environ.pop("REPROIT_UNDER_FUZZER", None)
    sdk = _fresh_sdk()
    sdk.invariant("violated", lambda: False)
    cap = io.StringIO()
    with contextlib.redirect_stderr(cap):
        sdk.record_snapshot(Node("window"), action="tap")
    assert _marker_lines(cap.getvalue()) == [], "no marker in production (gate unset)"
    print("PASS inert_without_the_gate")


if __name__ == "__main__":
    test_violations_reported_under_the_fuzzer()
    test_clean_state_is_silent()
    test_inert_without_the_gate()
    print("all invariant tests passed")
