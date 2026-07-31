#!/usr/bin/env python3
"""Regression test: no harness invokes an internal command by a dead spelling.

`__atspi` and `__tui` were hidden TOP-LEVEL subcommands until the minimal CLI
vocabulary landed (d957df5), which moved every internal command under one
`internal` multiplex. That commit re-pointed the Rust tests and the docs but
missed three shell harnesses, so clap exited 2 before the runner ever started
and three required gates went red for 17 commits without anyone reading them:
linux-atspi-gtk, linux-atspi-toolkits, and tui-pty.

The reason it stayed invisible is worth naming, because it is the shape this
project keeps paying for. Every assertion that failed was of the form "this
marker is PRESENT"; the handful that passed were of the form "this bad marker
is ABSENT", which a process that never ran satisfies for free. So the gate
printed four PASS lines and a process that produced no output at all looked,
at a glance, partially healthy.

The rule is one line: an internal command `__name` is reachable ONLY as
`reproit internal __name`, so any harness naming one must spell it that way.
The check is syntactic and looks only at executable lines. Like
test_check_flag_callers.py it skips `test_*` files, whose fixtures name the
dead spelling on purpose; a python test that drives the CLI as a gate is
therefore out of scope, which is the one blind spot here.
"""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Harness trees: scripts that drive the CLI as part of asserting a claim.
HARNESS_GLOBS = (
    ".github/scripts/*.sh",
    ".github/scripts/*.ps1",
    "validation/**/*.sh",
    "validation/**/*.py",
    "validation/**/*.ps1",
    "examples/**/*.sh",
)

# `reproit`, or a path ending in it, followed by the next word.
INVOCATION = re.compile(r"\breproit(?:\.exe)?\b[\"']?\s+(\S+)")

# The same command passed as a standalone argument string, which is how the
# Windows harness spells it: the binary is a variable, so the command never
# sits next to the word `reproit`. A whole quoted argument that is exactly one
# internal command is the dead spelling; `"internal __uia"` is not.
ARGUMENT = re.compile(r"[\"'](__[a-z][a-z0-9-]*)[\"']")

SKIP_DIRS = ("/target/", "/.claude/", "/node_modules/")


def executable_lines(text: str) -> list[tuple[int, str]]:
    """Lines that can run. Comments and docstrings describe intent; they cannot
    break a gate, and prose about the dead spelling is how the rule gets
    explained."""
    lines = []
    fence = None
    for number, raw in enumerate(text.splitlines(), start=1):
        stripped = raw.strip()
        if fence is not None:
            if fence in stripped:
                fence = None
            continue
        for quote in ('"""', "'''"):
            if stripped.startswith(quote) and stripped.count(quote) == 1:
                fence = quote
                break
        if fence is not None:
            continue
        if stripped.startswith("#") or stripped.startswith("//"):
            continue
        lines.append((number, raw))
    return lines


def bad_invocations(text: str) -> list[tuple[int, str]]:
    found = []
    for number, line in executable_lines(text):
        for match in INVOCATION.finditer(line):
            word = match.group(1).strip("\"'")
            if not word.startswith("__"):
                continue
            # `internal` must be the token immediately before the command.
            before = line[: match.start(1)].rstrip().rstrip("\"'")
            if before.endswith("internal"):
                continue
            found.append((number, word))
        for match in ARGUMENT.finditer(line):
            if (number, match.group(1)) in found:
                continue
            found.append((number, match.group(1)))
    return found


def scan_repository() -> tuple[int, list[str]]:
    """Returns (harnesses scanned, offending invocations)."""
    scanned = 0
    offenders: list[str] = []
    for glob in HARNESS_GLOBS:
        for path in sorted(ROOT.glob(glob)):
            if any(part in path.as_posix() for part in SKIP_DIRS):
                continue
            if not path.is_file() or path.name.startswith("test_"):
                continue
            scanned += 1
            text = path.read_text(encoding="utf-8", errors="replace")
            for number, word in bad_invocations(text):
                offenders.append(
                    f"{path.relative_to(ROOT)}:{number}: invokes `reproit {word}`, "
                    f"reachable only as `reproit internal {word}`; the dead spelling "
                    "exits 2 before the runner starts"
                )
    return scanned, offenders


class InternalInvocationTests(unittest.TestCase):
    def test_the_dead_top_level_spelling_is_refused(self) -> None:
        # The exact line that made the AT-SPI scenario gate red.
        text = 'RUNNER="/tmp/reproit-target/debug/reproit __atspi"\n'
        self.assertEqual(bad_invocations(text), [(1, "__atspi")])

    def test_the_multiplexed_spelling_passes(self) -> None:
        text = '"$ROOT/target/debug/reproit" internal __tui | tee "$LOG"\n'
        self.assertEqual(bad_invocations(text), [])

    def test_a_quoted_binary_path_still_matches(self) -> None:
        # `reproit"` then the command: the closing quote must not hide it. A sed
        # that ignored this quote is how a first negative control silently passed.
        text = '"$ROOT/target/debug/reproit" __tui\n'
        self.assertEqual(bad_invocations(text), [(1, "__tui")])

    def test_a_shell_comment_about_the_dead_spelling_is_not_a_failure(self) -> None:
        self.assertEqual(bad_invocations("# it used to be `reproit __atspi`\n"), [])

    def test_a_python_docstring_about_the_dead_spelling_is_not_a_failure(self) -> None:
        text = '"""Origin: `reproit __atspi` was top level.\n\nProse.\n"""\nx = 1\n'
        self.assertEqual(bad_invocations(text), [])

    def test_a_standalone_argument_string_is_refused(self) -> None:
        # The exact line that made the windows-uia gate red: the binary is a
        # variable, so the command never sits next to the word `reproit`.
        text = '            $start.Arguments = "__uia"\n'
        self.assertEqual(bad_invocations(text), [(1, "__uia")])

    def test_a_multiplexed_argument_string_passes(self) -> None:
        self.assertEqual(bad_invocations('$start.Arguments = "internal __uia"\n'), [])

    def test_a_dunder_identifier_is_not_a_command(self) -> None:
        self.assertEqual(bad_invocations('if __name__ == "__main__":\n'), [])

    def test_a_non_internal_command_is_ignored(self) -> None:
        self.assertEqual(bad_invocations("reproit check self-dogfood-cli\n"), [])

    def test_every_harness_in_the_repository_names_a_reachable_command(self) -> None:
        scanned, offenders = scan_repository()
        self.assertEqual(offenders, [])
        # A glob that matches nothing is the same trap as a filtered cargo test.
        self.assertGreater(scanned, 0)


def main() -> int:
    scanned, offenders = scan_repository()
    if scanned == 0:
        print(
            "internal invocations: matched no harness scripts, which is itself "
            "the failure this gate exists to catch",
            file=sys.stderr,
        )
        return 1
    if offenders:
        print("internal invocations: harnesses naming an unreachable command", file=sys.stderr)
        for offender in offenders:
            print(f"  {offender}", file=sys.stderr)
        return 1
    print(f"internal invocations: {scanned} harness scripts name reachable commands")
    return 0


if __name__ == "__main__":
    sys.exit(main())
