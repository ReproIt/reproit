#!/usr/bin/env python3
"""Validate and render Reproit's atomic compatibility contract."""

from __future__ import annotations

import argparse
import importlib.util
import json
import textwrap
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SUPPORT_PATH = ROOT / "validation/support-manifest.json"
GATES_PATH = ROOT / "validation/backends/evidence.json"
STATUS_PATH = ROOT / "validation/compatibility/status.json"
MARKDOWN_PATH = ROOT / "validation/compatibility/STATUS.md"
MAX_BYTES = 1_048_576
MATURITIES = {"stable", "preview", "experimental"}


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


def validate_policy(policy: object) -> dict:
    require(isinstance(policy, dict), "support policy must be an object")
    expected = {
        "stableMinIndependentApplications",
        "affectedReproductionsPerApplication",
        "fixedControlsPerApplication",
        "requireExactIdentity",
        "requireMinimizedTrigger",
        "requireNeighboringLegalBehavior",
        "requireExactCommitNativeEvidence",
    }
    require(set(policy) == expected, "support policy keys do not match schema 2")
    require(policy["stableMinIndependentApplications"] >= 2,
            "stable requires at least two independent applications")
    require(policy["affectedReproductionsPerApplication"] == 3,
            "affected reproduction contract must remain exactly three clean runs")
    require(policy["fixedControlsPerApplication"] == 3,
            "fixed control contract must remain exactly three clean runs")
    for field in expected - {
        "stableMinIndependentApplications",
        "affectedReproductionsPerApplication",
        "fixedControlsPerApplication",
    }:
        require(policy[field] is True, f"support policy {field} must fail closed")
    return policy


def validate_target(
    target_id: str,
    target: object,
    known_gates: dict,
    policy: dict,
) -> None:
    require(isinstance(target, dict), f"{target_id}: target must be an object")
    required = {
        "displayName",
        "family",
        "maturity",
        "scope",
        "ownedGates",
        "releaseGates",
        "promotionBlockers",
    }
    optional = {"fieldBenchmark"}
    require(required <= set(target) <= required | optional,
            f"{target_id}: target keys do not match schema 2")
    for field in ("displayName", "family", "scope"):
        require(isinstance(target[field], str) and target[field].strip(),
                f"{target_id}: {field} must be non-empty")
    require(target["maturity"] in MATURITIES, f"{target_id}: maturity is invalid")

    owned = target["ownedGates"]
    release = target["releaseGates"]
    require(isinstance(owned, list) and len(owned) == len(set(owned)),
            f"{target_id}: ownedGates must be unique")
    require(isinstance(release, dict), f"{target_id}: releaseGates must be an object")
    for gate_id in owned:
        require(gate_id in known_gates, f"{target_id}: unknown native gate {gate_id}")
    require(set(release) <= set(owned),
            f"{target_id}: release gates must be owned by the target")
    for gate_id, directory in release.items():
        require(
            isinstance(directory, str) and directory and "/" not in directory,
            f"{target_id}: release evidence directory for {gate_id} is invalid",
        )

    blockers = target["promotionBlockers"]
    require(
        isinstance(blockers, list)
        and len(blockers) == len(set(blockers))
        and all(isinstance(blocker, str) and blocker for blocker in blockers),
        f"{target_id}: promotionBlockers must be unique non-empty strings",
    )

    benchmark_path = target.get("fieldBenchmark")
    if target["maturity"] != "stable":
        require(blockers, f"{target_id}: non-stable target must name promotion blockers")
        if benchmark_path is not None:
            benchmark = load_json(ROOT / benchmark_path)
            field_validator().validate(benchmark, allow_pending=True)
            require(benchmark["target"] == target_id,
                    f"{target_id}: field benchmark names another target")
        return

    require(not blockers, f"{target_id}: stable target still has promotion blockers")
    require(owned and set(release) == set(owned),
            f"{target_id}: stable target must release-gate every native fixture")
    for gate_id in owned:
        require(
            known_gates[gate_id]["automation"].get("mode") == "required-ci",
            f"{target_id}: stable native gate {gate_id} is not required CI",
        )
    require(isinstance(benchmark_path, str) and benchmark_path,
            f"{target_id}: stable target has no field benchmark")
    benchmark = load_json(ROOT / benchmark_path)
    field_validator().validate(benchmark)
    require(benchmark["target"] == target_id,
            f"{target_id}: field benchmark names another target")
    require(
        len(benchmark["applications"]) >= policy["stableMinIndependentApplications"],
        f"{target_id}: field benchmark is below the independent-application floor",
    )


def validate_support(support: object, gates: object) -> dict:
    require(isinstance(support, dict), "support manifest root must be an object")
    require(set(support) == {"schema", "policy", "comment", "targets"},
            "support manifest keys do not match schema 2")
    require(support["schema"] == 2, "unsupported support manifest schema")
    require(isinstance(support["comment"], str) and support["comment"],
            "support manifest comment must be non-empty")
    policy = validate_policy(support["policy"])

    require(isinstance(gates, dict) and gates.get("schema") == 2,
            "native gate manifest schema is unsupported")
    known_gates = gates.get("gates")
    require(isinstance(known_gates, dict) and known_gates,
            "native gate manifest has no gates")
    targets = support["targets"]
    require(isinstance(targets, dict) and targets, "support manifest has no targets")
    display_names: set[str] = set()
    for target_id in sorted(targets):
        target = targets[target_id]
        require(target_id.replace("-", "").isalnum() and target_id == target_id.lower(),
                f"{target_id}: target id is invalid")
        validate_target(target_id, target, known_gates, policy)
        require(target["displayName"] not in display_names,
                f"{target_id}: displayName is duplicated")
        display_names.add(target["displayName"])
    return policy


def status_document(support: dict) -> dict:
    targets = []
    for target_id in sorted(support["targets"]):
        target = support["targets"][target_id]
        targets.append(
            {
                "id": target_id,
                "displayName": target["displayName"],
                "family": target["family"],
                "maturity": target["maturity"],
                "scope": target["scope"],
                "nativeGates": target["ownedGates"],
                "fieldBenchmark": target.get("fieldBenchmark"),
                "promotionBlockers": target["promotionBlockers"],
            }
        )
    counts = {
        maturity: sum(target["maturity"] == maturity for target in targets)
        for maturity in ("stable", "preview", "experimental")
    }
    return {
        "schemaVersion": 1,
        "policy": support["policy"],
        "counts": counts,
        "targets": targets,
    }


def markdown_status(status: dict) -> str:
    lines = [
        "# Compatibility qualification status",
        "",
        "Generated from `validation/support-manifest.json`. Do not edit by hand.",
        "",
    ]
    for target in status["targets"]:
        lines.extend(
            [
                f"## {target['displayName']}",
                "",
                f"- Maturity: {target['maturity'].title()}",
                *textwrap.wrap(
                    f"- Scope: {target['scope']}",
                    width=100,
                    subsequent_indent="  ",
                ),
                f"- Native gates: {', '.join(target['nativeGates']) or 'missing'}",
                f"- Field benchmark: {target['fieldBenchmark'] or 'incomplete'}",
                "- Promotion blockers:",
            ]
        )
        blockers = target["promotionBlockers"] or ["None"]
        for blocker in blockers:
            lines.extend(
                textwrap.wrap(
                    f"  - {blocker}",
                    width=100,
                    subsequent_indent="    ",
                )
            )
        lines.append("")
    lines.extend(
        [
            "A Stable target requires exact-commit native evidence, two independent",
            "affected-versus-fixed applications, three clean affected reproductions,",
            "three reached-observation fixed controls, exact identity preservation,",
            "verified minimization, and neighboring legal behavior.",
            "",
        ]
    )
    return "\n".join(lines)


def render() -> tuple[str, str]:
    support = load_json(SUPPORT_PATH)
    gates = load_json(GATES_PATH)
    validate_support(support, gates)
    status = status_document(support)
    return (
        json.dumps(status, indent=2, sort_keys=False) + "\n",
        markdown_status(status),
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check-generated", action="store_true")
    args = parser.parse_args()
    status_json, status_markdown = render()
    if args.write:
        STATUS_PATH.write_text(status_json, encoding="utf-8")
        MARKDOWN_PATH.write_text(status_markdown, encoding="utf-8")
    elif args.check_generated:
        require(STATUS_PATH.read_text(encoding="utf-8") == status_json,
                "compatibility status.json is stale; run check.py --write")
        require(MARKDOWN_PATH.read_text(encoding="utf-8") == status_markdown,
                "compatibility STATUS.md is stale; run check.py --write")
    else:
        print(status_json, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
