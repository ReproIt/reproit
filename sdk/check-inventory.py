#!/usr/bin/env python3
"""Refuse an SDK that ships without a blocking CI gate.

This file replaces check-tiers.py. The tiers it enforced said three different
things about what an SDK promised: core blocked a release, community was
reported but not merge blocking, and unmaintained was gated by nothing at all.
Nine SDKs sat in that last bucket. The product's copy promises every bug is
reproducible, and a support tier is exactly the hedge that copy must never
carry, so the tiers are gone and the promise is now the mechanism: if a
directory exists under sdk/, its suite runs on every push and a failure fails
the build.

Four shapes fail closed:

  1. a directory under sdk/ that INVENTORY.json does not declare, so an SDK
     could be added ungated and unnoticed
  2. an entry naming a directory that is not there
  3. an entry whose declared gate string is absent from ci.yml, so the suite is
     described as gated and is not run
  4. a job in ci.yml carrying `continue-on-error: true`, which is a support
     tier spelled in YAML: the job runs, reports, and lets the build pass

Rule 3 is a wiring check and is stated as one rather than dressed up. It cannot
tell a real suite from a step that merely mentions a path. What it CAN do is
make the invocation impossible to drop silently, which is the failure that
actually happened: seven SDKs had no mention in ci.yml at all, and four more
named only some of their test files.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

SDK_DIR = Path(__file__).resolve().parent
ROOT = SDK_DIR.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
CARGO = ROOT / "Cargo.toml"

# Directories under sdk/ that are not SDKs. Kept identical to
# check-behavior-coverage.py's list on purpose: two checkers disagreeing about
# what an SDK is would be another way to lose one.
NOT_AN_SDK = {"src", "test", "node_modules"}

# Anything that reintroduces a tier by another name. A `tier` key would be the
# vestigial field this change exists to remove; the rest are the words the old
# file used for "shipped, not gated".
FORBIDDEN_KEYS = {"tier", "tiers", "core", "community", "unmaintained"}


def discovered_sdks() -> set[str]:
    return {
        path.name
        for path in SDK_DIR.iterdir()
        if path.is_dir() and path.name not in NOT_AN_SDK and not path.name.startswith(".")
    }


def main() -> int:
    document = json.loads((SDK_DIR / "INVENTORY.json").read_text(encoding="utf-8"))
    sdks: dict[str, dict] = document["sdks"]
    problems: list[str] = []

    for key in FORBIDDEN_KEYS & set(document):
        problems.append(
            f"INVENTORY.json has a top level {key!r} key. There is no SDK "
            "tiering: every SDK in this repository is fully supported."
        )

    found = discovered_sdks()
    for name in sorted(found - set(sdks)):
        problems.append(
            f"{name} exists under sdk/ and INVENTORY.json does not declare it. "
            "Add it with the CI gate that blocks a release when it breaks."
        )
    for name in sorted(set(sdks) - found):
        problems.append(f"INVENTORY.json names {name}, which is not a directory under sdk/")

    workflow = WORKFLOW.read_text(encoding="utf-8") if WORKFLOW.is_file() else ""
    if not workflow:
        problems.append(f"{WORKFLOW} is missing, so nothing gates any SDK")

    for name, entry in sorted(sdks.items()):
        for key in FORBIDDEN_KEYS & set(entry):
            problems.append(f"{name}: {key!r} is not a field; there is no SDK tiering")
        if not entry.get("runs"):
            problems.append(f"{name}: no `runs` sentence, so what the gate covers is unstated")
        gates = entry.get("gate") or []
        if not gates:
            problems.append(f"{name}: no gate, so it ships with nothing blocking a release")
        # A gate must be unmistakably about THIS SDK. Naming the directory is
        # the usual way. The two Rust SDKs are cargo packages whose names differ
        # from their directories, so they name the package instead and the
        # package is tied back to the directory through the workspace members.
        package = entry.get("cargoPackage")
        if package:
            member = f'"sdk/{name}"'
            if member not in CARGO.read_text(encoding="utf-8"):
                problems.append(
                    f"{name}: declares cargoPackage {package!r} but sdk/{name} "
                    "is not a member of the root workspace, so `cargo -p` "
                    "cannot reach it"
                )
            identifies = any(f"-p {package}" in gate for gate in gates)
        else:
            identifies = any(name in gate for gate in gates)
        if not identifies:
            problems.append(
                f"{name}: no gate string names its own directory or cargo "
                "package, so the declaration could be satisfied by an "
                "unrelated CI step"
            )
        for gate in gates:
            if gate not in workflow:
                problems.append(
                    f"{name}: ci.yml does not contain {gate!r}, so the suite is "
                    "declared gated and is not actually run"
                )

    # A job that reports and lets the build pass is the old community tier with
    # a different spelling, so the workflow may not contain one.
    for match in re.finditer(r"continue-on-error:\s*true", workflow):
        line = workflow.count("\n", 0, match.start()) + 1
        problems.append(
            f"ci.yml:{line} sets continue-on-error: true. A job that runs, "
            "reports, and does not block is a support tier in YAML."
        )

    if problems:
        print("sdk inventory: an SDK ships without a blocking gate", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1

    print(f"sdk inventory: {len(sdks)} SDKs, every one gated in ci.yml and blocking")
    return 0


if __name__ == "__main__":
    sys.exit(main())
