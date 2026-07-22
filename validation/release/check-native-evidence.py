#!/usr/bin/env python3
"""Validate release-native gate results against their captured logs."""

from __future__ import annotations

import argparse
import hashlib
import json
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "validation/backends/evidence.json"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def require_timestamp(gate_id: str, field: str, value: object) -> None:
    if not isinstance(value, str):
        raise ValueError(f"{gate_id}: {field} must be an ISO-8601 timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(
            f"{gate_id}: {field} must be an ISO-8601 timestamp"
        ) from error
    if parsed.tzinfo is None:
        raise ValueError(f"{gate_id}: {field} must include a timezone")


def validate_result(gate_id: str, directory: Path, commit: str) -> dict[str, object]:
    result_path = directory / f"{gate_id}.json"
    log_path = directory / f"{gate_id}.log"
    result = json.loads(result_path.read_text(encoding="utf-8"))
    gates = json.loads(MANIFEST.read_text(encoding="utf-8"))["gates"]
    gate = gates[gate_id]
    required_fields = {
        "schema",
        "gateId",
        "commit",
        "startedAt",
        "finishedAt",
        "executor",
        "targetOs",
        "architectures",
        "fixture",
        "command",
        "status",
        "exitCode",
        "checks",
        "resetStrategy",
        "cleanupStrategy",
        "logSha256",
        "logPath",
    }
    if set(result) != required_fields:
        raise ValueError(f"{gate_id}: result fields do not match schema 1")
    expected = {
        "schema": 1,
        "gateId": gate_id,
        "commit": commit,
        "targetOs": gate["targetOs"],
        "fixture": gate["fixture"],
        "command": gate["command"],
        "status": "passed",
        "exitCode": 0,
        "resetStrategy": gate["resetStrategy"],
        "cleanupStrategy": gate["cleanupStrategy"],
    }
    for field, value in expected.items():
        if result.get(field) != value:
            raise ValueError(
                f"{gate_id}: {field} is {result.get(field)!r}, expected {value!r}"
            )
    require_timestamp(gate_id, "startedAt", result.get("startedAt"))
    require_timestamp(gate_id, "finishedAt", result.get("finishedAt"))
    executor = result.get("executor")
    if not isinstance(executor, dict) or set(executor) != {"os", "architecture"}:
        raise ValueError(f"{gate_id}: executor does not match schema 1")
    if not all(isinstance(value, str) and value for value in executor.values()):
        raise ValueError(f"{gate_id}: executor values must be non-empty strings")
    if Path(str(result.get("logPath"))).name != log_path.name:
        raise ValueError(f"{gate_id}: logPath does not name the captured log")
    checks = result.get("checks")
    if not isinstance(checks, dict) or set(checks) != set(gate["requiredOutput"]):
        raise ValueError(f"{gate_id}: required-output checks do not match the manifest")
    if not all(value is True for value in checks.values()):
        raise ValueError(f"{gate_id}: one or more required-output checks failed")
    architectures = result.get("architectures")
    if not isinstance(architectures, list) or not set(gate["architectures"]).issubset(
        architectures
    ):
        raise ValueError(f"{gate_id}: required architectures were not exercised")
    actual_digest = sha256(log_path)
    if result.get("logSha256") != actual_digest:
        raise ValueError(f"{gate_id}: captured log digest does not match its result")
    captured_log = log_path.read_text(encoding="utf-8", errors="replace")
    missing_markers = [marker for marker in gate["requiredOutput"] if marker not in captured_log]
    if missing_markers:
        raise ValueError(f"{gate_id}: captured log is missing required output markers")
    return {
        "gateId": gate_id,
        "commit": commit,
        "logSha256": actual_digest,
        "executor": result["executor"],
        "architectures": architectures,
        "startedAt": result["startedAt"],
        "finishedAt": result["finishedAt"],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--commit", required=True)
    parser.add_argument("--linux-hosted-dir", type=Path, required=True)
    parser.add_argument("--android-dir", type=Path, required=True)
    parser.add_argument("--flutter-dir", type=Path, required=True)
    parser.add_argument("--macos-dir", type=Path, required=True)
    parser.add_argument("--windows-dir", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if len(args.commit) != 40 or any(ch not in "0123456789abcdef" for ch in args.commit):
        raise ValueError("--commit must be a full lowercase Git commit")
    gate_directories = [
        ("web-chromium", args.linux_hosted_dir),
        ("react-native-android", args.android_dir),
        ("flutter-ios", args.flutter_dir),
        ("macos-ax", args.macos_dir),
        ("windows-uia", args.windows_dir),
    ]
    gates = [
        validate_result(gate_id, directory, args.commit)
        for gate_id, directory in gate_directories
    ]
    output = {"schema": 2, "commit": args.commit, "gates": gates}
    args.out.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
