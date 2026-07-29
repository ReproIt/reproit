#!/usr/bin/env python3
"""Validate the fail-closed capture-to-replay capability ledger."""

from __future__ import annotations

import json
import re
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LEDGER = Path(__file__).with_name("coverage.json")
CAPTURE_ENUM = ROOT / "crates/reproit-protocol/src/capture.rs"
MAX_CLAIMS = 128
MAX_EVIDENCE = 32
MAX_BLOCKERS = 32

CAPTURE_STATES = {
    "complete",
    "partial",
    "adapter-specific",
    "unavailable",
    "modeled-only",
}
COMPILER_STATES = {"complete", "partial", "modeled-only"}
REPLAY_STATES = {
    "structured",
    "command-provider",
    "adapter-specific",
    "unavailable",
}
QUALIFICATION_STATES = {"independent", "fixture", "field", "unqualified"}


class LedgerError(Exception):
    """The capability ledger made an incomplete or unsupported claim."""


def camel_to_kebab(value: str) -> str:
    first = re.sub(r"(.)([A-Z][a-z]+)", r"\1-\2", value)
    return re.sub(r"([a-z0-9])([A-Z])", r"\1-\2", first).lower()


def protocol_capabilities() -> list[str]:
    source = CAPTURE_ENUM.read_text(encoding="utf-8")
    match = re.search(
        r"pub enum CaptureCapabilityKind \{(?P<body>.*?)^\}",
        source,
        flags=re.MULTILINE | re.DOTALL,
    )
    if match is None:
        raise LedgerError(f"cannot locate CaptureCapabilityKind in {CAPTURE_ENUM}")
    variants = re.findall(r"^\s{4}([A-Z][A-Za-z0-9]+),$", match.group("body"), re.MULTILINE)
    if not variants:
        raise LedgerError("CaptureCapabilityKind contains no parseable variants")
    return sorted(camel_to_kebab(variant) for variant in variants)


def require_text(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > 4096:
        raise LedgerError(f"{label} must be bounded non-empty text")
    return value


def require_string_list(
    value: object, label: str, maximum: int, *, allow_empty: bool
) -> list[str]:
    if (
        not isinstance(value, list)
        or len(value) > maximum
        or (not allow_empty and not value)
    ):
        qualifier = "0.." if allow_empty else "1.."
        raise LedgerError(f"{label} must contain {qualifier}{maximum} entries")
    output = []
    for index, entry in enumerate(value):
        output.append(require_text(entry, f"{label}[{index}]"))
    if len(output) != len(set(output)):
        raise LedgerError(f"{label} contains duplicates")
    return output


def validate_evidence(claim_id: str, value: object) -> list[str]:
    paths = require_string_list(
        value,
        f"{claim_id}.evidence",
        MAX_EVIDENCE,
        allow_empty=False,
    )
    for path in paths:
        candidate = Path(path)
        if candidate.is_absolute() or ".." in candidate.parts:
            raise LedgerError(f"{claim_id}.evidence contains unsafe path {path!r}")
        if not (ROOT / candidate).is_file():
            raise LedgerError(f"{claim_id}.evidence path {path!r} does not exist")
    return paths


def validate_claim(claim: object) -> dict[str, object]:
    if not isinstance(claim, dict):
        raise LedgerError("every capability claim must be an object")
    expected_fields = {
        "id",
        "capture",
        "compiler",
        "replay",
        "provider",
        "qualification",
        "evidence",
        "blockers",
    }
    if set(claim) != expected_fields:
        raise LedgerError(
            f"capability claim fields are {sorted(claim)}, expected {sorted(expected_fields)}"
        )
    claim_id = require_text(claim["id"], "claim.id")
    capture = claim["capture"]
    compiler = claim["compiler"]
    replay = claim["replay"]
    qualification = claim["qualification"]
    if capture not in CAPTURE_STATES:
        raise LedgerError(f"{claim_id}.capture has invalid state {capture!r}")
    if compiler not in COMPILER_STATES:
        raise LedgerError(f"{claim_id}.compiler has invalid state {compiler!r}")
    if replay not in REPLAY_STATES:
        raise LedgerError(f"{claim_id}.replay has invalid state {replay!r}")
    if qualification not in QUALIFICATION_STATES:
        raise LedgerError(
            f"{claim_id}.qualification has invalid state {qualification!r}"
        )
    provider = require_text(claim["provider"], f"{claim_id}.provider")
    validate_evidence(claim_id, claim["evidence"])
    blockers = require_string_list(
        claim["blockers"],
        f"{claim_id}.blockers",
        MAX_BLOCKERS,
        allow_empty=True,
    )
    incomplete = (
        capture != "complete"
        or compiler != "complete"
        or replay != "structured"
        or qualification != "independent"
    )
    if incomplete and not blockers:
        raise LedgerError(f"{claim_id} is incomplete but declares no blocker")
    if not incomplete and blockers:
        raise LedgerError(f"{claim_id} is fully qualified but still declares blockers")
    if replay == "unavailable" and provider != "none":
        raise LedgerError(f"{claim_id} has unavailable replay but names a provider")
    return claim


def validate(ledger: object) -> dict[str, object]:
    if not isinstance(ledger, dict) or set(ledger) != {
        "schemaVersion",
        "claims",
        "summary",
    }:
        raise LedgerError("ledger must contain schemaVersion, claims, and summary")
    if ledger["schemaVersion"] != 1:
        raise LedgerError("schemaVersion must equal 1")
    raw_claims = ledger["claims"]
    if not isinstance(raw_claims, list) or not 1 <= len(raw_claims) <= MAX_CLAIMS:
        raise LedgerError(f"claims must contain 1..{MAX_CLAIMS} entries")
    claims = [validate_claim(claim) for claim in raw_claims]
    identifiers = [str(claim["id"]) for claim in claims]
    if identifiers != sorted(identifiers) or len(identifiers) != len(set(identifiers)):
        raise LedgerError("capability ids must be unique and sorted")
    expected = protocol_capabilities()
    if identifiers != expected:
        missing = sorted(set(expected) - set(identifiers))
        extra = sorted(set(identifiers) - set(expected))
        raise LedgerError(f"capability drift: missing={missing}, extra={extra}")
    actual_summary = {
        "capture": dict(sorted(Counter(claim["capture"] for claim in claims).items())),
        "compiler": dict(
            sorted(Counter(claim["compiler"] for claim in claims).items())
        ),
        "replay": dict(sorted(Counter(claim["replay"] for claim in claims).items())),
        "qualification": dict(
            sorted(Counter(claim["qualification"] for claim in claims).items())
        ),
    }
    if ledger["summary"] != actual_summary:
        raise LedgerError(
            f"summary drift: expected {actual_summary}, found {ledger['summary']}"
        )
    return {
        "schemaVersion": 1,
        "capabilities": len(claims),
        "summary": actual_summary,
    }


def main() -> int:
    try:
        ledger = json.loads(LEDGER.read_text(encoding="utf-8"))
        report = validate(ledger)
    except (OSError, json.JSONDecodeError, LedgerError) as error:
        sys.stderr.write(f"capability ledger: {error}\n")
        return 1
    json.dump(report, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
