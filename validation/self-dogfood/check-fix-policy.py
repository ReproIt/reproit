#!/usr/bin/env python3
"""Enforce the Reproit self-dogfood bug-fix declaration policy (gate D4).

Every commit that changes production source must declare, in exactly one
``Reproit-Dogfood:`` trailer, how its defect is proven:

  guard:<rep_id>              a committed required guard with affected/fixed
                              evidence
  exception:<code>:<id>       a typed eligibility exception with a retained
                              evidence record
  no-repro:<test path>        no stable automated reproduction is practical,
                              plus an independent regression test changed in
                              the same range
  not-a-fix                   the change is not a bug fix

A missing declaration is a failure, never an implicit exception.

The gate also refuses any change that weakens the required guard corpus: a
guard removed, renamed, or downgraded out of ``required`` must carry a
``Reproit-Guard-Retire:`` trailer and a retained retirement record.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXCEPTIONS = Path("validation/self-dogfood/exceptions")
RETIREMENTS = Path("validation/self-dogfood/retirements")
GUARD_ROOT = ".reproit/repros"

# Directories whose contents can carry a defect. A change here always needs a
# declaration; docs, workflows, and retained evidence do not.
SOURCE_PREFIXES = (
    "crates/",
    "src/",
    "sdk/",
    "runners/",
    "scripts/",
)
SOURCE_SUFFIXES = (".rs", ".ts", ".tsx", ".js", ".mjs", ".py", ".swift", ".kt", ".dart")

MAX_COMMITS = 200
MAX_FILES_PER_COMMIT = 5000
DECLARATION = re.compile(r"^Reproit-Dogfood:\s*(.+?)\s*$", re.MULTILINE)
RETIRE = re.compile(r"^Reproit-Guard-Retire:\s*(rep_[a-f0-9]{12})\s*$", re.MULTILINE)
REPRO_ID = re.compile(r"^rep_[a-f0-9]{12}$")
RAW_GUARD_ID = re.compile(r"^[a-f0-9]{12}$")
EXCEPTION_ID = re.compile(r"^[a-z0-9][a-z0-9-]{0,63}$")
BLOCKER_CODES = frozenset(
    {
        "incomplete-evidence",
        "unsupported-capability",
        "environment-unreachable",
        "unsafe-to-execute",
        "authority-missing",
        "flaky-within-budget",
    }
)


class PolicyError(Exception):
    """A declaration, guard, or retained record failed the policy."""


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        check=False,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        raise PolicyError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout


def commits(repo: Path, base: str, head: str) -> list[str]:
    listed = git(repo, "rev-list", "--no-merges", f"{base}..{head}").split()
    if len(listed) > MAX_COMMITS:
        raise PolicyError(
            f"range {base}..{head} has {len(listed)} commits, above the "
            f"{MAX_COMMITS} bound"
        )
    return listed


def changed_files(repo: Path, commit: str) -> list[str]:
    listed = git(
        repo, "diff-tree", "--no-commit-id", "--name-only", "-r", commit
    ).splitlines()
    if len(listed) > MAX_FILES_PER_COMMIT:
        raise PolicyError(f"{commit[:12]} changes {len(listed)} files, above the bound")
    return [line for line in listed if line]


def touches_source(paths: list[str]) -> bool:
    for path in paths:
        if path.startswith(SOURCE_PREFIXES) and path.endswith(SOURCE_SUFFIXES):
            return True
    return False


def declarations(message: str) -> list[str]:
    return DECLARATION.findall(message)


def guard_is_required(repo: Path, ref: str, guard_id: str) -> bool:
    """Read a guard's meta.json at ``ref`` and report whether it is required."""
    raw = guard_id.removeprefix("rep_")
    path = f"{GUARD_ROOT}/{raw}/meta.json"
    try:
        blob = git(repo, "show", f"{ref}:{path}")
    except PolicyError:
        return False
    try:
        meta = json.loads(blob)
    except json.JSONDecodeError as error:
        raise PolicyError(f"{path} at {ref} is not valid JSON") from error
    return meta.get("status") == "required" and meta.get("id") == raw


def required_guards(repo: Path, ref: str) -> dict[str, str]:
    """Map required guard ids to their trigger signature at ``ref``."""
    listing = git(repo, "ls-tree", "-r", "--name-only", ref, GUARD_ROOT).splitlines()
    guards: dict[str, str] = {}
    for path in listing:
        parts = path.split("/")
        if len(parts) != 4 or parts[3] != "meta.json":
            continue
        raw = parts[2]
        if not RAW_GUARD_ID.match(raw):
            raise PolicyError(f"{path} at {ref} is not a content-addressed guard")
        meta = json.loads(git(repo, "show", f"{ref}:{path}"))
        if meta.get("status") == "required":
            guards[f"rep_{raw}"] = str(meta.get("trigger_sig", ""))
    return guards


def load_record(repo: Path, ref: str, path: Path, label: str) -> dict[str, object]:
    try:
        blob = git(repo, "show", f"{ref}:{path.as_posix()}")
    except PolicyError as error:
        raise PolicyError(f"{label} record {path} is missing at {ref}") from error
    try:
        record = json.loads(blob)
    except json.JSONDecodeError as error:
        raise PolicyError(f"{label} record {path} is not valid JSON") from error
    if not isinstance(record, dict):
        raise PolicyError(f"{label} record {path} must be an object")
    return record


def require_text(record: dict[str, object], field: str, path: Path) -> str:
    value = record.get(field)
    if not isinstance(value, str) or not value.strip() or len(value) > 4096:
        raise PolicyError(f"{path}: {field} must be bounded non-empty text")
    return value


def validate_exception(repo: Path, ref: str, code: str, identifier: str) -> None:
    if code not in BLOCKER_CODES:
        raise PolicyError(
            f"exception code {code!r} is not one of {sorted(BLOCKER_CODES)}"
        )
    if not EXCEPTION_ID.match(identifier):
        raise PolicyError(f"exception id {identifier!r} is not a safe slug")
    path = EXCEPTIONS / f"{identifier}.json"
    record = load_record(repo, ref, path, "exception")
    if record.get("schemaVersion") != 1:
        raise PolicyError(f"{path}: schemaVersion must equal 1")
    if record.get("id") != identifier:
        raise PolicyError(f"{path}: id must equal {identifier}")
    if record.get("code") != code:
        raise PolicyError(f"{path}: code must equal the declared {code}")
    require_text(record, "detail", path)
    require_text(record, "issue", path)
    require_text(record, "missingCapability", path)
    evidence = record.get("retainedEvidence")
    if not isinstance(evidence, list) or not evidence:
        raise PolicyError(f"{path}: retainedEvidence must be a non-empty list")
    for entry in evidence:
        if not isinstance(entry, str) or not entry or entry.startswith("/"):
            raise PolicyError(f"{path}: retainedEvidence holds a bad path")


def validate_no_repro(repo: Path, commit: str, test_path: str, files: list[str]) -> None:
    if test_path.startswith("/") or ".." in test_path.split("/"):
        raise PolicyError(f"no-repro test path {test_path!r} is not relative")
    if test_path not in files:
        raise PolicyError(
            f"{commit[:12]}: no-repro declares {test_path}, which the commit "
            "does not change; the independent regression test must land with "
            "the fix"
        )
    try:
        git(repo, "show", f"{commit}:{test_path}")
    except PolicyError as error:
        raise PolicyError(f"{commit[:12]}: {test_path} does not exist") from error


def validate_declaration(
    repo: Path, commit: str, declaration: str, files: list[str]
) -> dict[str, str]:
    if declaration == "not-a-fix":
        return {"kind": "not-a-fix"}
    kind, _, rest = declaration.partition(":")
    if kind == "guard":
        if not REPRO_ID.match(rest):
            raise PolicyError(f"{commit[:12]}: {rest!r} is not a repro id")
        if not guard_is_required(repo, commit, rest):
            raise PolicyError(
                f"{commit[:12]}: guard {rest} is not a required guard in this "
                "commit's tree"
            )
        return {"kind": "guard", "guard": rest}
    if kind == "exception":
        code, _, identifier = rest.partition(":")
        validate_exception(repo, commit, code, identifier)
        return {"kind": "exception", "code": code, "id": identifier}
    if kind == "no-repro":
        validate_no_repro(repo, commit, rest, files)
        return {"kind": "no-repro", "test": rest}
    raise PolicyError(
        f"{commit[:12]}: unknown declaration {declaration!r}; use guard:, "
        "exception:, no-repro:, or not-a-fix"
    )


def validate_retirements(repo: Path, base: str, head: str, retired: set[str]) -> list[str]:
    before = required_guards(repo, base)
    after = required_guards(repo, head)
    weakened = []
    for guard_id, signature in before.items():
        if guard_id not in after:
            weakened.append(f"{guard_id} is no longer a required guard")
        elif after[guard_id] != signature:
            weakened.append(
                f"{guard_id} changed trigger signature from {signature!r} to "
                f"{after[guard_id]!r}"
            )
    unresolved = []
    for message in weakened:
        guard_id = message.split(" ", 1)[0]
        if guard_id not in retired:
            unresolved.append(message)
            continue
        path = RETIREMENTS / f"{guard_id}.json"
        record = load_record(repo, head, path, "retirement")
        if record.get("schemaVersion") != 1 or record.get("guard") != guard_id:
            raise PolicyError(f"{path}: schemaVersion 1 and guard {guard_id} required")
        require_text(record, "reason", path)
        require_text(record, "replacement", path)
    if unresolved:
        raise PolicyError(
            "the required guard corpus was weakened without a retirement "
            "declaration: " + "; ".join(unresolved)
        )
    return weakened


def review(repo: Path, base: str, head: str) -> dict[str, object]:
    listed = commits(repo, base, head)
    results = []
    retired: set[str] = set()
    for commit in listed:
        message = git(repo, "show", "--no-patch", "--format=%B", commit)
        retired.update(RETIRE.findall(message))
        files = changed_files(repo, commit)
        if not touches_source(files):
            results.append({"commit": commit, "declaration": {"kind": "no-source"}})
            continue
        found = declarations(message)
        if len(found) != 1:
            raise PolicyError(
                f"{commit[:12]}: expected exactly one Reproit-Dogfood trailer, "
                f"found {len(found)}. A missing declaration is not an implicit "
                "exception."
            )
        results.append(
            {
                "commit": commit,
                "declaration": validate_declaration(repo, commit, found[0], files),
            }
        )
    weakened = validate_retirements(repo, base, head, retired)
    return {
        "schemaVersion": 1,
        "gate": "self-dogfood-fix-policy",
        "base": base,
        "head": head,
        "commits": len(listed),
        "declared": [entry for entry in results if entry["declaration"]["kind"] != "no-source"],
        "retiredGuards": sorted(retired),
        "weakenedGuards": weakened,
    }


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True, help="merge base or release ref")
    parser.add_argument("--head", default="HEAD", help="candidate ref")
    parser.add_argument("--repo", default=str(ROOT), help="repository root")
    arguments = parser.parse_args(argv)
    try:
        report = review(Path(arguments.repo).resolve(), arguments.base, arguments.head)
    except PolicyError as error:
        sys.stderr.write(f"self-dogfood fix policy: {error}\n")
        return 1
    json.dump(report, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
