#!/usr/bin/env python3
"""Validate and render Reproit's atomic compatibility contract.

`validation/support-manifest.json` is the single canonical promotion record.
Every public compatibility surface is generated from it: the status JSON, the
generated status document, the README compatibility table, the promotion
section of `docs/compatibility.md`, and the support claim in `SUPPORT.md`.
Hand-edited prose cannot promote a target.
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
MATURITIES = {"stable", "preview", "experimental"}
STANDARDS = {"schema-2", "schema-3"}
PRODUCTION_TO_LOCAL = {"Unqualified", "FixtureQualified", "IndependentQualified"}
QUALIFICATION_KINDS = {"ci-gate", "evidence", "field-benchmark", "missing"}
BLOCKER_CODES = {
    "incomplete-evidence",
    "unsupported-capability",
    "environment-unreachable",
    "unsafe-to-execute",
    "authority-missing",
    "flaky-within-budget",
    "permission-denied",
    "product-coverage-missing",
}
# Qualification slots every schema-3 Stable promotion must satisfy.
QUALIFICATION_SLOTS = (
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

    A qualification that names a CI gate must name a job and step that really
    exist, so a promotion cannot cite an imaginary check.
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


def validate_policy(policy: object, target_ids: set[str]) -> dict:
    require(isinstance(policy, dict), "support policy must be an object")
    expected = {
        "stableMinIndependentApplications",
        "affectedReproductionsPerApplication",
        "fixedControlsPerApplication",
        "requireExactIdentity",
        "requireMinimizedTrigger",
        "requireNeighboringLegalBehavior",
        "requireExactCommitNativeEvidence",
        "newPromotionStandard",
        "grandfatheredStableTargets",
    }
    require(set(policy) == expected, "support policy keys do not match schema 3")
    require(policy["stableMinIndependentApplications"] >= 2,
            "stable requires at least two independent applications")
    require(policy["affectedReproductionsPerApplication"] == 3,
            "affected reproduction contract must remain exactly three clean runs")
    require(policy["fixedControlsPerApplication"] == 3,
            "fixed control contract must remain exactly three clean runs")
    for field in (
        "requireExactIdentity",
        "requireMinimizedTrigger",
        "requireNeighboringLegalBehavior",
        "requireExactCommitNativeEvidence",
    ):
        require(policy[field] is True, f"support policy {field} must fail closed")
    require(policy["newPromotionStandard"] == "schema-3",
            "every new promotion must use the schema-3 standard")
    grandfathered = policy["grandfatheredStableTargets"]
    require(isinstance(grandfathered, list) and len(grandfathered) == len(set(grandfathered)),
            "grandfatheredStableTargets must be a unique list")
    require(grandfathered == sorted(grandfathered),
            "grandfatheredStableTargets must be sorted")
    require(set(grandfathered) <= target_ids,
            "grandfatheredStableTargets names an unknown target")
    # The grandfathered set is frozen: it records who was promoted under the
    # earlier standard and can only ever shrink.
    require(len(grandfathered) <= 4, "the grandfathered stable set cannot grow")
    return policy


def validate_blockers(target_id: str, blockers: object) -> None:
    require(isinstance(blockers, list), f"{target_id}: blockers must be a list")
    seen = set()
    for index, blocker in enumerate(blockers):
        label = f"{target_id}: blockers[{index}]"
        require(isinstance(blocker, dict), f"{label} must be an object")
        require(set(blocker) == {"code", "detail", "command", "evidence"},
                f"{label} keys must be code, detail, command, evidence")
        require(blocker["code"] in BLOCKER_CODES, f"{label} code {blocker['code']!r} is untyped")
        require(isinstance(blocker["detail"], str) and blocker["detail"].strip(),
                f"{label} detail must be non-empty")
        command = blocker["command"]
        require(command is None or (isinstance(command, str) and command.strip()),
                f"{label} command must be null or the exact failed command")
        evidence = blocker["evidence"]
        require(isinstance(evidence, list), f"{label} evidence must be a list")
        for path in evidence:
            require(isinstance(path, str) and path and not path.startswith("/")
                    and ".." not in path.split("/"),
                    f"{label} evidence path is unsafe")
            require((ROOT / path).exists(), f"{label} evidence is missing: {path}")
        key = (blocker["code"], blocker["detail"])
        require(key not in seen, f"{label} duplicates an earlier blocker")
        seen.add(key)


def validate_qualification(
    target_id: str, slot: str, value: object, jobs: dict[str, set[str]]
) -> None:
    label = f"{target_id}.promotion.{slot}"
    require(isinstance(value, dict), f"{label} must be an object")
    kind = value.get("kind")
    require(kind in QUALIFICATION_KINDS, f"{label} kind is invalid")
    if kind == "missing":
        require(set(value) == {"kind"}, f"{label} missing slot carries no other field")
        return
    if kind == "field-benchmark":
        require(set(value) == {"kind"}, f"{label} field-benchmark slot carries no other field")
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
    label = f"{target_id}.promotion.bounds"
    require(isinstance(bounds, dict) and set(bounds) == {"runtime", "framework"},
            f"{label} keys must be runtime and framework")
    for field in ("runtime", "framework"):
        values = bounds[field]
        require(isinstance(values, list) and values
                and len(values) == len(set(values))
                and all(isinstance(item, str) and item.strip() for item in values),
                f"{label}.{field} must be a unique non-empty list")


def validate_promotion(
    target_id: str,
    target: dict,
    promotion: object,
    policy: dict,
    jobs: dict[str, set[str]],
) -> None:
    require(isinstance(promotion, dict), f"{target_id}: promotion must be an object")
    expected = {
        "standard",
        "fieldBenchmark",
        "cleanCorpus",
        "adversarialCorpus",
        "packageInstall",
        "manualReview",
        "bounds",
        "productionToLocal",
        "blockers",
    }
    require(set(promotion) == expected,
            f"{target_id}: promotion keys do not match schema 3")
    require(promotion["standard"] in STANDARDS, f"{target_id}: promotion standard is invalid")
    grandfathered = set(policy["grandfatheredStableTargets"])
    if promotion["standard"] == "schema-2":
        require(target_id in grandfathered,
                f"{target_id}: only grandfathered targets may use the schema-2 standard")
        require(target["maturity"] == "stable",
                f"{target_id}: the schema-2 standard exists only for already-Stable targets")
    require(promotion["productionToLocal"] in PRODUCTION_TO_LOCAL,
            f"{target_id}: productionToLocal value is invalid")
    validate_bounds(target_id, promotion["bounds"])
    for slot in QUALIFICATION_SLOTS:
        validate_qualification(target_id, slot, promotion[slot], jobs)
    validate_blockers(target_id, promotion["blockers"])

    benchmark_path = promotion["fieldBenchmark"]
    require(benchmark_path is None or (isinstance(benchmark_path, str) and benchmark_path),
            f"{target_id}: fieldBenchmark must be null or a path")

    if target["maturity"] != "stable":
        require(promotion["blockers"],
                f"{target_id}: non-stable target must name typed promotion blockers")
        if benchmark_path is not None:
            benchmark = load_json(ROOT / benchmark_path)
            field_validator().validate(benchmark, allow_pending=True)
            require(benchmark["target"] == target_id,
                    f"{target_id}: field benchmark names another target")
        return

    require(not promotion["blockers"],
            f"{target_id}: stable target still has promotion blockers")
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
    if promotion["standard"] == "schema-2":
        return
    # Schema-3 Stable additionally requires every qualification slot to be
    # satisfied by a real gate or a real retained artifact.
    for slot in QUALIFICATION_SLOTS:
        require(promotion[slot]["kind"] != "missing",
                f"{target_id}: stable target has no {slot} qualification")


def validate_target(
    target_id: str,
    target: object,
    known_gates: dict,
    policy: dict,
    jobs: dict[str, set[str]],
) -> None:
    require(isinstance(target, dict), f"{target_id}: target must be an object")
    required = {
        "displayName",
        "family",
        "maturity",
        "scope",
        "ownedGates",
        "releaseGates",
        "promotion",
    }
    require(set(target) == required, f"{target_id}: target keys do not match schema 3")
    for field in ("displayName", "family", "scope"):
        require(isinstance(target[field], str) and target[field].strip(),
                f"{target_id}: {field} must be non-empty")
    require(target["maturity"] in MATURITIES, f"{target_id}: maturity is invalid")

    owned = target["ownedGates"]
    release = target["releaseGates"]
    require(isinstance(owned, list) and owned and len(owned) == len(set(owned)),
            f"{target_id}: ownedGates must be unique and non-empty")
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

    if target["maturity"] == "stable":
        require(set(release) == set(owned),
                f"{target_id}: stable target must release-gate every native fixture")
        for gate_id in owned:
            require(
                known_gates[gate_id]["automation"].get("mode") == "required-ci",
                f"{target_id}: stable native gate {gate_id} is not required CI",
            )
    validate_promotion(target_id, target, target["promotion"], policy, jobs)


def platform_bounds(target: dict, known_gates: dict) -> dict:
    """Derive OS and architecture bounds from the target's owned native gates."""
    operating_systems: list[str] = []
    architectures: list[str] = []
    for gate_id in target["ownedGates"]:
        gate = known_gates[gate_id]
        if gate["targetOs"] not in operating_systems:
            operating_systems.append(gate["targetOs"])
        for architecture in gate["architectures"]:
            if architecture not in architectures:
                architectures.append(architecture)
    bounds = target["promotion"]["bounds"]
    return {
        "os": sorted(operating_systems),
        "arch": sorted(architectures),
        "runtime": bounds["runtime"],
        "framework": bounds["framework"],
    }


def validate_support(support: object, gates: object) -> dict:
    require(isinstance(support, dict), "support manifest root must be an object")
    require(set(support) == {"schema", "policy", "comment", "targets"},
            "support manifest keys do not match schema 3")
    require(support["schema"] == 3, "unsupported support manifest schema")
    require(isinstance(support["comment"], str) and support["comment"],
            "support manifest comment must be non-empty")
    targets = support["targets"]
    require(isinstance(targets, dict) and targets, "support manifest has no targets")
    policy = validate_policy(support["policy"], set(targets))

    require(isinstance(gates, dict) and gates.get("schema") == 2,
            "native gate manifest schema is unsupported")
    known_gates = gates.get("gates")
    require(isinstance(known_gates, dict) and known_gates,
            "native gate manifest has no gates")
    jobs = workflow_steps()
    display_names: set[str] = set()
    for target_id in sorted(targets):
        target = targets[target_id]
        require(target_id.replace("-", "").isalnum() and target_id == target_id.lower(),
                f"{target_id}: target id is invalid")
        validate_target(target_id, target, known_gates, policy, jobs)
        require(target["displayName"] not in display_names,
                f"{target_id}: displayName is duplicated")
        display_names.add(target["displayName"])
    return policy


def status_document(support: dict) -> dict:
    known_gates = load_json(GATES_PATH)["gates"]
    targets = []
    for target_id in sorted(support["targets"]):
        target = support["targets"][target_id]
        promotion = target["promotion"]
        targets.append(
            {
                "id": target_id,
                "displayName": target["displayName"],
                "family": target["family"],
                "maturity": target["maturity"],
                "scope": target["scope"],
                "nativeGates": target["ownedGates"],
                "promotionStandard": promotion["standard"],
                "fieldBenchmark": promotion["fieldBenchmark"],
                "qualifications": {
                    slot: promotion[slot]["kind"] for slot in QUALIFICATION_SLOTS
                },
                "bounds": platform_bounds(target, known_gates),
                "productionToLocal": promotion["productionToLocal"],
                "promotionBlockers": [
                    {"code": blocker["code"], "detail": blocker["detail"]}
                    for blocker in promotion["blockers"]
                ],
            }
        )
    counts = {
        maturity: sum(target["maturity"] == maturity for target in targets)
        for maturity in ("stable", "preview", "experimental")
    }
    qualification_counts = {
        state: sum(
            target["productionToLocal"] == state for target in targets
        )
        for state in sorted(PRODUCTION_TO_LOCAL)
    }
    return {
        "schemaVersion": 2,
        "policy": support["policy"],
        "counts": counts,
        "productionToLocalCounts": qualification_counts,
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
        bounds = target["bounds"]
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
                f"- Promotion standard: {target['promotionStandard']}",
                f"- Native gates: {', '.join(target['nativeGates']) or 'missing'}",
                f"- Field benchmark: {target['fieldBenchmark'] or 'incomplete'}",
                f"- Production-to-local: {target['productionToLocal']}",
                f"- Operating systems: {', '.join(bounds['os'])}",
                f"- Architectures: {', '.join(bounds['arch'])}",
                f"- Runtimes: {', '.join(bounds['runtime'])}",
                f"- Frameworks: {', '.join(bounds['framework'])}",
                "- Qualifications:",
            ]
        )
        for slot in QUALIFICATION_SLOTS:
            lines.append(f"  - {slot}: {target['qualifications'][slot]}")
        lines.append("- Promotion blockers:")
        blockers = target["promotionBlockers"]
        if not blockers:
            lines.append("  - None")
        for blocker in blockers:
            lines.extend(
                textwrap.wrap(
                    f"  - [{blocker['code']}] {blocker['detail']}",
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
            "verified minimization, and neighboring legal behavior. Targets on the",
            "schema-3 standard additionally require a clean corpus, an adversarial",
            "corpus, a clean package installation, and a confirmed manual review.",
            "",
        ]
    )
    return "\n".join(lines)


def readme_table(status: dict) -> str:
    lines = [
        "| Target | Compatibility | Backend | Production-to-local |",
        "|---|---|---|---|",
    ]
    for target in status["targets"]:
        backend = ", ".join(target["bounds"]["runtime"])
        lines.append(
            f"| {target['displayName']} | {target['maturity'].title()} | "
            f"{backend} | {target['productionToLocal']} |"
        )
    return "\n".join(lines)


def compatibility_section(status: dict) -> str:
    counts = status["counts"]
    lines = [
        f"Stable atomic targets: {counts['stable']}. "
        f"Preview: {counts['preview']}. "
        f"Experimental: {counts['experimental']}.",
        "",
        "| Target | Maturity | Standard | OS | Architectures | Blockers |",
        "|---|---|---|---|---|---|",
    ]
    for target in status["targets"]:
        bounds = target["bounds"]
        lines.append(
            f"| {target['displayName']} | {target['maturity'].title()} | "
            f"{target['promotionStandard']} | {', '.join(bounds['os'])} | "
            f"{', '.join(bounds['arch'])} | {len(target['promotionBlockers'])} |"
        )
    lines.extend(
        [
            "",
            "Every blocker, with its typed code and exact detail, is listed in",
            "[the generated status](../validation/compatibility/STATUS.md).",
        ]
    )
    return "\n".join(lines)


def support_claim(status: dict) -> str:
    stable = [t["displayName"] for t in status["targets"] if t["maturity"] == "stable"]
    preview = [t["displayName"] for t in status["targets"] if t["maturity"] == "preview"]
    qualified = [
        t["displayName"]
        for t in status["targets"]
        if t["productionToLocal"] != "Unqualified"
    ]
    lines = [
        f"Stable ({len(stable)}): " + (", ".join(stable) or "none") + ".",
        "",
        f"Preview ({len(preview)}): " + (", ".join(preview) or "none") + ".",
        "",
        "Production-to-local qualified: " + (", ".join(qualified) or "none") + ".",
        "",
        "Stable is an atomic compatibility claim. It does not by itself claim",
        "that every production occurrence on that target reproduces locally;",
        "that is the separate production-to-local qualification above.",
    ]
    return "\n".join(lines)


def splice(path: Path, name: str, body: str) -> str:
    """Replace the generated block named `name` inside `path`."""
    begin = BEGIN.format(name=name)
    end = END.format(name=name)
    text = path.read_text(encoding="utf-8")
    start = text.find(begin)
    stop = text.find(end)
    require(start != -1 and stop != -1 and start < stop,
            f"{path.relative_to(ROOT)} has no generated block named {name}")
    return f"{text[:start]}{begin}\n\n{body}\n\n{text[stop:]}"


def render() -> dict[str, tuple[Path, str]]:
    support = load_json(SUPPORT_PATH)
    gates = load_json(GATES_PATH)
    validate_support(support, gates)
    status = status_document(support)
    return {
        "status.json": (STATUS_PATH, json.dumps(status, indent=2) + "\n"),
        "STATUS.md": (MARKDOWN_PATH, markdown_status(status)),
        "README.md": (README_PATH, splice(README_PATH, "compatibility", readme_table(status))),
        "docs/compatibility.md": (
            COMPATIBILITY_DOC_PATH,
            splice(COMPATIBILITY_DOC_PATH, "promotion-state", compatibility_section(status)),
        ),
        "SUPPORT.md": (
            SUPPORT_DOC_PATH,
            splice(SUPPORT_DOC_PATH, "support-claim", support_claim(status)),
        ),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check-generated", action="store_true")
    args = parser.parse_args()
    rendered = render()
    if args.write:
        for path, body in rendered.values():
            path.write_text(body, encoding="utf-8")
    elif args.check_generated:
        for name, (path, body) in rendered.items():
            require(path.read_text(encoding="utf-8") == body,
                    f"{name} is stale; run validation/compatibility/check.py --write")
    else:
        print(rendered["status.json"][1], end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
