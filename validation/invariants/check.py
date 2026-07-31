#!/usr/bin/env python3
"""Refuse an invariant ledger entry whose proof does not exist.

The ledger at LEDGER.md records behaviors this codebase learned, usually by
paying for them. An entry is only worth writing down if something EXECUTES it,
so this checker fails closed on three shapes:

  1. an entry naming a proof path that is not on disk
  2. an entry whose proof is a cargo test filter that names no test in the tree
  3. a ledger with no entries at all, which is the same trap as a filtered
     `cargo test` matching nothing and exiting 0

It deliberately does NOT run the proofs. Running them needs several toolchains,
containers, and in one case an Android emulator; that is what the acceptance
scripts and CI are for. This checker answers the cheaper question that has to
be true first: does the thing this entry points at actually exist.

Ledger format, one table row per invariant:

  | id | invariant | proof | kind | origin |

`proof` is a repo-relative path, optionally followed by `::` and a test name or
filter. `kind` is one of the PROOF_KINDS below.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LEDGER = Path(__file__).resolve().parent / "LEDGER.md"

# How the proof is executed. Recorded so a reader knows what running it costs.
PROOF_KINDS = {
    "cargo",  # a rust test, run by cargo test
    "script",  # an acceptance script, run directly
    "python",  # a python test or checker
    "node",  # a node test or checker
    "sdk",  # a per-SDK suite in its own toolchain
    "manual",  # measured on hardware, evidence retained, not re-run in CI
}

ROW = re.compile(r"^\|\s*(?P<id>[a-z0-9][a-z0-9-]*)\s*\|(?P<rest>.*)\|\s*$")
GAP_HEADING = "## Invariants with no executable proof"


def rows(text: str) -> list[dict[str, str]]:
    """Table rows above the gap heading. Everything below it is, by
    definition, the list of invariants with no proof to check."""
    body = text.split(GAP_HEADING)[0]
    found = []
    for line in body.splitlines():
        stripped = line.strip()
        match = ROW.match(stripped)
        if not match:
            continue
        # A markdown table repeats its header and separator per section; those
        # are not entries. `id` as an id is always the header.
        if match.group("id") == "id" or set(stripped) <= set("| -"):
            continue
        cells = [cell.strip() for cell in match.group("rest").split("|")]
        if len(cells) < 4:
            continue
        found.append(
            {
                "id": match.group("id"),
                "invariant": cells[0],
                "proof": cells[1],
                "kind": cells[2],
                "origin": cells[3],
            }
        )
    return found


def cargo_test_exists(filter_name: str) -> bool:
    """A cargo proof names a test function. Grep the tree for its definition
    rather than compiling, because the crate may legitimately be mid-edit."""
    result = subprocess.run(
        ["grep", "-rqE", rf"fn {re.escape(filter_name)}\b", "crates/"],
        cwd=ROOT,
        check=False,
    )
    return result.returncode == 0


def check_entry(entry: dict[str, str]) -> list[str]:
    problems = []
    if entry["kind"] not in PROOF_KINDS:
        problems.append(
            f"{entry['id']}: proof kind {entry['kind']!r} is not one of "
            f"{sorted(PROOF_KINDS)}"
        )
    proof = entry["proof"].strip("`")
    path_part, _, name_part = proof.partition("::")
    target = ROOT / path_part
    if not target.exists():
        problems.append(f"{entry['id']}: proof path {path_part} does not exist")
        return problems
    if name_part and entry["kind"] == "cargo" and not cargo_test_exists(name_part):
        problems.append(
            f"{entry['id']}: no test named {name_part} exists, so this entry "
            "records a proof that cannot run"
        )
    if not entry["invariant"] or not entry["origin"]:
        problems.append(f"{entry['id']}: invariant and origin must both be stated")
    return problems


def main() -> int:
    if not LEDGER.is_file():
        print(f"invariant ledger: {LEDGER} is missing", file=sys.stderr)
        return 1
    text = LEDGER.read_text(encoding="utf-8")
    entries = rows(text)
    if not entries:
        print(
            "invariant ledger: zero entries parsed. An empty ledger passes "
            "vacuously, which is the exact failure this gate exists to refuse.",
            file=sys.stderr,
        )
        return 1
    if GAP_HEADING not in text:
        print(
            f"invariant ledger: missing the {GAP_HEADING!r} section. The honest "
            "gap list is mandatory, not optional.",
            file=sys.stderr,
        )
        return 1

    seen: set[str] = set()
    problems: list[str] = []
    for entry in entries:
        if entry["id"] in seen:
            problems.append(f"{entry['id']}: duplicate ledger id")
        seen.add(entry["id"])
        problems.extend(check_entry(entry))

    if problems:
        print("invariant ledger: entries whose proof does not exist", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1
    print(f"invariant ledger: {len(entries)} invariants, every proof present")
    return 0


if __name__ == "__main__":
    sys.exit(main())
