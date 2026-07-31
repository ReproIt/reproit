#!/usr/bin/env python3
"""Refuse an acceptance script that cannot tell passing from stopping early.

This gate exists because the same defect appeared FOUR times in one day, in
four different harnesses written by four different authors:

  1. a filtered `cargo test` that matched zero tests and exited 0, reported
     green while running nothing
  2. an exit-code-only hermetic check, where a capture the CLI could not even
     resolve also exits 1, so a resolution error read as a reproduction
  3. `run_case` enabling errexit and never restoring it, so in a script that
     deliberately runs subjects exiting non-zero the first such command killed
     the run and the output looked exactly like a pass of everything printed
  4. a defect verifier deciding on one regex, so a binary that never ran at all
     was reported as "did not reproduce"

They share one shape: A HARNESS THAT STOPS EARLY LOOKS EXACTLY LIKE ONE THAT
PASSED EVERYTHING IT PRINTED. The fix in each case was the same, so it is now a
rule rather than a lesson: an acceptance script must end by asserting how many
cases actually ran, and a verdict must rest on positive evidence rather than on
an exit code alone.

The check is deliberately shallow and syntactic. It cannot prove a harness is
honest; it refuses the specific shape that has burned this project repeatedly.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Scripts whose whole job is to assert a claim. Helper and packaging scripts
# are deliberately out of scope: they are not verdict authorities.
ACCEPTANCE_GLOBS = (
    "validation/backend/*-e2e/run.sh",
    "validation/process/run.sh",
    "validation/process-checkpoint/*.sh",
    "sdk/*/validation/hermetic-e2e.sh",
)

# Evidence that the script counts what it ran, rather than trusting that
# reaching the end means everything happened.
COUNT_MARKERS = (
    "cases",
    "case count",
    "CASES_RUN",
    "ran=",
    "expected_cases",
    "assert_case_count",
)


def has_case_accounting(text: str) -> bool:
    lowered = text.lower()
    if any(marker.lower() in lowered for marker in COUNT_MARKERS):
        return True
    # A script that increments a counter and compares it at the end also
    # qualifies, however it spells the variable.
    return bool(re.search(r"\+\+\s*\)\)|\b\w+=\$\(\(\s*\$?\w+\s*\+\s*1\s*\)\)", text))


def leaks_errexit(text: str) -> bool:
    """`set +e` without a matching restore inside the same helper is the third
    defect above. A script that disables errexit must turn it back on or
    capture the status explicitly."""
    disables = text.count("set +e")
    restores = text.count("set -e")
    return disables > 0 and restores == 0


def main() -> int:
    problems: list[str] = []
    checked = 0
    for pattern in ACCEPTANCE_GLOBS:
        for path in sorted(ROOT.glob(pattern)):
            checked += 1
            text = path.read_text(encoding="utf-8", errors="replace")
            relative = path.relative_to(ROOT)
            if not has_case_accounting(text):
                problems.append(
                    f"{relative}: no case accounting. An acceptance script must "
                    "assert how many cases ran, or an early exit is "
                    "indistinguishable from a pass."
                )
            if leaks_errexit(text):
                problems.append(
                    f"{relative}: disables errexit and never restores it. "
                    "Capture the status instead, or the first failing command "
                    "silently ends the run."
                )
    if checked == 0:
        print(
            "harness integrity: matched no acceptance scripts, which is itself "
            "the failure this gate exists to catch",
            file=sys.stderr,
        )
        return 1
    if problems:
        print("harness integrity: acceptance scripts that can hide an early exit", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1
    print(f"harness integrity: {checked} acceptance scripts account for their cases")
    return 0


if __name__ == "__main__":
    sys.exit(main())
