#!/usr/bin/env python3
"""Regression test: no repo caller passes a deleted flag to `reproit check`.

The vocabulary purge moved check's per-project knobs (runs, devices, locale,
device, kind) into gate config; a caller still passing them makes check exit
2 with a usage error, which read as a flaky guard in CI. This test fails
while any workflow or self-dogfood script still passes one of the dead flags
to a check invocation, and passes once every caller relies on gate config.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEAD_CHECK_FLAGS = ("--runs", "--devices", "--locale", "--device", "--kind")
CALLER_GLOBS = (
    ".github/workflows/*.yml",
    ".github/actions/**/*.yml",
    "validation/self-dogfood/*.py",
    "validation/self-dogfood/*.mjs",
)


def check_invocations(text: str) -> list[str]:
    """Windows of text around each `check` token of a reproit invocation."""
    windows = []
    for match in re.finditer(r"reproit[\"',\s\\]+(--json[\"',\s\\]+)?(--yes[\"',\s\\]+)?", text):
        windows.append(text[match.start() : match.start() + 400])
    # Python list-form command vectors spell arguments one per line.
    if '"check"' in text or "'check'" in text:
        windows.append(text)
    return windows


def main() -> int:
    offenders = []
    for pattern in CALLER_GLOBS:
        for path in ROOT.glob(pattern):
            # Test files legitimately embed the AFFECTED fixture text; the
            # callers under contract are the workflows and runner scripts.
            if path.name.startswith("test_"):
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            for window in check_invocations(text):
                if "check" not in window:
                    continue
                for flag in DEAD_CHECK_FLAGS:
                    if flag in window:
                        offenders.append(f"{path.relative_to(ROOT)}: passes {flag} to check")
    if offenders:
        print("dead check flags still passed by callers:", file=sys.stderr)
        for offender in sorted(set(offenders)):
            print(f"  {offender}", file=sys.stderr)
        return 1
    print("no caller passes a dead check flag")
    return 0


if __name__ == "__main__":
    sys.exit(main())
