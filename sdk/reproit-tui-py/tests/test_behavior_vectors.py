#!/usr/bin/env python3
"""Execute the shared behavioral vectors for the FROZEN runner wire.

This SDK is replay only: it never records a capture batch, so it has no inline
body budget, no header table and no $reproit placeholder. Its whole shared
surface with the rest of the fleet is the secret-key predicate, and eight
languages hand implement that predicate. A divergence about which keys count as
secret is silent in both directions: too narrow and a credential ships inside a
capsule, too wide and a field replay needs is scrubbed into a placeholder that
never matches. ../capture-behavior-v1.json states the predicate once and every
one of the eight runs it, so a defect is found once instead of eight times.

One difference from the capture wire is deliberate and is asserted here so it
cannot be closed by accident: `idempotency_key` IS secret on the capture wire
and is NOT secret here. The runner list is thirteen parts, one shorter, because
changing it would change bytes the fuzz harness compares.
"""

import json
from pathlib import Path

from reproit_tui_py.causal import _SECRET, _redact

# ../capture-behavior-v1.json relative to the SDK root, which is one level up
# from tests/.
_VECTORS = json.loads(
    (Path(__file__).resolve().parents[2] / "capture-behavior-v1.json").read_text()
)["causalRedaction"]


def test_causal_folding_cases():
    for case in _VECTORS["foldingCases"]:
        field, secret = case["field"], case["secret"]
        assert bool(_SECRET.search(field)) is secret, field
        safe = _redact({field: "raw-value"})
        assert safe[field] == ("<reproit:string:length=9>" if secret else "raw-value"), field


def test_causal_placeholder():
    for case in _VECTORS["foldingCases"]:
        if not case["secret"]:
            continue
        assert _redact({case["field"]: 7})[case["field"]] == _VECTORS["placeholder"]


if __name__ == "__main__":
    test_causal_folding_cases()
    test_causal_placeholder()
    print("ok")
