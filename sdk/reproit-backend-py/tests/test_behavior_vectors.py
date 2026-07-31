"""Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).

Eleven SDKs hand implement one contract, so a defect otherwise has to be found
eleven times. Four instances of one class landed in a single day, and every
group below was written against one of them.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

from reproit_backend_py import instrument, replay
from reproit_backend_py.trace import redact

VECTORS = json.loads(
    (Path(__file__).resolve().parents[2] / "capture-behavior-v1.json").read_text()
)


def body_of(spec: dict) -> object:
    if "bodyRepeat" in spec:
        char, count = spec["bodyRepeat"]
        return char * count
    return spec.get("body")


def test_constants_match_the_shared_vectors() -> None:
    assert instrument.MAX_EXCHANGE_BODY_BYTES == VECTORS["constants"]["maxExchangeBodyBytes"]
    assert replay.DIVERGENCE_MARKER == VECTORS["constants"]["divergenceMarker"]


def test_bounds_vectors() -> None:
    for case in VECTORS["bounds"]["cases"]:
        actual = instrument._bounded_body(
            body_of(case["input"]), case["input"].get("contentType")
        )
        expected = dict(case["expect"])
        if isinstance(expected.get("body"), dict) and "repeat" in expected["body"]:
            char, count = expected["body"]["repeat"]
            expected["body"] = char * count
        assert actual == expected, case["name"]


def test_bounds_digests_are_over_every_byte() -> None:
    """The truncated case keeps identity, so the digest must cover the whole
    body rather than the inline prefix."""
    for case in VECTORS["bounds"]["cases"]:
        if not case["expect"].get("truncated"):
            continue
        char, count = case["input"]["bodyRepeat"]
        whole = (char * count).encode()
        assert case["expect"]["bodySha256"] == hashlib.sha256(whole).hexdigest()
        assert case["expect"]["bodyBytes"] == len(whole)


def test_redaction_type_vectors() -> None:
    for case in VECTORS["redaction"]["typeCases"]:
        assert redact(case["input"]) == case["expect"], case["input"]


def test_redaction_key_folding_vectors() -> None:
    for case in VECTORS["redaction"]["foldingCases"]:
        out = redact({case["field"]: "value"})
        value = out[case["field"]]
        was_redacted = isinstance(value, dict) and "$reproit" in value
        assert was_redacted == case["secret"], case["field"]


def test_redaction_nesting_vectors() -> None:
    for case in VECTORS["redaction"]["nestingCases"]:
        assert redact(case["input"]) == case["expect"], case["input"]


def test_trigger_token_is_in_the_protocol_vocabulary() -> None:
    token = VECTORS["triggerTokens"]["bySdkKind"]["backend"]
    assert token in VECTORS["triggerTokens"]["allowed"]
    source = (
        Path(__file__).resolve().parents[1] / "reproit_backend_py" / "capture.py"
    ).read_text()
    assert token in source
    for bad in VECTORS["triggerTokens"]["rejected"]:
        assert f'"{bad}"' not in source
        assert f"'{bad}'" not in source
