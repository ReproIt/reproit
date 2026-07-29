#!/usr/bin/env python3
"""Validate one atomic target's clean and adversarial corpus record.

The field benchmark proves the oracle finds the defect. This record proves it
reports nothing on known-good subjects, including subjects that superficially
resemble the defect. A single confirmed false positive fails the gate.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SUPPORT = ROOT / "validation/support-manifest.json"
MAX_BYTES = 1_048_576
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HTTPS_GITHUB = re.compile(r"^https://github\.com/[^/]+/[^/]+(?:\.git)?$")
CASE_ID = re.compile(r"^[a-z0-9-]{3,64}$")
KINDS = {"clean", "adversarial"}
MIN_CLEAN = 1
MIN_ADVERSARIAL = 2


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def exact_keys(value: dict, expected: set[str], label: str) -> None:
    actual = set(value)
    require(actual == expected, f"{label} keys: expected {sorted(expected)}, got {sorted(actual)}")


def validate_case(case: object, index: int) -> str:
    label = f"cases[{index}]"
    require(isinstance(case, dict), f"{label} must be an object")
    exact_keys(
        case,
        {
            "id", "kind", "application", "repository", "revision", "fixture",
            "variant", "why", "observationReached", "identity", "falsePositive",
            "observation",
        },
        label,
    )
    require(isinstance(case["id"], str) and CASE_ID.match(case["id"]), f"{label}.id is invalid")
    require(case["kind"] in KINDS, f"{label}.kind is invalid")
    require(isinstance(case["application"], str) and case["application"], f"{label} has no application")
    require(
        isinstance(case["repository"], str) and HTTPS_GITHUB.match(case["repository"]),
        f"{label}.repository must be a GitHub HTTPS repository",
    )
    require(
        isinstance(case["revision"], str) and HEX_40.match(case["revision"]),
        f"{label}.revision must be a full commit SHA",
    )
    require(
        isinstance(case["why"], str) and case["why"].strip(),
        f"{label} does not say why it belongs in the corpus",
    )
    # A subject the oracle never reached proves nothing either way.
    require(case["observationReached"] is True, f"{label} never reached its observation point")
    require(
        case["identity"] is None and case["falsePositive"] is False,
        f"{label} is a known-good subject but reported {case['identity']!r}",
    )
    require(isinstance(case["observation"], dict), f"{label}.observation must be retained")
    require(
        case["observation"].get("identity") is None,
        f"{label} retained observation disagrees with the case verdict",
    )
    return case["kind"]


def validate(document: object) -> None:
    require(isinstance(document, dict), "corpus root must be an object")
    exact_keys(
        document,
        {
            "schemaVersion", "target", "worker", "cleanCases", "adversarialCases",
            "confirmedFalsePositives", "unreachedObservations", "containersRemaining", "cases",
        },
        "corpus",
    )
    require(document["schemaVersion"] == 1, "unsupported corpus schemaVersion")
    targets = json.loads(SUPPORT.read_text(encoding="utf-8"))["targets"]
    require(document["target"] in targets, f"corpus target {document['target']!r} is not a target")
    worker = document["worker"]
    require(isinstance(worker, dict), "corpus worker must be an object")
    exact_keys(worker, {"image", "platform", "network"}, "corpus.worker")
    require(worker["network"] == "none", "corpus subjects must run offline")
    require(document["containersRemaining"] == 0, "the corpus left a container behind")
    require(document["unreachedObservations"] == 0, "a corpus subject never reached observation")

    cases = document["cases"]
    require(isinstance(cases, list) and cases, "corpus must hold cases")
    kinds = [validate_case(case, index) for index, case in enumerate(cases)]
    clean = kinds.count("clean")
    adversarial = kinds.count("adversarial")
    require(clean == document["cleanCases"], "cleanCases disagrees with the case list")
    require(adversarial == document["adversarialCases"], "adversarialCases disagrees with the cases")
    require(clean >= MIN_CLEAN, f"corpus needs at least {MIN_CLEAN} clean subject")
    require(
        adversarial >= MIN_ADVERSARIAL,
        f"corpus needs at least {MIN_ADVERSARIAL} adversarial subjects",
    )
    require(
        document["confirmedFalsePositives"] == 0,
        f"corpus reports {document['confirmedFalsePositives']} confirmed false positive(s)",
    )
    identifiers = {case["id"] for case in cases}
    require(len(identifiers) == len(cases), "corpus case ids must be unique")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("corpus", type=Path)
    args = parser.parse_args()
    require(args.corpus.stat().st_size <= MAX_BYTES, "corpus record exceeds 1 MiB")
    document = json.loads(args.corpus.read_text(encoding="utf-8"))
    validate(document)
    print(
        f"{document['target']} corpus: PASS "
        f"({document['cleanCases']} clean, {document['adversarialCases']} adversarial, "
        f"0 false positives)"
    )


if __name__ == "__main__":
    main()
