#!/usr/bin/env python3
"""Validate and render Reproit's atomic compatibility contract.

`validation/support-manifest.json` is the single canonical record of the
supported platform targets. Every public compatibility surface is generated
from it: the status JSON, the generated status document, the README supported
platform list, the target table in `docs/compatibility.md`, and the support
claim in `SUPPORT.md`. Hand-edited prose cannot add a target.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import textwrap
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SUPPORT_PATH = ROOT / "validation/support-manifest.json"
GATES_PATH = ROOT / "validation/backends/evidence.json"
STATUS_PATH = ROOT / "validation/compatibility/status.json"
MARKDOWN_PATH = ROOT / "validation/compatibility/STATUS.md"
WORKFLOW_PATH = ROOT / ".github/workflows/ci.yml"
README_PATH = ROOT / "README.md"
COMPATIBILITY_DOC_PATH = ROOT / "docs/compatibility.md"
SUPPORT_DOC_PATH = ROOT / "SUPPORT.md"

MAX_BYTES = 1_048_576
EVIDENCE_KINDS = {"ci-gate", "evidence", "field-benchmark", "missing"}
# Evidence slots every target declares. `missing` records that the slot has no
# retained artifact yet; it is a fact about the record, not a rank.
EVIDENCE_SLOTS = (
    "cleanCorpus",
    "adversarialCorpus",
    "packageInstall",
    "manualReview",
)
BEGIN = "<!-- generated:{name} -->"
END = "<!-- /generated:{name} -->"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def load_json(path: Path) -> object:
    require(path.is_file(), f"missing required file: {path.relative_to(ROOT)}")
    require(path.stat().st_size <= MAX_BYTES, f"{path.relative_to(ROOT)} exceeds 1 MiB")
    return json.loads(path.read_text(encoding="utf-8"))


def field_validator():
    path = ROOT / "validation/field/check-benchmark.py"
    spec = importlib.util.spec_from_file_location("field_benchmark", path)
    require(spec is not None and spec.loader is not None, "cannot load field validator")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def workflow_steps() -> dict[str, set[str]]:
    """Map CI job ids to their step names, without a YAML dependency.

    An evidence slot that names a CI gate must name a job and step that really
    exist, so the record cannot cite an imaginary check.
    """
    require(WORKFLOW_PATH.is_file(), "CI workflow is missing")
    jobs: dict[str, set[str]] = {}
    current: str | None = None
    in_jobs = False
    for line in WORKFLOW_PATH.read_text(encoding="utf-8").splitlines():
        if line.startswith("jobs:"):
            in_jobs = True
            continue
        if not in_jobs:
            continue
        job = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if job:
            current = job.group(1)
            jobs[current] = set()
            continue
        name = re.match(r"^\s+-?\s*name:\s*(.+?)\s*$", line)
        if name and current:
            jobs[current].add(name.group(1).strip("'\""))
    require(bool(jobs), "CI workflow declares no jobs")
    return jobs


def validate_policy(policy: object) -> dict:
    require(isinstance(policy, dict), "support policy must be an object")
    expected = {
        "requireExactIdentity",
        "requireMinimizedTrigger",
        "requireNeighboringLegalBehavior",
        "requireExactCommitNativeEvidence",
    }
    require(set(policy) == expected, "support policy keys do not match the schema")
    for field in sorted(expected):
        require(policy[field] is True, f"support policy {field} must fail closed")
    return policy


def validate_evidence_slot(
    target_id: str, slot: str, value: object, jobs: dict[str, set[str]]
) -> None:
    label = f"{target_id}.evidence.{slot}"
    require(isinstance(value, dict), f"{label} must be an object")
    kind = value.get("kind")
    require(kind in EVIDENCE_KINDS, f"{label} kind is invalid")
    if kind in {"missing", "field-benchmark"}:
        require(set(value) == {"kind"}, f"{label} {kind} slot carries no other field")
        return
    if kind == "ci-gate":
        require(set(value) == {"kind", "job", "step"}, f"{label} needs job and step")
        job = value["job"]
        require(job in jobs, f"{label} names CI job {job!r}, which does not exist")
        require(value["step"] in jobs[job],
                f"{label} names step {value['step']!r}, absent from CI job {job}")
        return
    require(set(value) == {"kind", "path"}, f"{label} needs a path")
    path = value["path"]
    require(isinstance(path, str) and not path.startswith("/") and ".." not in path.split("/"),
            f"{label} path is unsafe")
    require((ROOT / path).is_file(), f"{label} evidence is missing: {path}")


def validate_bounds(target_id: str, bounds: object) -> None:
    label = f"{target_id}.evidence.bounds"
    require(isinstance(bounds, dict) and set(bounds) == {"platforms", "runtime", "framework"},
            f"{label} keys must be platforms, runtime and framework")
    for field in ("platforms", "runtime", "framework"):
        values = bounds[field]
        require(isinstance(values, list) and values
                and len(values) == len(set(values))
                and all(isinstance(item, str) and item.strip() for item in values),
                f"{label}.{field} must be a unique non-empty list")


def validate_evidence(
    target_id: str,
    evidence: object,
    jobs: dict[str, set[str]],
) -> None:
    require(isinstance(evidence, dict), f"{target_id}: evidence must be an object")
    expected = {
        "fieldBenchmark",
        "cleanCorpus",
        "adversarialCorpus",
        "packageInstall",
        "manualReview",
        "bounds",
    }
    require(set(evidence) == expected, f"{target_id}: evidence keys do not match the schema")
    validate_bounds(target_id, evidence["bounds"])
    for slot in EVIDENCE_SLOTS:
        validate_evidence_slot(target_id, slot, evidence[slot], jobs)

    benchmark_path = evidence["fieldBenchmark"]
    require(benchmark_path is None or (isinstance(benchmark_path, str) and benchmark_path),
            f"{target_id}: fieldBenchmark must be null or a path")
    if benchmark_path is None:
        return
    benchmark = load_json(ROOT / benchmark_path)
    field_validator().validate(benchmark, allow_pending=True)
    require(benchmark["target"] == target_id,
            f"{target_id}: field benchmark names another target")


def validate_target(
    target_id: str,
    target: object,
    known_gates: dict,
    jobs: dict[str, set[str]],
) -> None:
    require(isinstance(target, dict), f"{target_id}: target must be an object")
    required = {
        "displayName",
        "family",
        "scope",
        "ownedGates",
        "releaseGates",
        "evidence",
    }
    require(set(target) == required, f"{target_id}: target keys do not match the schema")
    for field in ("displayName", "family", "scope"):
        require(isinstance(target[field], str) and target[field].strip(),
                f"{target_id}: {field} must be non-empty")

    owned = target["ownedGates"]
    release = target["releaseGates"]
    require(isinstance(owned, list) and owned and len(owned) == len(set(owned)),
            f"{target_id}: ownedGates must be unique and non-empty")
    require(isinstance(release, dict), f"{target_id}: releaseGates must be an object")
    for gate_id in owned:
        require(gate_id in known_gates, f"{target_id}: unknown native gate {gate_id}")
    for gate_id, directory in release.items():
        require(
            isinstance(directory, str) and directory and "/" not in directory,
            f"{target_id}: release evidence directory for {gate_id} is invalid",
        )
    require(set(release) == set(owned),
            f"{target_id}: every owned native fixture must be release-gated")

    validate_evidence(target_id, target["evidence"], jobs)


def platform_bounds(target: dict, known_gates: dict) -> dict:
    """Return the target's DECLARED bounds.

    Platforms are declared per target, never derived from the gates: a gate's
    targetOs names where CI executes it (`ios-simulator`, `linux-container`,
    `windows-x86_64-interactive`), which is a runner descriptor and says
    nothing about where a user's application runs.
    """
    bounds = target["evidence"]["bounds"]
    return {
        "platforms": bounds["platforms"],
        "runtime": bounds["runtime"],
        "framework": bounds["framework"],
    }
