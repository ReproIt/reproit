#!/usr/bin/env python3
"""Refuse an SDK that does not execute the shared behavioral vectors.

`capture-behavior-v1.json` is language neutral on purpose: twenty SDKs hand
implement one contract, so a defect otherwise has to be found twenty times. That
only works if every SDK actually RUNS the vectors, and for a long time most did
not. Bounds was executed by five of the ten wired SDKs and the header cap by
exactly one, which is how Android could cap headers in insertion order (the Go
defect, verbatim) while a shared vector describing sorted order sat unread in
the same repository.

Three shapes fail closed:

  1. a directory under sdk/ that the coverage table does not mention, so a new
     SDK cannot be added unwired and unnoticed
  2. a coverage entry naming a test file that is not on disk
  3. a test file that does not reference every vector group its role requires

Rule 3 is a spelling check, and is stated as one rather than dressed up. It
cannot tell a real assertion from a mention. What it CAN do is make the wiring
impossible to drop silently, which is the failure that actually happened; the
assertions themselves are proven by each SDK's own suite, and by the negative
controls recorded in validation/invariants/LEDGER.md.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

SDK_DIR = Path(__file__).resolve().parent
VECTORS = SDK_DIR / "capture-behavior-v1.json"

# Directories under sdk/ that are not SDKs. Kept identical to
# check-inventory.py's list on purpose: two checkers disagreeing about what an
# SDK is would be a third way to lose one.
NOT_AN_SDK = {"src", "test", "node_modules"}

# The token a test file must contain to count as executing a group. A group
# named `redaction.typeCases` is reached in every language as the JSON key
# `typeCases`, so the leaf is what gets searched for.
def token(group: str) -> str:
    return group.split(".")[-1]


def main() -> int:
    document = json.loads(VECTORS.read_text(encoding="utf-8"))
    coverage = document["coverage"]
    roles = coverage["roles"]
    sdks = coverage["sdks"]
    not_applicable = coverage["notApplicable"]
    # `notApplicable` used to also carry src/ and test/, which are not SDKs at
    # all, so the summary line read "3 accounted for as not applicable" when the
    # true number of SDKs the vectors are meaningless for was one. Two claims
    # that different deserve two fields.
    not_an_sdk = coverage["notAnSdk"]
    problems: list[str] = []

    for name in sorted(set(not_an_sdk) - NOT_AN_SDK):
        problems.append(
            f"{name}: listed under notAnSdk but this checker treats it as an "
            "SDK directory, so it would still need to execute the vectors"
        )
    for name in sorted(set(not_applicable) & NOT_AN_SDK):
        problems.append(
            f"{name}: is not an SDK, so notApplicable is the wrong field for "
            "it; that is how the not-applicable count came to overstate itself"
        )

    for group in (g for role in roles.values() for g in role["requiredGroups"]):
        node = document
        for part in group.split("."):
            if not isinstance(node, dict) or part not in node:
                problems.append(f"coverage requires group {group}, which the vectors lack")
                break
            node = node[part]

    found = {
        path.name
        for path in SDK_DIR.iterdir()
        if path.is_dir() and not path.name.startswith(".")
    }
    listed = set(sdks) | set(not_applicable) | set(not_an_sdk)
    for name in sorted(found - listed - NOT_AN_SDK):
        problems.append(
            f"{name} exists under sdk/ and the coverage table does not mention it. "
            "Wire it to the shared vectors, or record why they are meaningless "
            "for it under notApplicable."
        )
    for name in sorted(listed - found):
        problems.append(f"coverage names {name}, which is not a directory under sdk/")
    for name, reason in sorted(not_applicable.items()):
        if len(reason) < 80:
            problems.append(
                f"{name}: notApplicable needs a reason, not a shrug. "
                "Say what the SDK does instead and which surfaces it therefore lacks."
            )

    for name, entry in sorted(sdks.items()):
        role = entry["role"]
        if role not in roles:
            problems.append(f"{name}: unknown role {role!r}")
            continue
        test = SDK_DIR / name / entry["test"]
        if not test.is_file():
            problems.append(f"{name}: {entry['test']} does not exist")
            continue
        source = test.read_text(encoding="utf-8", errors="replace")
        for group in roles[role]["requiredGroups"]:
            if token(group) not in source:
                problems.append(
                    f"{name}: {entry['test']} never reads {group}, so that "
                    "vector proves nothing here"
                )

    if problems:
        print("behavior vector coverage: unexecuted vectors", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1

    counts: dict[str, int] = {}
    for entry in sdks.values():
        counts[entry["role"]] = counts.get(entry["role"], 0) + 1
    summary = ", ".join(f"{count} {role}" for role, count in sorted(counts.items()))
    print(
        f"behavior vector coverage: {len(sdks)} SDKs execute the vectors "
        f"({summary}); {len(not_applicable)} SDK accounted for as not "
        f"applicable, {len(not_an_sdk)} directories are not SDKs"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
