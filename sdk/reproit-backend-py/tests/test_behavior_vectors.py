"""Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).

Eleven SDKs hand implement one contract, so a defect otherwise has to be found
eleven times. Four instances of one class landed in a single day, and every
group below was written against one of them. The groups are harvested, not
invented; each names the defect it pins:

  bounds                a budget measured in string length rather than encoded
                        bytes recorded 4096 characters of "euro" inline, 12288
                        bytes, past a budget the replayer trusts.
  headers               the 32 header cap applied in arrival order recorded a
                        different subset per run (Go's defect, repeated by
                        Android). The cap is over NAME SORTED order, so the
                        generated case is fed scrambled on purpose.
  redaction.typeCases   the $reproit stub must report the ORIGINAL type and
                        length, not "string" for everything.
  redaction.foldingCases  secret detection folds case and separators and
                        matches substrings: `X-Authorization` and `tokenizer`
                        are secret, `username` is not.
  redaction.nestingCases  redaction recurses through objects AND arrays; a
                        top-level-only scrub shipped nested keys in plaintext.
  redaction.structureCases  redaction preserves shape: no key dropped, no array
                        shortened, an explicit null stays a null VALUE. An
                        Android encoder dropping null map values made a capsule
                        say {"symbol": "ACME"} where production sent
                        {"prices": null}, and replay reproduced a DIFFERENT bug.
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


def scrambled_headers(spec: dict) -> dict:
    """Build the generated header table in an order that is neither ascending
    nor descending: 17 is coprime with 40, so `step * 17 % count` is a
    permutation. A cap applied before sorting then keeps a visibly wrong
    subset instead of accidentally passing on already-sorted input."""
    count = spec["headerCount"]
    headers = {}
    for step in range(count):
        index = (step * 17) % count
        headers[spec["namePattern"] % index] = spec["value"]
    return headers


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


def test_header_vectors() -> None:
    for case in VECTORS["headers"]["cases"]:
        if "input" in case:
            assert instrument._bounded_headers(case["input"]["headers"]) == case["expect"], (
                case["name"]
            )
            continue
        actual = instrument._bounded_headers(scrambled_headers(case["inputGenerated"]))
        names = sorted(actual["headers"])
        assert len(names) == case["expect"]["headerCount"], case["name"]
        # The cap is over sorted names, not the order the headers arrived in.
        assert names[0] == case["expect"]["firstName"], case["name"]
        assert names[-1] == case["expect"]["lastName"], case["name"]


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


def test_redaction_structure_vectors() -> None:
    for case in VECTORS["redaction"]["structureCases"]:
        assert redact(case["input"]) == case["expect"], case["name"]


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


def test_matching_vectors() -> None:
    for case in VECTORS["matching"]["cases"]:
        actual = replay._http_request_matcher(case["recorded"], case["live"])
        assert actual == case["expect"]["matches"], case["name"]


def test_pg_matching_vectors() -> None:
    for case in VECTORS["matching"]["pgCases"]:
        actual = replay._db_request_matcher(case["recorded"], case["live"])
        assert actual == case["expect"]["matches"], case["name"]


def test_divergence_marker_starts_the_line_and_carries_required_fields(capsys) -> None:
    case = VECTORS["divergence"]["cases"][0]
    session = replay.ReplaySession(
        {
            "format": "reproit-backend-capture",
            "version": 2,
            "operation": "GET /x",
            "oracle": "backend-server-error",
            "events": [
                {"kind": "effect", "sequence": index + 1, "exchange": exchange}
                for index, exchange in enumerate(case["capsuleExchanges"])
            ],
        }
    )
    assert session.match("http", {"method": "GET", "url": "http://svc/unknown"}) is None
    lines = capsys.readouterr().err.splitlines()
    prefix = VECTORS["divergence"]["markerPrefix"]
    marker = next(line for line in lines if line.startswith(prefix))
    report = json.loads(marker[len(prefix):])
    for field in VECTORS["divergence"]["reportFields"]["required"]:
        assert field in report, field
    assert report["consumed"] == case["expect"]["consumed"]
    assert report["total"] == case["expect"]["total"]
    assert report["expected"] == case["expect"]["expectedRequest"]
