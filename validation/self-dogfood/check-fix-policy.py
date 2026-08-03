#!/usr/bin/env python3
"""Enforce the Reproit self-dogfood bug-fix declaration policy (gate D4).

Every commit that changes production source must declare, in exactly one
``Reproit-Dogfood:`` trailer, how its defect is proven:

  guard:<rep_id>              a committed required guard with affected/fixed
                              evidence
  exception:<code>:<id>       a typed eligibility exception with a retained
                              evidence record
  no-repro:<id>               no stable automated reproduction is practical;
                              a bounded independent regression test must fail
                              on the parent and pass on the fix
  not-a-fix:<id>              an evidence-backed record proves the change is
                              a feature, refactor, maintenance, or tooling work

A missing declaration is a failure, never an implicit exception.

The gate also refuses any change that weakens the required guard corpus: a
guard removed, renamed, or downgraded out of ``required`` must carry a
``Reproit-Guard-Retire:`` trailer and a retained retirement record.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXCEPTIONS = Path("validation/self-dogfood/exceptions")
NO_REPRO = Path("validation/self-dogfood/no-repro")
NOT_A_FIX = Path("validation/self-dogfood/not-a-fix")
RETIREMENTS = Path("validation/self-dogfood/retirements")
GUARD_ROOT = ".reproit/repros"

# Directories and root build files whose contents can carry a defect. This list
# deliberately includes workflow and packaging code: a broken gate or release
# recipe is a product defect even when no runtime source file changed.
SOURCE_PREFIXES = (
    ".github/actions/",
    ".github/workflows/",
    "crates/",
    "runners/",
    "scripts/",
    "sdk/",
    "src/",
    "validation/",
)
NON_SOURCE_PREFIXES = (
    "validation/field/evidence/",
    "validation/self-dogfood/evidence/",
    "validation/self-dogfood/exceptions/",
    "validation/self-dogfood/no-repro/",
    "validation/self-dogfood/not-a-fix/",
    "validation/self-dogfood/retirements/",
)
NON_SOURCE_FILES = frozenset(
    {
        "validation/compatibility/STATUS.md",
        "validation/compatibility/status.json",
    }
)
SOURCE_FILES = frozenset(
    {
        "Cargo.lock",
        "Cargo.toml",
        "Dockerfile",
        "Makefile",
        "build.gradle",
        "build.gradle.kts",
        "go.mod",
        "go.sum",
        "package-lock.json",
        "package.json",
        "pnpm-lock.yaml",
        "pyproject.toml",
        "settings.gradle",
        "settings.gradle.kts",
        "uv.lock",
    }
)
SOURCE_SUFFIXES = (
    ".bash",
    ".c",
    ".cc",
    ".cpp",
    ".cs",
    ".css",
    ".cxx",
    ".dart",
    ".gql",
    ".go",
    ".gradle",
    ".graphql",
    ".h",
    ".hh",
    ".hpp",
    ".html",
    ".java",
    ".js",
    ".json",
    ".jsx",
    ".kt",
    ".kts",
    ".m",
    ".mjs",
    ".mm",
    ".php",
    ".proto",
    ".ps1",
    ".py",
    ".rb",
    ".rs",
    ".sh",
    ".swift",
    ".toml",
    ".ts",
    ".tsx",
    ".xml",
    ".yaml",
    ".yml",
)

MAX_COMMITS = 200
MAX_FILES_PER_COMMIT = 5000
MAX_TEST_ARGUMENTS = 32
MAX_TEST_TIMEOUT_SECONDS = 300
DECLARATION = re.compile(r"^Reproit-Dogfood:\s*(.+?)\s*$", re.MULTILINE)
RETIRE = re.compile(r"^Reproit-Guard-Retire:\s*(rep_[a-f0-9]{12})\s*$", re.MULTILINE)
REPRO_ID = re.compile(r"^rep_[a-f0-9]{12}$")
RAW_GUARD_ID = re.compile(r"^[a-f0-9]{12}$")
EXCEPTION_ID = re.compile(r"^[a-z0-9][a-z0-9-]{0,63}$")
SHA256 = re.compile(r"^sha256:[a-f0-9]{64}$")
NOT_A_FIX_TYPES = frozenset({"feature", "maintenance", "refactor", "tooling"})
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


def git_bytes(repo: Path, *args: str) -> bytes:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        check=False,
        timeout=120,
    )
    if result.returncode != 0:
        error = result.stderr.decode("utf-8", errors="replace").strip()
        raise PolicyError(f"git {' '.join(args)} failed: {error}")
    return result.stdout


def commits(repo: Path, base: str, head: str) -> list[str]:
    # A force-push hands CI a `before` sha that no longer exists (the amended
    # commit orphaned it). The declarations of reachable history were already
    # gated when they first landed, so the honest fallback is to gate the head
    # commit alone rather than fail on an unevaluable range.
    base_reachable = subprocess.run(
        ["git", "-C", str(repo), "cat-file", "-e", f"{base}^{{commit}}"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if base_reachable.returncode != 0:
        print(
            f"self-dogfood fix policy: base {base} is unreachable "
            "(force-push); gating the head commit only",
            file=sys.stderr,
        )
        return [git(repo, "rev-parse", head).strip()]
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
        if path in NON_SOURCE_FILES or path.startswith(NON_SOURCE_PREFIXES):
            continue
        name = Path(path).name
        if "/" not in path and name in SOURCE_FILES:
            return True
        if path.startswith(SOURCE_PREFIXES) and (
            path.endswith(SOURCE_SUFFIXES) or name in SOURCE_FILES
        ):
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


def require_safe_path(value: object, label: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or value.startswith("/")
        or ".." in value.split("/")
        or len(value) > 4096
    ):
        raise PolicyError(f"{label} must be a bounded safe relative path")
    return value


def validate_evidence(
    repo: Path,
    ref: str,
    evidence: object,
    record_path: Path,
    field: str,
) -> list[str]:
    if not isinstance(evidence, list) or not evidence:
        raise PolicyError(f"{record_path}: {field} must be a non-empty list")
    paths = []
    for index, entry in enumerate(evidence):
        if not isinstance(entry, dict) or set(entry) != {"path", "sha256"}:
            raise PolicyError(
                f"{record_path}: {field}[{index}] must contain path and sha256"
            )
        evidence_path = require_safe_path(
            entry.get("path"), f"{record_path}: {field}[{index}].path"
        )
        expected = entry.get("sha256")
        if not isinstance(expected, str) or not SHA256.fullmatch(expected):
            raise PolicyError(
                f"{record_path}: {field}[{index}].sha256 must be sha256:<64 hex>"
            )
        try:
            blob = git_bytes(repo, "show", f"{ref}:{evidence_path}")
        except PolicyError as error:
            raise PolicyError(
                f"{record_path}: retained evidence {evidence_path} is missing at {ref}"
            ) from error
        actual = f"sha256:{hashlib.sha256(blob).hexdigest()}"
        if actual != expected:
            raise PolicyError(
                f"{record_path}: retained evidence {evidence_path} digest "
                f"is {actual}, expected {expected}"
            )
        paths.append(evidence_path)
    return paths


def validate_exception(
    repo: Path,
    ref: str,
    code: str,
    identifier: str,
    range_files: set[str] | None = None,
) -> None:
    if code not in BLOCKER_CODES:
        raise PolicyError(
            f"exception code {code!r} is not one of {sorted(BLOCKER_CODES)}"
        )
    if not EXCEPTION_ID.match(identifier):
        raise PolicyError(f"exception id {identifier!r} is not a safe slug")
    path = EXCEPTIONS / f"{identifier}.json"
    record = load_record(repo, ref, path, "exception")
    # An exception is the only declaration kind with nothing tying it to the
    # change that cites it: a guard must be required in that tree, a no-repro
    # test is executed and must fail at the parent, and a not-a-fix record must
    # change with its declaration. Without this check a commit can satisfy the
    # gate by pointing at any exception record that already exists, which is a
    # syntactically valid and semantically false declaration. Requiring the
    # record to be touched somewhere in the reviewed range keeps a legitimate
    # follow-up in the same push able to cite it, while refusing a citation of
    # an unrelated record from history.
    if range_files is not None and path.as_posix() not in range_files:
        raise PolicyError(
            f"{ref[:12]}: exception record {path} is not touched by this "
            "range, so the declaration is not bound to this change. Write the "
            "record with the commit that cites it, or cite a different kind."
        )
    if record.get("schemaVersion") != 1:
        raise PolicyError(f"{path}: schemaVersion must equal 1")
    if record.get("id") != identifier:
        raise PolicyError(f"{path}: id must equal {identifier}")
    if record.get("code") != code:
        raise PolicyError(f"{path}: code must equal the declared {code}")
    require_text(record, "detail", path)
    require_text(record, "issue", path)
    require_text(record, "missingCapability", path)
    validate_evidence(repo, ref, record.get("retainedEvidence"), path, "retainedEvidence")


def validate_not_a_fix(
    repo: Path, commit: str, identifier: str, files: list[str]
) -> dict[str, str]:
    if not EXCEPTION_ID.fullmatch(identifier):
        raise PolicyError(f"not-a-fix id {identifier!r} is not a safe slug")
    path = NOT_A_FIX / f"{identifier}.json"
    if path.as_posix() not in files:
        raise PolicyError(
            f"{commit[:12]}: not-a-fix record {path} must change with the declaration"
        )
    record = load_record(repo, commit, path, "not-a-fix")
    if record.get("schemaVersion") != 1 or record.get("id") != identifier:
        raise PolicyError(f"{path}: schemaVersion 1 and id {identifier} required")
    change_type = record.get("changeType")
    if change_type not in NOT_A_FIX_TYPES:
        raise PolicyError(f"{path}: changeType must be one of {sorted(NOT_A_FIX_TYPES)}")
    require_text(record, "detail", path)
    evidence = validate_evidence(
        repo, commit, record.get("evidence"), path, "evidence"
    )
    if not any(evidence_path in files for evidence_path in evidence):
        raise PolicyError(
            f"{path}: at least one evidence artifact must change in the same commit"
        )
    return {"kind": "not-a-fix", "id": identifier, "changeType": str(change_type)}


def validate_test_command(record: dict[str, object], path: Path) -> tuple[list[str], int]:
    command = record.get("command")
    if (
        not isinstance(command, list)
        or not command
        or len(command) > MAX_TEST_ARGUMENTS
        or any(
            not isinstance(argument, str)
            or not argument
            or len(argument) > 4096
            for argument in command
        )
    ):
        raise PolicyError(
            f"{path}: command must contain 1..{MAX_TEST_ARGUMENTS} bounded arguments"
        )
    timeout = record.get("timeoutSeconds")
    if (
        not isinstance(timeout, int)
        or isinstance(timeout, bool)
        or not 1 <= timeout <= MAX_TEST_TIMEOUT_SECONDS
    ):
        raise PolicyError(
            f"{path}: timeoutSeconds must be 1..{MAX_TEST_TIMEOUT_SECONDS}"
        )
    return list(command), timeout


def execute_test_at_commit(
    repo: Path,
    commit: str,
    command: list[str],
    timeout_seconds: int,
    overlays: dict[str, bytes] | None = None,
) -> int:
    with tempfile.TemporaryDirectory(prefix="reproit-no-repro-") as directory:
        git(repo, "worktree", "add", "--detach", "--quiet", directory, commit)
        try:
            for relative, content in (overlays or {}).items():
                target = Path(directory) / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(content)
            result = subprocess.run(
                command,
                cwd=directory,
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=timeout_seconds,
            )
        except subprocess.TimeoutExpired as error:
            raise PolicyError(
                f"{commit[:12]}: no-repro test timed out after {timeout_seconds}s"
            ) from error
        finally:
            subprocess.run(
                ["git", "-C", str(repo), "worktree", "remove", "--force", directory],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=30,
            )
        return result.returncode


def validate_no_repro(
    repo: Path,
    commit: str,
    identifier: str,
    files: list[str],
    execute: bool,
) -> dict[str, str]:
    if not EXCEPTION_ID.fullmatch(identifier):
        raise PolicyError(f"no-repro id {identifier!r} is not a safe slug")
    path = NO_REPRO / f"{identifier}.json"
    if path.as_posix() not in files:
        raise PolicyError(
            f"{commit[:12]}: no-repro record {path} must change with the fix"
        )
    record = load_record(repo, commit, path, "no-repro")
    if record.get("schemaVersion") != 1 or record.get("id") != identifier:
        raise PolicyError(f"{path}: schemaVersion 1 and id {identifier} required")
    require_text(record, "detail", path)
    test_path = require_safe_path(record.get("test"), f"{path}: test")
    if test_path not in files:
        raise PolicyError(
            f"{commit[:12]}: no-repro record names {test_path}, which the commit "
            "does not change; the independent regression test must land with "
            "the fix"
        )
    try:
        git(repo, "show", f"{commit}:{test_path}")
    except PolicyError as error:
        raise PolicyError(f"{commit[:12]}: {test_path} does not exist") from error
    validate_evidence(
        repo, commit, record.get("affectedEvidence"), path, "affectedEvidence"
    )
    command, timeout = validate_test_command(record, path)
    affected_exit_code = record.get("affectedExitCode")
    if (
        not isinstance(affected_exit_code, int)
        or isinstance(affected_exit_code, bool)
        or not 1 <= affected_exit_code <= 255
    ):
        raise PolicyError(f"{path}: affectedExitCode must be 1..255")
    if execute:
        fixed_exit_code = execute_test_at_commit(repo, commit, command, timeout)
        if fixed_exit_code != 0:
            raise PolicyError(
                f"{commit[:12]}: fixed no-repro test exited {fixed_exit_code}"
            )
        test_source = git_bytes(repo, "show", f"{commit}:{test_path}")
        affected_ref = f"{commit}^"
        affected_actual = execute_test_at_commit(
            repo,
            affected_ref,
            command,
            timeout,
            overlays={test_path: test_source},
        )
        if affected_actual != affected_exit_code:
            raise PolicyError(
                f"{commit[:12]}: affected no-repro test exited {affected_actual}, "
                f"expected {affected_exit_code}"
            )
    return {"kind": "no-repro", "id": identifier, "test": test_path}


def validate_declaration(
    repo: Path,
    commit: str,
    declaration: str,
    files: list[str],
    execute_no_repro: bool,
    range_files: set[str] | None = None,
) -> dict[str, str]:
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
        validate_exception(repo, commit, code, identifier, range_files)
        return {"kind": "exception", "code": code, "id": identifier}
    if kind == "no-repro":
        return validate_no_repro(
            repo, commit, rest, files, execute=execute_no_repro
        )
    if kind == "not-a-fix":
        return validate_not_a_fix(repo, commit, rest, files)
    raise PolicyError(
        f"{commit[:12]}: unknown declaration {declaration!r}; use guard:, "
        "exception:, no-repro:, or not-a-fix:"
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
        replacement = require_text(record, "replacement", path)
        if not REPRO_ID.fullmatch(replacement) or replacement == guard_id:
            raise PolicyError(f"{path}: replacement must name a different guard id")
        if replacement not in after:
            raise PolicyError(
                f"{path}: replacement {replacement} is not a required guard at {head}"
            )
    if unresolved:
        raise PolicyError(
            "the required guard corpus was weakened without a retirement "
            "declaration: " + "; ".join(unresolved)
        )
    return weakened


def review(
    repo: Path,
    base: str,
    head: str,
    *,
    execute_no_repro: bool = False,
) -> dict[str, object]:
    listed = commits(repo, base, head)
    results = []
    retired: set[str] = set()
    # Every file touched anywhere in the reviewed range, so a follow-up commit
    # may cite an exception record written by an earlier commit in the same
    # push while a citation of unrelated history still fails.
    range_files: set[str] = set()
    for commit in listed:
        range_files.update(changed_files(repo, commit))
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
                "declaration": validate_declaration(
                    repo,
                    commit,
                    found[0],
                    files,
                    execute_no_repro=execute_no_repro,
                    range_files=range_files,
                ),
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
        report = review(
            Path(arguments.repo).resolve(),
            arguments.base,
            arguments.head,
            execute_no_repro=True,
        )
    except PolicyError as error:
        sys.stderr.write(f"self-dogfood fix policy: {error}\n")
        return 1
    json.dump(report, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
