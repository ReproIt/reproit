#!/usr/bin/env python3
"""Validate the independent-application field gate for the stable 1.0 target."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

MAX_BYTES = 1_048_576
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HTTPS_GITHUB = re.compile(r"^https://github\.com/[^/]+/[^/]+(?:\.git)?$")
HTTPS_ISSUE = re.compile(r"^https://github\.com/[^/]+/[^/]+/issues/[1-9][0-9]*$")
REQUIRED_CONTROLS = {"fixed-revision", "neighboring-legal-behavior"}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def exact_keys(value: dict, expected: set[str], label: str) -> None:
    actual = set(value)
    require(actual == expected, f"{label} keys: expected {sorted(expected)}, got {sorted(actual)}")


def validate_application(application: object, index: int) -> None:
    label = f"applications[{index}]"
    require(isinstance(application, dict), f"{label} must be an object")
    exact_keys(
        application,
        {
            "id", "repository", "issueUrl", "affectedRevision", "fixedRevision",
            "authority", "expectedIdentity", "reproductions", "minimized",
            "controls", "manualReview", "metrics", "evidence",
        },
        label,
    )
    identifier = application["id"]
    require(isinstance(identifier, str) and re.match(r"^[a-z0-9-]{3,64}$", identifier),
            f"{label}.id is invalid")
    require(isinstance(application["repository"], str)
            and HTTPS_GITHUB.match(application["repository"]),
            f"{label}.repository must be a GitHub HTTPS repository")
    require(isinstance(application["issueUrl"], str) and HTTPS_ISSUE.match(application["issueUrl"]),
            f"{label}.issueUrl must be a GitHub issue URL")
    for field in ("affectedRevision", "fixedRevision"):
        require(isinstance(application[field], str) and HEX_40.match(application[field]),
                f"{label}.{field} must be a full commit SHA")
    require(application["affectedRevision"] != application["fixedRevision"],
            f"{label} revisions must differ")
    exact_authorities = {"standard", "authored-contract", "typed-model", "platform"}
    require(application["authority"] in exact_authorities, f"{label}.authority is not exact")
    require(isinstance(application["expectedIdentity"], str)
            and 1 <= len(application["expectedIdentity"]) <= 256,
            f"{label}.expectedIdentity is invalid")

    reproductions = application["reproductions"]
    require(isinstance(reproductions, list) and 3 <= len(reproductions) <= 10,
            f"{label} needs 3 to 10 affected-revision reproductions")
    for run in reproductions:
        require(isinstance(run, dict) and set(run) == {"status", "identity", "cleanLaunch"},
                f"{label} reproduction shape is invalid")
        require(run["status"] == "reproduced" and run["cleanLaunch"] is True,
                f"{label} reproduction did not complete from a clean launch")
        require(run["identity"] == application["expectedIdentity"],
                f"{label} reproduction identity drifted")
    require(application["minimized"] is True, f"{label} was not minimized and reverified")
    require(set(application["controls"]) == REQUIRED_CONTROLS,
            f"{label} negative controls are incomplete")
    require(application["manualReview"] == "confirmed-target-bug",
            f"{label} has no confirmed manual review")

    metrics = application["metrics"]
    require(isinstance(metrics, dict), f"{label}.metrics must be an object")
    exact_keys(metrics, {"setupSeconds", "replaySecondsP95", "peakMemoryMiB"}, f"{label}.metrics")
    require(isinstance(metrics["setupSeconds"], int) and 1 <= metrics["setupSeconds"] <= 7200,
            f"{label}.metrics.setupSeconds is outside bounds")
    require(isinstance(metrics["replaySecondsP95"], (int, float))
            and 0 < metrics["replaySecondsP95"] <= 900,
            f"{label}.metrics.replaySecondsP95 is outside bounds")
    require(isinstance(metrics["peakMemoryMiB"], int) and 1 <= metrics["peakMemoryMiB"] <= 32768,
            f"{label}.metrics.peakMemoryMiB is outside bounds")

    evidence = application["evidence"]
    require(isinstance(evidence, list) and 2 <= len(evidence) <= 20,
            f"{label}.evidence must contain reviewable paths")
    for path in evidence:
        require(isinstance(path, str) and path.startswith("validation/field/evidence/")
                and ".." not in path and len(path) <= 240,
                f"{label} has an unsafe evidence path")
        require(Path(path).is_file(), f"{label} evidence is missing: {path}")


def validate(document: object, allow_pending: bool = False) -> None:
    require(isinstance(document, dict), "benchmark root must be an object")
    exact_keys(document, {"schemaVersion", "target", "status", "applications"}, "benchmark")
    require(document["schemaVersion"] == 1, "unsupported benchmark schemaVersion")
    require(document["target"] == "web-chromium", "benchmark target must be web-chromium")
    require(document["status"] in {"pending", "complete"}, "benchmark status is invalid")
    applications = document["applications"]
    require(isinstance(applications, list), "applications must be an array")
    if document["status"] == "pending" and allow_pending:
        require(len(applications) <= 2, "pending benchmark exceeds the two-application bound")
        return
    require(document["status"] == "complete", "field benchmark is still pending")
    require(len(applications) == 2, "complete benchmark requires exactly two applications")
    for index, application in enumerate(applications):
        validate_application(application, index)
    repositories = {application["repository"] for application in applications}
    require(len(repositories) == 2, "benchmark applications must use independent repositories")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("benchmark", type=Path)
    parser.add_argument("--allow-pending", action="store_true")
    args = parser.parse_args()
    require(args.benchmark.stat().st_size <= MAX_BYTES, "benchmark exceeds 1 MiB")
    document = json.loads(args.benchmark.read_text(encoding="utf-8"))
    validate(document, args.allow_pending)
    state = "PENDING" if document["status"] == "pending" else "PASS"
    print(f"Chromium field benchmark: {state}")


if __name__ == "__main__":
    main()
