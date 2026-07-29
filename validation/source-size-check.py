#!/usr/bin/env python3
"""Reject owned source files above the workspace's 1,000-line boundary."""

from __future__ import annotations

import os
import sys
from pathlib import Path

MAX_LINES = 1_000
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
    oversized = []
    for root in source_roots():
        for path in source_files(root):
            lines = line_count(path)
            if lines > MAX_LINES:
                oversized.append((lines, path))
    if oversized:
        for lines, path in sorted(oversized, reverse=True):
            print(f"{path}: {lines} lines, maximum is {MAX_LINES}", file=sys.stderr)
        return 1
    print(f"source-size check passed: every owned source file is at most {MAX_LINES} lines")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
