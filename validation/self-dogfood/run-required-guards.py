#!/usr/bin/env python3
"""Replay every required self-dogfood guard as an explicit strict check."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GUARD_ROOT = Path(".reproit/repros")
RAW_GUARD_ID = re.compile(r"^[a-f0-9]{12}$")
MAX_GUARDS = 1000
REPLAY_TIMEOUT_SECONDS = 30 * 60


class CorpusError(Exception):
    """The committed required-guard corpus is missing or malformed."""


def required_guard_references(repo: Path) -> list[str]:
    guard_root = repo / GUARD_ROOT
    try:
        directories = sorted(path for path in guard_root.iterdir() if path.is_dir())
    except OSError as error:
        raise CorpusError(f"cannot enumerate {GUARD_ROOT}") from error
    if len(directories) > MAX_GUARDS:
        raise CorpusError(f"guard corpus exceeds the {MAX_GUARDS} guard bound")

    required = []
    for directory in directories:
        raw_id = directory.name
        if not RAW_GUARD_ID.fullmatch(raw_id):
            raise CorpusError(f"{directory.relative_to(repo)} is not content-addressed")
        path = directory / "meta.json"
        if not path.is_file():
            raise CorpusError(f"{directory.relative_to(repo)} is missing meta.json")
        try:
            metadata = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise CorpusError(f"cannot read valid JSON from {path.relative_to(repo)}") from error
        if not isinstance(metadata, dict) or metadata.get("id") != raw_id:
            raise CorpusError(f"{path.relative_to(repo)} does not identify its guard directory")
        status = metadata.get("status")
        if status not in {"quarantined", "required"}:
            raise CorpusError(f"{path.relative_to(repo)} has invalid status {status!r}")
        if status == "required":
            required.append(f"rep_{raw_id}")

    if not required:
        raise CorpusError("self-dogfood corpus contains no required guards")
    return required


def replay_required_guards(repo: Path, binary: str, guards: list[str]) -> int:
    failed: list[str] = []
    for guard in guards:
        command = [
            binary,
            "--json",
            "--yes",
            "check",
            guard,
            "--strict",
            "--runs",
            "3",
        ]
        try:
            result = subprocess.run(
                command,
                cwd=repo,
                check=False,
                timeout=REPLAY_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            failed.append(f"{guard} (timed out)")
            continue
        if result.returncode != 0:
            failed.append(f"{guard} (exit {result.returncode})")

    if failed:
        sys.stderr.write(f"required self-dogfood guard failures: {', '.join(failed)}\n")
        return 1
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", help="path to the built Reproit CLI")
    parser.add_argument("--repo", default=str(ROOT), help="repository root")
    arguments = parser.parse_args(argv)
    repo = Path(arguments.repo).resolve()
    try:
        guards = required_guard_references(repo)
        return replay_required_guards(repo, arguments.binary, guards)
    except CorpusError as error:
        sys.stderr.write(f"required self-dogfood guards: {error}\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
