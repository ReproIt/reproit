#!/usr/bin/env python3
"""Regression test: no tracked file points at a fixture path that does not exist.

Renaming `examples/` to `fixtures/` broke CI TWICE in one session, both times
because a hand-written sweep rule missed a shape nobody thought of. First the
rule excluded any path preceded by a segment, which correctly skipped the
foreign `examples/` directories (a cargo convention dir in the Rust SDK, a
cloned grpc checkout, two URL paths) but also skipped `$ROOT/examples/...`,
which was ours. Then the sweep excluded the moved tree itself, so
`fixtures/compose-fixture/compose-appium-smoke.sh` still did
`cd "$ROOT/examples/compose-fixture"` and android-hosted went red.

Both misses share a shape this project keeps paying for: a rule that has to
ENUMERATE what to look for. The check here enumerates nothing. It reads every
`fixtures/<name>` and `examples/<name>` reference out of the tracked files and
asks the filesystem whether it resolves. A path that does not exist is either a
rename that was missed or a fixture that was deleted, and both should fail the
build rather than a gate twenty minutes into CI.

Recorded evidence and past declarations are skipped on purpose: they describe
what was true when they were written, and rewriting them would falsify a record.
"""

from __future__ import annotations

import re
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# A reference to a fixture or example, in any quoting or variable context:
# "$ROOT/fixtures/web-fixture", 'examples/qt-fixture/run.sh', bare paths.
REFERENCE = re.compile(r"(?:fixtures|examples)/([A-Za-z0-9][A-Za-z0-9._-]*)")

# Files whose content is a historical record. They keep their original spelling.
SKIP_PREFIXES = (
    "validation/field/evidence/",
    "validation/self-dogfood/evidence/",
    "validation/self-dogfood/not-a-fix/",
    "validation/self-dogfood/no-repro/",
    "validation/self-dogfood/retirements/",
    ".reproit/",
)

SKIP_SUFFIXES = (".png", ".gif", ".mp4", ".mov", ".jpg", ".svg", ".ico", ".lock")

# Dependency lockfiles describe other people's package layouts, not ours.
SKIP_NAMES = ("package-lock.json", "Cargo.lock", "pnpm-lock.yaml")

# References that are not this repository's directories at all.
FOREIGN = (
    "sdk/reproit-backend-rs/examples/",  # a cargo convention dir
    "$GRPC/examples/",  # inside a cloned grpc checkout
    "/app/examples/",  # a URL path in a browser test
    "pages/examples/",  # a URL path in a browser test
    "examples/one",  # a synthetic monorepo path in a unit test
    "examples/Cargo.toml",  # the same synthetic path
    "examples/helloworld",  # inside a cloned grpc checkout
    "examples/hermetic_fixture",  # a cargo example target, not a directory
    "examples/replay_parity_probe",  # a cargo example target, not a directory
    "../reproit-cloud/",  # a sibling repository, guarded by exists() at the call site
)


# The whole path-like token a reference sits inside, so a nested
# `tests/fixtures/backend/...` is resolved as written rather than truncated to
# its `fixtures/...` tail.
TOKEN = re.compile(r"[A-Za-z0-9_$./{}-]*(?:fixtures|examples)/[A-Za-z0-9_$./{}-]*")


def candidates(line: str, reference: str) -> list[str]:
    """Paths worth trying for one reference, longest first."""
    found = [reference]
    for token in TOKEN.finditer(line):
        text = token.group(0).strip("./")
        if reference.split("/", 1)[1].split("/", 1)[0] not in text:
            continue
        # Drop a leading shell/CMake variable: "$ROOT/fixtures/x" -> "fixtures/x".
        cleaned = re.sub(r"^\$?\{?[A-Za-z0-9_]+\}?/", "", text)
        found.extend([text, cleaned])
    return found


def resolves(relative: str, reference: str) -> bool:
    """True when the reference names something that exists.

    Resolved against the referencing file's own directory and every ancestor up
    to the repository root, because `fixtures/` is a normal directory name: the
    protocol crate, the a2ui runner and several SDKs each keep their own, and a
    reference to one of those is correct rather than stale.
    """
    directory = (ROOT / relative).parent
    while True:
        if (directory / reference).exists():
            return True
        if directory == ROOT:
            return False
        directory = directory.parent


def executable_lines(text: str) -> list[tuple[int, str]]:
    """Lines that can actually run something.

    Comments and docstrings DESCRIBE paths, including dead ones: this file's own
    header explains the break it exists to prevent, and that prose is not a
    reference. Prose about a dead path is how the rule gets explained, so it
    must not be what trips the rule. Same discipline as
    test_check_internal_invocations.py.
    """
    lines: list[tuple[int, str]] = []
    fence = None
    for number, raw in enumerate(text.splitlines(), start=1):
        stripped = raw.strip()
        if fence is not None:
            if fence in stripped:
                fence = None
            continue
        for quote in ('\u0022\u0022\u0022', "\u0027\u0027\u0027"):
            if stripped.startswith(quote) and stripped.count(quote) == 1:
                fence = quote
                break
        if fence is not None:
            continue
        if stripped.startswith(('#', '//', '*', '<!--')):
            continue
        lines.append((number, raw))
    return lines


def tracked_files() -> list[str]:
    listing = subprocess.run(
        ["git", "ls-files"], cwd=ROOT, capture_output=True, text=True, check=True
    )
    return listing.stdout.split()


def unresolved_references() -> tuple[int, list[str]]:
    """Returns (files scanned, references naming something that is not there)."""
    scanned = 0
    broken: list[str] = []
    for relative in tracked_files():
        if relative.startswith(SKIP_PREFIXES) or relative.endswith(SKIP_SUFFIXES):
            continue
        if Path(relative).name in SKIP_NAMES:
            continue
        path = ROOT / relative
        if not path.is_file() or "node_modules" in relative:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        scanned += 1
        for number, line in executable_lines(text):
            if any(foreign in line for foreign in FOREIGN):
                continue
            for match in REFERENCE.finditer(line):
                # Trailing sentence punctuation is prose, not part of the path.
                name = match.group(1).rstrip(".")
                reference = f"{match.group(0).split('/')[0]}/{name}"
                if not name or "*" in name:
                    continue
                if any(
                    resolves(relative, candidate)
                    for candidate in candidates(line, reference)
                ):
                    continue
                # The rename's exact failure shape: still points at examples/
                # while the thing now lives under fixtures/.
                if (ROOT / "fixtures" / name).exists():
                    broken.append(
                        f"{relative}:{number}: names `examples/{name}`, "
                        f"which now lives at `fixtures/{name}`"
                    )
                    continue
                if (ROOT / "docs" / "examples" / name).exists():
                    continue
                broken.append(
                    f"{relative}:{number}: names `{reference}`, which does not exist"
                )
    return scanned, broken


class FixturePathTests(unittest.TestCase):
    def test_every_fixture_reference_resolves(self) -> None:
        scanned, broken = unresolved_references()
        self.assertEqual(broken, [])
        # A scan that matched nothing is the same trap as a filtered test run.
        self.assertGreater(scanned, 100)


def main() -> int:
    scanned, broken = unresolved_references()
    if scanned == 0:
        print(
            "fixture paths: matched no tracked files, which is itself the "
            "failure this gate exists to catch",
            file=sys.stderr,
        )
        return 1
    if broken:
        print("fixture paths: references that do not resolve", file=sys.stderr)
        for line in broken:
            print(f"  {line}", file=sys.stderr)
        return 1
    print(f"fixture paths: {scanned} tracked files name only paths that exist")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
