#!/usr/bin/env python3
"""Keep owned source files around the workspace's 1,000-line guideline.

The guideline is a target, not a wall: good engineering is being around that
amount, and a mechanical change (a rustfmt pass, a generated table) must not
turn the gate red. Files over the guideline are named as warnings; the check
fails only past the hard ceiling, where "around" has clearly ended.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

GUIDELINE_LINES = 1_000
HARD_CEILING_LINES = 1_200
SOURCE_SUFFIXES = {
    ".css",
    ".dart",
    ".html",
    ".js",
    ".mjs",
    ".py",
    ".rs",
    ".sh",
    ".swift",
    ".ts",
    ".tsx",
}
EXCLUDED_DIRECTORIES = {
    ".cache",
    ".git",
    ".gradle",
    ".pytest_cache",
    ".venv",
    ".work",
    "artifacts",
    "build",
    "cases",
    "locks",
    "node_modules",
    "reports",
    "studies",
    "target",
}
WORKSPACE_PROJECTS = (
    "reproit-cli",
    "reproit-cloud",
    "reproit-cloud-deploy",
    "reproit-lab",
    "reproit-proof",
    "reproit-site",
)


def source_roots() -> list[Path]:
    cli_root = Path(__file__).resolve().parents[1]
    workspace = cli_root.parent
    roots = [
        workspace / project
        for project in WORKSPACE_PROJECTS
        if (workspace / project).is_dir()
    ]
    return roots or [cli_root]


def source_files(root: Path):
    for directory, names, filenames in os.walk(root):
        names[:] = [name for name in names if name not in EXCLUDED_DIRECTORIES]
        for filename in filenames:
            path = Path(directory, filename)
            if path.suffix in SOURCE_SUFFIXES:
                yield path


def line_count(path: Path) -> int:
    with path.open("rb") as source:
        return sum(1 for _ in source)


def main() -> int:
    over_guideline = []
    over_ceiling = []
    for root in source_roots():
        for path in source_files(root):
            lines = line_count(path)
            if lines > HARD_CEILING_LINES:
                over_ceiling.append((lines, path))
            elif lines > GUIDELINE_LINES:
                over_guideline.append((lines, path))
    for lines, path in sorted(over_guideline, reverse=True):
        print(
            f"warning: {path}: {lines} lines, over the {GUIDELINE_LINES}-line "
            f"guideline; split it when the next real change lands",
            file=sys.stderr,
        )
    if over_ceiling:
        for lines, path in sorted(over_ceiling, reverse=True):
            print(
                f"{path}: {lines} lines, past the {HARD_CEILING_LINES}-line "
                f"hard ceiling (guideline {GUIDELINE_LINES})",
                file=sys.stderr,
            )
        return 1
    print(
        f"source-size check passed: guideline {GUIDELINE_LINES} lines "
        f"({len(over_guideline)} file(s) over, warned), hard ceiling {HARD_CEILING_LINES}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
