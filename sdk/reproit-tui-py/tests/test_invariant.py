#!/usr/bin/env python3
"""Dogfood the app-invariant oracle for the Python TUI SDK, both directions.

observe_contents is the path observe() funnels through, so registering invariants
and driving one observe exercises the marker emission end to end. The TUI backend
(crates/reproit/src/backends/tui.rs) provisions REPROIT_INVARIANT_FILE, scrapes
the appended markers, and re-emits them as EXPLORE:INVARIANT.

Run:
    python3 sdk/reproit-tui-py/tests/test_invariant.py
"""

import json
import os
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SDK_ROOT = os.path.dirname(HERE)
sys.path.insert(0, SDK_ROOT)

from reproit_tui_py import Reporter  # noqa: E402


def _markers(path):
    if not os.path.exists(path):
        return []
    with open(path, encoding="utf-8") as f:
        return [
            json.loads(ln[len("REPROIT_INVARIANT ") :])
            for ln in f.read().splitlines()
            if ln.startswith("REPROIT_INVARIANT ")
        ]


def _raise():
    raise ValueError("kaboom")


def test_violations_reported_under_the_fuzzer():
    path = tempfile.mktemp(suffix=".ndjson")
    os.environ["REPROIT_INVARIANT_FILE"] = path
    r = Reporter(app_id="t", on_event=lambda ev: None)
    r.invariant("holds", lambda: True)
    r.invariant("neg", lambda: {"ok": False, "message": "count < 0"})
    r.invariant("falsy", lambda: 0)
    r.invariant("raises", _raise)
    sig = r.observe_contents("Count: -1", (0, 0), "key:Down")
    markers = _markers(path)
    assert len(markers) == 1, markers
    assert markers[0]["sig"] == sig
    ids = {it["id"]: it["message"] for it in markers[0]["items"]}
    assert set(ids) == {"neg", "falsy", "raises"}, ids
    assert ids["neg"] == "count < 0" and ids["falsy"] == "" and ids["raises"] == "kaboom"
    os.remove(path)
    print("PASS violations_reported_under_the_fuzzer")


def test_clean_and_inert():
    path = tempfile.mktemp(suffix=".ndjson")
    os.environ["REPROIT_INVARIANT_FILE"] = path
    r = Reporter(app_id="t", on_event=lambda ev: None)
    r.invariant("holds", lambda: True)
    r.observe_contents("Count: 3", (0, 0), "load")
    assert _markers(path) == [], "a satisfied registry emits nothing"

    # Inert without the gate (production): no file even with a violation.
    os.environ.pop("REPROIT_INVARIANT_FILE", None)
    r2 = Reporter(app_id="t", on_event=lambda ev: None)
    r2.invariant("violated", lambda: False)
    r2.observe_contents("Count: 3", (0, 0), "load")
    print("PASS clean_and_inert")


if __name__ == "__main__":
    test_violations_reported_under_the_fuzzer()
    test_clean_and_inert()
    print("all invariant tests passed")
