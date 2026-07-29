#!/usr/bin/env python3
"""Validate and render Reproit's atomic compatibility contract.

`validation/support-manifest.json` is the single canonical promotion record.
Every public compatibility surface is generated from it: the status JSON, the
generated status document, the README supported-platform list, the promotion
section of `docs/compatibility.md`, the all-target stability plan, and the
support claim in `SUPPORT.md`. Hand-edited prose cannot promote a target.
"""

from __future__ import annotations

import argparse
import hashlib
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
STABILITY_PLAN_PATH = ROOT / "validation/compatibility/STABILITY_PLAN.md"
WORKFLOW_PATH = ROOT / ".github/workflows/ci.yml"
README_PATH = ROOT / "README.md"
COMPATIBILITY_DOC_PATH = ROOT / "docs/compatibility.md"
SUPPORT_DOC_PATH = ROOT / "SUPPORT.md"

MAX_BYTES = 1_048_576
MATURITIES = {"stable", "preview", "experimental"}
STANDARDS = {"schema-2", "schema-3"}
PRODUCTION_TO_LOCAL = {"Unqualified", "FixtureQualified", "IndependentQualified"}
PRODUCTION_ORIGINS = {
    "FixtureQualified": "fixture",
    "IndependentQualified": "independent-application",
}
REVISION_PATTERN = re.compile(r"^(?:git:[a-f0-9]{40}|sha256:[a-f0-9]{64})$")
SHA256_PATTERN = re.compile(r"^sha256:[a-f0-9]{64}$")
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

STABILITY_LANES = (
    {
        "name": "Linux architecture matrix",
        "route": (
            "arm64 containers on the local Docker worker; native x86_64 containers and "
            "host checks through `ssh black@zgx-5a09.local`, then `ssh strix`"
        ),
        "targets": (
            "backend-contract",
            "electron-linux",
            "linux-gtk",
            "linux-qt-quick",
            "linux-qt-widgets",
            "linux-wxwidgets",
            "tauri-linux",
            "tui",
            "web-chromium",
            "web-firefox",
            "web-webkit",
        ),
        "prerequisite": (
            "Add a bounded native-x86 pack, execute, collect, and cleanup helper. Run "
            "Docker or Compose on `strix` for contained x86_64 applications. The local "
            "amd64 emulation failure is diagnostic only and cannot defer native Linux."
        ),
    },
    {
        "name": "Android reset-AVD lane",
        "route": "Android Studio SDK, Appium, and UiAutomator2 on a reset installed AVD",
        "targets": (
            "compose-android",
            "flutter-android",
            "react-native-android",
        ),
        "prerequisite": (
            "Record the AVD, API level, architecture, application id, permissions, "
            "network policy, snapshot state, and reset evidence for every campaign."
        ),
    },
    {
        "name": "Apple native lane",
        "route": (
            "Xcode simulators through `xcrun simctl`, Appium, and XCUITest for iOS; "
            "the local macOS host for Accessibility"
        ),
        "targets": (
            "flutter-ios",
            "macos-ax",
            "react-native-ios",
            "swiftui-ios",
        ),
        "prerequisite": (
            "Record simulator or host identity, runtime, architecture, bundle id, "
            "permissions, network policy, boot state, and reset evidence."
        ),
    },
    {
        "name": "Windows native x86_64 lane",
        "route": (
            "`ssh black@zgx-5a09.local`, then `ssh strix`, then the forwarded native "
            "Windows guest via `validation/causal/run-windows-remote.sh`"
        ),
        "targets": (
            "windows-avalonia",
            "windows-winui",
            "windows-wpf",
        ),
        "prerequisite": (
            "Use a fetchable exact commit. Prove the UIA session, process ownership, "
            "readiness, reset, bounded execution, artifact return, and cleanup."
        ),
    },
)


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
    # Every Stable target has completed the schema-3 ratchet. Reintroducing the
    # compatibility escape hatch would silently weaken future promotions.
    require(not grandfathered, "the grandfathered stable set cannot be reintroduced")
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


def non_empty_string(value: object, label: str) -> str:
    require(isinstance(value, str) and value.strip(), f"{label} must be non-empty")
    return value


def validate_revision(value: object, label: str) -> None:
    require(
        isinstance(value, str) and REVISION_PATTERN.fullmatch(value),
        f"{label} must be an exact git or sha256 revision",
    )


def validate_stage_reference(
    value: object,
    label: str,
    stage_ids: set[str],
) -> None:
    require(isinstance(value, dict), f"{label} must be an object")
    require(set(value) == {"command", "evidence"},
            f"{label} keys must be command and evidence")
    non_empty_string(value["command"], f"{label}.command")
    evidence = value["evidence"]
    require(
        isinstance(evidence, list)
        and evidence
        and len(evidence) == len(set(evidence))
        and all(isinstance(item, str) and item in stage_ids for item in evidence),
        f"{label}.evidence must name unique retained stages",
    )


def validate_production_record(
    target_id: str,
    level: str,
    evidence_path: str,
) -> dict:
    label = f"{target_id}.promotion.productionToLocal.evidence"
    path = ROOT / evidence_path
    record = load_json(path)
    require(isinstance(record, dict), f"{label} record must be an object")
    expected = {
        "schemaVersion",
        "gate",
        "targetId",
        "qualification",
        "origin",
        "revisions",
        "cloud",
        "local",
        "execution",
        "stages",
        "missingRequiredStages",
        "qualificationBlockers",
        "chainSha256",
    }
    require(set(record) == expected, f"{label} record keys do not match schema 2")
    require(record["schemaVersion"] == 2, f"{label} record schema is unsupported")
    require(record["gate"] == "D5-production-to-local", f"{label} gate is invalid")
    require(record["targetId"] == target_id, f"{label} names another target")
    require(record["qualification"] == level, f"{label} qualification does not match")

    origin = record["origin"]
    require(isinstance(origin, dict) and set(origin) == {"kind", "summary"},
            f"{label}.origin keys must be kind and summary")
    require(
        origin["kind"] == PRODUCTION_ORIGINS[level],
        f"{label}.origin.kind cannot prove {level}",
    )
    non_empty_string(origin["summary"], f"{label}.origin.summary")

    revisions = record["revisions"]
    require(isinstance(revisions, dict)
            and set(revisions) == {"cli", "sdk", "application"},
            f"{label}.revisions keys must be cli, sdk, and application")
    validate_revision(revisions["cli"], f"{label}.revisions.cli")
    validate_revision(revisions["application"], f"{label}.revisions.application")
    sdk = revisions["sdk"]
    require(isinstance(sdk, dict) and set(sdk) == {"name", "revision"},
            f"{label}.revisions.sdk keys must be name and revision")
    non_empty_string(sdk["name"], f"{label}.revisions.sdk.name")
    validate_revision(sdk["revision"], f"{label}.revisions.sdk.revision")

    cloud = record["cloud"]
    require(
        isinstance(cloud, dict)
        and set(cloud) == {"baseUrl", "projectId", "occurrenceId", "bucketId"},
        f"{label}.cloud keys must bind the service and occurrence identities",
    )
    for field in ("baseUrl", "projectId", "occurrenceId", "bucketId"):
        non_empty_string(cloud[field], f"{label}.cloud.{field}")

    local = record["local"]
    require(isinstance(local, dict) and set(local) == {"provider", "trusted"},
            f"{label}.local keys must be provider and trusted")
    non_empty_string(local["provider"], f"{label}.local.provider")
    require(local["trusted"] is True, f"{label}.local provider must be trusted")

    stages = record["stages"]
    require(isinstance(stages, list) and stages, f"{label}.stages must be non-empty")
    stage_ids: set[str] = set()
    chain_parts = []
    for index, stage in enumerate(stages):
        stage_label = f"{label}.stages[{index}]"
        require(isinstance(stage, dict), f"{stage_label} must be an object")
        base_keys = {"id", "summary", "present", "required"}
        present_keys = base_keys | {
            "file",
            "bytes",
            "malformed",
            "rawSha256",
            "sanitizedSha256",
        }
        absent_keys = (base_keys, base_keys | {"reason"})
        expected_keys = present_keys if stage.get("present") is True else absent_keys
        require(
            set(stage) == expected_keys
            if isinstance(expected_keys, set)
            else set(stage) in expected_keys,
            f"{stage_label} keys do not match a retained stage",
        )
        stage_id = non_empty_string(stage["id"], f"{stage_label}.id")
        require(stage_id not in stage_ids, f"{stage_label}.id is duplicated")
        stage_ids.add(stage_id)
        non_empty_string(stage["summary"], f"{stage_label}.summary")
        require(isinstance(stage["required"], bool), f"{stage_label}.required must be boolean")
        if stage["present"] is not True:
            require(stage["present"] is False, f"{stage_label}.present must be boolean")
            require(stage["required"] is False,
                    f"{stage_label} is a missing required stage")
            if "reason" in stage:
                non_empty_string(stage["reason"], f"{stage_label}.reason")
            continue
        require(stage["malformed"] is False, f"{stage_label} is malformed")
        artifact = non_empty_string(stage["file"], f"{stage_label}.file")
        require(not artifact.startswith("/") and ".." not in artifact.split("/"),
                f"{stage_label}.file is unsafe")
        artifact_path = path.parent / artifact
        require(artifact_path.is_file(), f"{stage_label}.file is missing")
        payload = artifact_path.read_bytes()
        require(len(payload) == stage["bytes"], f"{stage_label}.bytes does not match")
        for field in ("rawSha256", "sanitizedSha256"):
            require(
                isinstance(stage[field], str) and SHA256_PATTERN.fullmatch(stage[field]),
                f"{stage_label}.{field} is invalid",
            )
        sanitized_hash = f"sha256:{hashlib.sha256(payload).hexdigest()}"
        require(
            sanitized_hash == stage["sanitizedSha256"],
            f"{stage_label}.sanitizedSha256 does not match the retained artifact",
        )
        chain_parts.append(f"{stage_id}:{stage['sanitizedSha256']}")

    missing = record["missingRequiredStages"]
    require(missing == [], f"{label} has missing required stages")
    require(record["qualificationBlockers"] == [],
            f"{label} has qualification blockers")
    required_stage_ids = {
        "production-signal",
        "cloud-ingestion",
        "local-materialization",
        "exact-local-reproduction",
        "direct-replay",
        "reset",
        "retention-and-deletion",
    }
    require(required_stage_ids <= stage_ids, f"{label} lacks a required chain stage")

    execution = record["execution"]
    base_execution_keys = {"commands", "reset", "cleanup"}
    web_engines = {
        "web-chromium": "chromium",
        "web-firefox": "firefox",
        "web-webkit": "webkit",
    }
    expected_execution_keys = (
        base_execution_keys | {"adapter"}
        if target_id in web_engines
        else base_execution_keys
    )
    require(
        isinstance(execution, dict) and set(execution) == expected_execution_keys,
        f"{label}.execution keys do not match the target contract",
    )
    if target_id in web_engines:
        adapter = execution["adapter"]
        require(
            isinstance(adapter, dict) and set(adapter) == {"kind", "engine"},
            f"{label}.execution.adapter must bind kind and engine",
        )
        require(
            adapter["kind"] == "playwright",
            f"{label}.execution.adapter.kind must be playwright",
        )
        require(
            adapter["engine"] == web_engines[target_id],
            f"{label}.execution.adapter.engine does not match {target_id}",
        )
    commands = execution["commands"]
    require(isinstance(commands, list) and commands,
            f"{label}.execution.commands must be non-empty")
    command_stages = set()
    for index, command in enumerate(commands):
        command_label = f"{label}.execution.commands[{index}]"
        require(isinstance(command, dict)
                and set(command) == {"stage", "command", "assertions"},
                f"{command_label} keys must be stage, command, and assertions")
        command_stage = non_empty_string(command["stage"], f"{command_label}.stage")
        require(command_stage in stage_ids, f"{command_label}.stage is not retained")
        require(command_stage not in command_stages, f"{command_label}.stage is duplicated")
        command_stages.add(command_stage)
        non_empty_string(command["command"], f"{command_label}.command")
        assertions = command["assertions"]
        require(
            isinstance(assertions, list)
            and assertions
            and all(isinstance(item, str) and item.strip() for item in assertions),
            f"{command_label}.assertions must be non-empty strings",
        )
    require(required_stage_ids <= command_stages,
            f"{label}.execution.commands does not cover every required stage")
    validate_stage_reference(
        execution["reset"],
        f"{label}.execution.reset",
        stage_ids,
    )
    validate_stage_reference(
        execution["cleanup"],
        f"{label}.execution.cleanup",
        stage_ids,
    )

    require(
        isinstance(record["chainSha256"], str)
        and SHA256_PATTERN.fullmatch(record["chainSha256"]),
        f"{label}.chainSha256 is invalid",
    )
    calculated_chain = f"sha256:{hashlib.sha256(chr(10).join(chain_parts).encode()).hexdigest()}"
    require(
        calculated_chain == record["chainSha256"],
        f"{label}.chainSha256 does not match the retained stages",
    )
    return record


def validate_production_to_local(target_id: str, value: object) -> None:
    label = f"{target_id}.promotion.productionToLocal"
    require(isinstance(value, dict), f"{label} must be an evidence binding object")
    require(set(value) == {"level", "evidence"},
            f"{label} keys must be level and evidence")
    level = value["level"]
    require(level in PRODUCTION_TO_LOCAL, f"{label}.level is invalid")
    evidence = value["evidence"]
    if level == "Unqualified":
        require(evidence is None, f"{label} must not cite evidence while Unqualified")
        return
    require(
        isinstance(evidence, str)
        and evidence
        and not evidence.startswith("/")
        and ".." not in evidence.split("/"),
        f"{label}.evidence must be a safe repository-relative path",
    )
    require((ROOT / evidence).is_file(), f"{label}.evidence is missing: {evidence}")
    validate_production_record(target_id, level, evidence)


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
    validate_production_to_local(target_id, promotion["productionToLocal"])
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
        production_to_local = promotion["productionToLocal"]
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
                "productionToLocal": production_to_local["level"],
                "productionToLocalEvidence": production_to_local["evidence"],
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
        "schemaVersion": 3,
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
                (
                    "- Production-to-local evidence: "
                    f"{target['productionToLocalEvidence'] or 'none'}"
                ),
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


def shell_argv(argv: list[str]) -> list[str]:
    """Render checked-in gate argv as a copyable, width-bounded shell command."""
    tokens = [
        value if re.fullmatch(r"[A-Za-z0-9_./:-]+", value) else json.dumps(value)
        for value in argv
    ]
    lines: list[str] = []
    current = ""
    for token in tokens:
        candidate = f"{current} {token}".strip()
        if current and len(candidate) > 88:
            lines.append(f"{current} \\")
            current = f"  {token}"
        else:
            current = candidate
    if current:
        lines.append(current)
    return lines


def stability_plan(status: dict, gates: dict) -> str:
    preview = [target for target in status["targets"] if target["maturity"] != "stable"]
    stable = [target for target in status["targets"] if target["maturity"] == "stable"]
    targets_by_id = {target["id"]: target for target in status["targets"]}
    lane_target_list = [
        target_id
        for lane in STABILITY_LANES
        for target_id in lane["targets"]
    ]
    lane_target_ids = set(lane_target_list)
    require(
        len(lane_target_list) == len(lane_target_ids),
        "stability execution lanes contain a duplicate target",
    )
    require(
        lane_target_ids == set(targets_by_id),
        "stability execution lanes must cover every target exactly once",
    )
    total = len(status["targets"])
    lines = [
        "# All-target stability plan",
        "",
        "Generated from `validation/support-manifest.json` and",
        "`validation/backends/evidence.json`. Do not edit by hand.",
        "",
        "## Program end state",
        "",
        f"This program is complete only when all {total} atomic targets satisfy both axes:",
        "",
        f"- {total} of {total} targets are Stable under schema-3.",
        f"- {total} of {total} targets are `IndependentQualified` for production-to-local.",
        "- No target has a typed promotion blocker or a missing qualification slot.",
        "- The grandfathered schema-2 set is empty.",
        "- Every native result, field campaign, and production chain names one exact commit.",
        "- Generated compatibility surfaces agree with the canonical manifest.",
        "",
        "Stable and production-to-local remain independent claims. Reaching Stable does not",
        "silently grant production qualification, and a fixture replay never counts as an",
        "independent application replay.",
        "",
        "## Stable completion contract",
        "",
        "A target becomes Stable only when the manifest validator can prove all of these:",
        "",
        "1. Every owned native gate is required CI and passes at the exact CLI commit.",
        "2. Two independent affected-versus-fixed application campaigns are retained.",
        "3. Each application has three clean affected reproductions with one exact identity.",
        "4. Each application has three reached-observation fixed controls.",
        "5. Minimization and neighboring legal behavior are verified.",
        "6. Clean and adversarial corpora, package installation, and manual review are retained.",
        "7. Every typed blocker is removed because evidence closes it, never by prose alone.",
        "",
        "## Qualification evidence contract",
        "",
        "Before changing any `productionToLocal` value, extend the manifest schema so the value",
        "is derived from a retained evidence record instead of a manually editable string.",
        "Each record must bind the target id, qualification level, exact CLI and SDK commits,",
        "application revision, origin type, Cloud occurrence identity, trusted local provider,",
        "input and artifact hashes, replay command, behavioral assertion, reset, and cleanup.",
        "",
        "The qualification levels have distinct gates:",
        "",
        "1. `FixtureQualified`: run a disposable SDK-to-Cloud-to-trusted-local chain from a",
        "   controlled fixture, retain every stage, assert exact local behavior, and clean up.",
        "2. `IndependentQualified`: repeat the complete chain from a real independent affected",
        "   application occurrence for the exact target. A renamed or modified built-in fixture",
        "   does not qualify.",
        "",
        "`validation/cloud/run-production-loop.sh` is the web fixture reference harness. It can",
        "prove `FixtureQualified` only for the target it actually exercises. Add a bounded",
        "target adapter, or an equally strict target-specific harness, before using the workflow",
        "for mobile, desktop, terminal, or backend targets.",
        "",
        "## Shared prerequisites and dependency order",
        "",
        "1. Freeze one reviewable candidate commit. Update Cloud, SDK, package, and deployment",
        "   pins to that exact commit before collecting promotion evidence.",
        "2. Add evidence-backed qualification fields and validators before changing any",
        "   `productionToLocal` state.",
        "3. Close runner gaps per lane. Every runner must prove process ownership, readiness,",
        "   reset, containment, bounded work, artifact retention, and cleanup on every exit.",
        "4. Ratchet the existing Stable targets from schema-2 to schema-3. Keep them Stable only",
        "   if the exact-commit gates and new corpus evidence pass.",
        "5. Run owned native gates and field campaigns by lane, but validate and promote each",
        "   target independently. Never use one framework's evidence for a neighboring target.",
        "6. For every Stable target, retain a target-specific fixture production chain and advance",
        "   only that target to `FixtureQualified`.",
        "7. For every fixture-qualified target, retain a real independent application chain and",
        "   advance only that target to `IndependentQualified`.",
        "8. Regenerate all public surfaces and run the final all-target audit.",
        "",
        "## Execution lanes",
        "",
    ]
    for lane in STABILITY_LANES:
        target_names = [
            targets_by_id[target_id]["displayName"]
            for target_id in lane["targets"]
        ]
        lines.extend(
            [
                f"### {lane['name']}",
                "",
                *textwrap.wrap(
                    f"- Route: {lane['route']}",
                    width=100,
                    subsequent_indent="  ",
                ),
                *textwrap.wrap(
                    f"- Targets: {', '.join(target_names)}",
                    width=100,
                    subsequent_indent="  ",
                ),
                *textwrap.wrap(
                    f"- Lane prerequisite: {lane['prerequisite']}",
                    width=100,
                    subsequent_indent="  ",
                ),
                "- Exit gate: every target-specific native command passes at the candidate",
                "  commit, and retained reset and cleanup evidence validates.",
                "",
            ]
        )
    lines.extend(
        [
            "## Existing Stable ratchet",
            "",
            "These targets are not finished merely because they are already Stable. They must pass",
            "the current exact-commit gates, move to schema-3, and complete both production",
            "qualification levels.",
            "",
        ]
    )
    for target in stable:
        missing = [
            name for name, kind in target["qualifications"].items() if kind == "missing"
        ]
        lines.append(
            f"- {target['displayName']}: "
            + (
                f"add {', '.join(missing)} and move to schema-3"
                if missing
                else "already satisfies every recorded qualification slot"
            )
        )
    lines.extend(
        [
            "",
            "## Preview target worklists",
            "",
            "Execute these worklists in lane order. A target leaves this section only when its",
            "complete target-specific record validates.",
            "",
        ]
    )
    for target in preview:
        lines.extend(
            [
                f"### {target['displayName']}",
                "",
                f"- Target id: `{target['id']}`",
                f"- Current maturity: {target['maturity'].title()}",
                f"- Environment: {', '.join(target['bounds']['os'])}; "
                f"{', '.join(target['bounds']['arch'])}",
                f"- Runtime bound: {', '.join(target['bounds']['runtime'])}",
                f"- Framework bound: {', '.join(target['bounds']['framework'])}",
                "- Native gates:",
            ]
        )
        for gate_id in target["nativeGates"]:
            gate = gates[gate_id]
            automation = gate["automation"]
            route = automation.get("route")
            suffix = f"; route: {route}" if route else ""
            lines.extend(
                textwrap.wrap(
                    f"  - `{gate_id}`: {automation['mode']} in "
                    f"{automation['workflow']} job `{automation['job']}`{suffix}",
                    width=100,
                    subsequent_indent="    ",
                )
            )
            lines.extend(
                [
                    "    ```sh",
                    *(f"    {line}" for line in shell_argv(gate["command"])),
                    "    ```",
                ]
            )
        lines.append(
            f"- Field benchmark: `{target['fieldBenchmark']}`"
            if target["fieldBenchmark"]
            else f"- Field benchmark to create: `validation/field/{target['id']}.json`"
        )
        lines.append("- Open blockers:")
        for blocker in target["promotionBlockers"]:
            lines.extend(
                textwrap.wrap(
                    f"  - [{blocker['code']}] {blocker['detail']}",
                    width=100,
                    subsequent_indent="    ",
                )
            )
        lines.extend(
            [
                "- Promotion gate:",
                f"  - Set `{target['id']}.maturity` to `stable` only after the benchmark,",
                "    qualification slots, required-CI gates, and blockers validate together.",
                "- Qualification dependency:",
                "  - After Stable, run the target-specific fixture chain and then a distinct",
                "    independent application chain. Retain and validate both records.",
                "",
            ]
        )
    lines.extend(
        [
            "## All-target production-to-local qualification",
            "",
            "The following checklist covers every target, including the targets that were Stable",
            "when this plan was generated. Each row is complete only at `IndependentQualified`.",
            "",
            "| Target | Current maturity | Current qualification | Required end state |",
            "|---|---|---|---|",
        ]
    )
    for target in status["targets"]:
        lines.append(
            f"| `{target['id']}` | {target['maturity'].title()} | "
            f"{target['productionToLocal']} | Stable + `IndependentQualified` |"
        )
    lines.extend(
        [
            "",
            "For each row, use this atomic sequence:",
            "",
            "1. Confirm the target's schema-3 Stable record at the candidate commit.",
            "2. Run and retain the target-specific controlled fixture chain.",
            "3. Validate the record and advance only that target to `FixtureQualified`.",
            "4. Capture a real occurrence from an independent application at a pinned revision.",
            "5. Ingest it through the SDK into a disposable isolated Cloud project.",
            "6. Replay it locally with the declared trusted provider and assert exact behavior.",
            "7. Retain immutable stage hashes plus reset and cleanup proof.",
            "8. Validate the independent record and advance only that target to",
            "   `IndependentQualified`.",
            "",
            "## Final audit",
            "",
            f"- Assert Stable count: {total}.",
            f"- Assert `IndependentQualified` count: {total}.",
            "- Assert Preview and Experimental counts: zero.",
            "- Assert blocker count, missing qualification slots, and schema-2 targets: zero.",
            "- Re-run every required-CI and native gate at the same exact commit.",
            "- Review retained evidence for target identity, independent origin, hashes, reset,",
            "  containment, and cleanup before publishing the generated claims.",
            "",
            "## Required validation",
            "",
            "```sh",
            "python3 validation/compatibility/check.py --write",
            "python3 validation/compatibility/check.py --check-generated",
            "cargo fmt --all --check",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "cargo test --workspace --locked",
            "```",
            "",
            "Then run every target's native command above on its declared environment and retain",
            "the exact-commit evidence required by `validation/release/check-native-evidence.py`.",
            "",
        ]
    )
    return "\n".join(lines)


def readme_platforms(status: dict) -> str:
    return "\n".join(
        f"- {target['displayName']}"
        for target in status["targets"]
    )


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
    gate_document = load_json(GATES_PATH)
    validate_support(support, gate_document)
    status = status_document(support)
    return {
        "status.json": (STATUS_PATH, json.dumps(status, indent=2) + "\n"),
        "STATUS.md": (MARKDOWN_PATH, markdown_status(status)),
        "STABILITY_PLAN.md": (
            STABILITY_PLAN_PATH,
            stability_plan(status, gate_document["gates"]),
        ),
        "README.md": (README_PATH, splice(README_PATH, "platforms", readme_platforms(status))),
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
