#!/usr/bin/env python3
"""Keep sdk/TIERS.json honest about what exists and what CI actually gates.

The tier file is only worth having if it cannot quietly drift from reality.
Three ways it could:

  1. an SDK directory exists and no tier claims it, so its cost is invisible
  2. a tier names an SDK that no longer exists
  3. a core SDK is not actually gated in CI, so the parity promise is a claim
     rather than a mechanism

The third is the one that matters. Before this file, the four backend SDKs the
product's thesis depends on had ONE shared conformance test in CI and no suite
of their own, while eight UI SDKs each had a dedicated job. The gating was
inverted from the thesis, and nothing said so.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

SDK_DIR = Path(__file__).resolve().parent
ROOT = SDK_DIR.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"

# Directories under sdk/ that are not SDKs.
NOT_AN_SDK = {"src", "test", "node_modules"}


def discovered_sdks() -> set[str]:
    found = set()
    for path in SDK_DIR.iterdir():
        if not path.is_dir() or path.name in NOT_AN_SDK or path.name.startswith("."):
            continue
        found.add(path.name)
    return found


def main() -> int:
    document = json.loads((SDK_DIR / "TIERS.json").read_text(encoding="utf-8"))
    tiers = document["tiers"]
    declared: dict[str, str] = {}
    problems: list[str] = []

    for tier, names in tiers.items():
        for name in names:
            if name in declared:
                problems.append(f"{name} is declared in both {declared[name]} and {tier}")
            declared[name] = tier

    found = discovered_sdks()
    for name in sorted(found - set(declared)):
        problems.append(
            f"{name} exists under sdk/ but no tier claims it, so its ongoing "
            "cost is invisible. Add it to TIERS.json, including as unmaintained."
        )
    for name in sorted(set(declared) - found):
        problems.append(f"TIERS.json names {name}, which is not a directory under sdk/")

    # The load bearing check: a core SDK must be gated by a real CI job.
    workflow = WORKFLOW.read_text(encoding="utf-8") if WORKFLOW.is_file() else ""
    for name in tiers.get("core", []):
        if name not in workflow:
            problems.append(
                f"{name} is core but is never named in ci.yml, so its parity "
                "promise has no mechanism behind it"
            )

    if problems:
        print("sdk tiers: declaration does not match reality", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1

    counts = {tier: len(names) for tier, names in tiers.items()}
    summary = ", ".join(f"{tier} {count}" for tier, count in counts.items())
    print(f"sdk tiers: {len(declared)} SDKs declared ({summary}), every core SDK gated")
    return 0


if __name__ == "__main__":
    sys.exit(main())
