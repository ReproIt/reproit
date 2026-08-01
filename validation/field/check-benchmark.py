#!/usr/bin/env python3
"""Validate one atomic target's independent-application field gate."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SUPPORT = ROOT / "validation/support-manifest.json"
MAX_BYTES = 1_048_576
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
# The contract wants a RESOLVABLE public issue reference, not a GitHub one.
# Restricting to github.com was an accident of the first campaigns all living
# there, and it blocked two targets outright: four of five qualified GTK
# applications are on gitlab.gnome.org and every qualified Qt Quick application
# is on invent.kde.org, so neither target could record a benchmark at all no
# matter how much work was done. This is a named ALLOWLIST rather than a general
# URL match, so the reference stays as checkable as it was: each forge below
# hosts public, stable, numerically addressed issues.
FORGES = (
    "github.com",
    "gitlab.com",
    "gitlab.gnome.org",
    "gitlab.freedesktop.org",
    "invent.kde.org",
    "codeberg.org",
)
_HOSTS = "|".join(host.replace(".", r"\.") for host in FORGES)
# GitLab-family projects can be nested in groups, so allow extra path segments.
# The path must not contain an "issues" or GitLab "-" router segment: without
# that assertion an issue URL carrying an INVALID number matched as a
# repository and slipped past both patterns. Written as a whole-path lookahead
# because a per-segment one anchored to end-of-string instead of end-of-segment
# and silently did nothing.
HTTPS_GITHUB = re.compile(
    rf"^https://(?:{_HOSTS})/(?!.*/(?:issues|-)(?:/|$))[^/]+(?:/[^/]+)+?(?:\.git)?$"
)
HTTPS_ISSUE = re.compile(
    rf"^https://(?:{_HOSTS})/[^/]+(?:/[^/]+)+?/(?:-/)?issues/[1-9][0-9]*$"
    r"|^https://bugs\.kde\.org/show_bug\.cgi\?id=[1-9][0-9]*$"
)
REQUIRED_CONTROLS = {"fixed-revision", "neighboring-legal-behavior"}
REQUIRED_RUNS = 3


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def exact_keys(value: dict, expected: set[str], label: str) -> None:
    actual = set(value)
    require(actual == expected, f"{label} keys: expected {sorted(expected)}, got {sorted(actual)}")


def validate_evidence_record(
    application: dict,
    label: str,
    root: Path,
    schema_version: int,
) -> None:
    json_paths = [path for path in application["evidence"] if path.endswith(".json")]
    require(len(json_paths) == 1, f"{label} must name exactly one structured evidence record")
    record_path = root / json_paths[0]
    record = json.loads(record_path.read_text(encoding="utf-8"))
    require(isinstance(record, dict), f"{label} structured evidence must be an object")
    expected = {
        "issue": application["issueUrl"],
        "affectedRevision": application["affectedRevision"],
        "fixedRevision": application["fixedRevision"],
        "identity": application["expectedIdentity"],
    }
    for field, value in expected.items():
        require(record.get(field) == value, f"{label} evidence {field} disagrees with benchmark")
    memory_measurement = "js-heap"
    if schema_version == 3:
        memory_measurement = application["metrics"]["memoryMeasurement"]
        require(
            record.get("memoryMeasurement") == memory_measurement,
            f"{label} evidence memory measurement disagrees with benchmark",
        )
    for revision in ("affected", "fixed"):
        runs = record.get(revision)
        require(
            isinstance(runs, list) and len(runs) == REQUIRED_RUNS,
            f"{label} evidence needs exactly {REQUIRED_RUNS} {revision} runs",
        )
        for run_index, run in enumerate(runs, start=1):
            require(
                isinstance(run, dict)
                and run.get("run") == run_index
                and run.get("cleanLaunch") is True
                and run.get("exceptions") == [],
                f"{label} evidence {revision} run {run_index} is not clean",
            )
            heap_mib = run.get("jsHeapMiB")
            if memory_measurement == "js-heap":
                require(
                    isinstance(heap_mib, (int, float)) and not isinstance(heap_mib, bool),
                    f"{label} evidence {revision} run {run_index} has invalid jsHeapMiB",
                )
            else:
                require(
                    heap_mib is None,
                    f"{label} evidence {revision} run {run_index} invents unavailable memory",
                )
            require(
                isinstance(run.get("elapsedSeconds"), (int, float))
                and not isinstance(run.get("elapsedSeconds"), bool),
                f"{label} evidence {revision} run {run_index} has invalid elapsedSeconds",
            )
    require(
        isinstance(record.get("neighboringLegalBehavior"), str)
        and record["neighboringLegalBehavior"].strip(),
        f"{label} evidence has no neighboring legal behavior",
    )
    require(
        isinstance(record.get("minimizedAction"), str) and record["minimizedAction"].strip(),
        f"{label} evidence has no minimized action",
    )
    all_runs = record["affected"] + record["fixed"]
    replay_p95 = max(run["elapsedSeconds"] for run in record["affected"])
    if memory_measurement == "js-heap":
        peak_memory = max(run["jsHeapMiB"] for run in all_runs)
        require(
            application["metrics"]["peakMemoryMiB"] == peak_memory,
            f"{label} peak memory metric disagrees with evidence",
        )
    else:
        require(
            application["metrics"]["peakMemoryMiB"] is None,
            f"{label} peak memory must be null when measurement is unavailable",
        )
    require(
        application["metrics"]["replaySecondsP95"] == replay_p95,
        f"{label} replay p95 metric disagrees with evidence",
    )


def validate_application(
    application: object,
    index: int,
    root: Path = ROOT,
    schema_version: int = 2,
) -> None:
    label = f"applications[{index}]"
    require(isinstance(application, dict), f"{label} must be an object")
    exact_keys(
        application,
        {
            "id", "repository", "issueUrl", "affectedRevision", "fixedRevision",
            "authority", "expectedIdentity", "affectedReproductions",
            "fixedReproductions", "minimized", "controls", "manualReview",
            "metrics", "evidence",
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

    affected = application["affectedReproductions"]
    require(isinstance(affected, list) and len(affected) == REQUIRED_RUNS,
            f"{label} needs exactly {REQUIRED_RUNS} affected-revision reproductions")
    for run in affected:
        require(
            isinstance(run, dict)
            and set(run) == {"status", "identity", "cleanLaunch", "observationReached"},
            f"{label} affected reproduction shape is invalid",
        )
        require(run["status"] == "reproduced" and run["cleanLaunch"] is True,
                f"{label} affected reproduction did not complete from a clean launch")
        require(run["observationReached"] is True,
                f"{label} affected reproduction did not reach its observation point")
        require(run["identity"] == application["expectedIdentity"],
                f"{label} affected reproduction identity drifted")

    fixed = application["fixedReproductions"]
    require(isinstance(fixed, list) and len(fixed) == REQUIRED_RUNS,
            f"{label} needs exactly {REQUIRED_RUNS} fixed-revision controls")
    for run in fixed:
        require(
            isinstance(run, dict)
            and set(run) == {"status", "identity", "cleanLaunch", "observationReached"},
            f"{label} fixed reproduction shape is invalid",
        )
        require(run["status"] == "not_reproduced" and run["cleanLaunch"] is True,
                f"{label} fixed control did not complete from a clean launch")
        require(run["observationReached"] is True,
                f"{label} fixed control did not reach its observation point")
        require(run["identity"] is None,
                f"{label} fixed control still observed a failure identity")
    require(application["minimized"] is True, f"{label} was not minimized and reverified")
    require(set(application["controls"]) == REQUIRED_CONTROLS,
            f"{label} negative controls are incomplete")
    require(application["manualReview"] == "confirmed-target-bug",
            f"{label} has no confirmed manual review")

    metrics = application["metrics"]
    require(isinstance(metrics, dict), f"{label}.metrics must be an object")
    metric_keys = {"setupSeconds", "replaySecondsP95", "peakMemoryMiB"}
    if schema_version == 3:
        metric_keys.add("memoryMeasurement")
    exact_keys(metrics, metric_keys, f"{label}.metrics")
    require(isinstance(metrics["setupSeconds"], int) and 1 <= metrics["setupSeconds"] <= 7200,
            f"{label}.metrics.setupSeconds is outside bounds")
    require(isinstance(metrics["replaySecondsP95"], (int, float))
            and 0 < metrics["replaySecondsP95"] <= 900,
            f"{label}.metrics.replaySecondsP95 is outside bounds")
    if schema_version == 3:
        require(
            metrics["memoryMeasurement"] in {"js-heap", "unavailable"},
            f"{label}.metrics.memoryMeasurement is invalid",
        )
    memory_measurement = metrics.get("memoryMeasurement", "js-heap")
    if memory_measurement == "js-heap":
        require(
            isinstance(metrics["peakMemoryMiB"], int)
            and 1 <= metrics["peakMemoryMiB"] <= 32768,
            f"{label}.metrics.peakMemoryMiB is outside bounds",
        )
    else:
        require(
            metrics["peakMemoryMiB"] is None,
            f"{label}.metrics.peakMemoryMiB must be null when unavailable",
        )

    evidence = application["evidence"]
    require(isinstance(evidence, list) and 2 <= len(evidence) <= 20,
            f"{label}.evidence must contain reviewable paths")
    for path in evidence:
        require(isinstance(path, str) and path.startswith("validation/field/evidence/")
                and ".." not in path and len(path) <= 240,
                f"{label} has an unsafe evidence path")
        require((root / path).is_file(), f"{label} evidence is missing: {path}")
    validate_evidence_record(application, label, root, schema_version)


def validate(document: object, allow_pending: bool = False, root: Path = ROOT) -> None:
    require(isinstance(document, dict), "benchmark root must be an object")
    exact_keys(document, {"schemaVersion", "target", "status", "applications"}, "benchmark")
    schema_version = document["schemaVersion"]
    require(schema_version in {2, 3}, "unsupported benchmark schemaVersion")
    targets = json.loads(SUPPORT.read_text(encoding="utf-8"))["targets"]
    require(document["target"] in targets,
            f"benchmark target must be a support-manifest target, got {document['target']!r}")
    require(document["status"] in {"pending", "complete"}, "benchmark status is invalid")
    applications = document["applications"]
    require(isinstance(applications, list), "applications must be an array")
    if document["status"] == "pending" and allow_pending:
        require(len(applications) <= 2, "pending benchmark exceeds the two-application bound")
        return
    require(document["status"] == "complete", "field benchmark is still pending")
    require(len(applications) == 2, "complete benchmark requires exactly two applications")
    for index, application in enumerate(applications):
        validate_application(application, index, root, schema_version)
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
    print(f"{document['target']} field benchmark: {state}")


if __name__ == "__main__":
    main()
