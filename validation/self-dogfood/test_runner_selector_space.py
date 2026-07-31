#!/usr/bin/env python3
"""`role:<role>#<idx>` must denote ONE element per DOM runner.

The selector is the product's element identity: a snapshot ASSIGNS the index,
and a later tap, type, or clip-box resolution READS it back. Those halves ran
different code. Twelve copies of the in-page `interactive()` predicate carried
four incompatible tappable grammars, and the Electron and Tauri runners each
shipped more than one at once, so `role:textfield#N` named different elements
inside a single run: one that a finding was recorded against, another that a
replay then drove. A selector that means two things is not an identity, and the
whole reproduction claim rests on it.

The content-bug oracle had the same shape of defect. The web copy keyed an
unkeyed element by its document-order tag index; the Electron and Tauri copies
did `if (!key) continue`. So a bare `<span>[object Object]</span>` produced no
finding on two of the three DOM runners, while `content-bug` was declared
evaluated on all nine in validation/oracles/coverage.json. An absent finding
reads exactly like a clean result, which is the failure this product exists to
prevent.

This gate is structural and dependency-free (it must run in a bare worktree),
and it checks the SHIPPED bundles as well as the sources, so a rebuild cannot
smuggle a second grammar back in. The behavioural half -- that the shared
predicate actually selects the elements it claims to, in a real browser -- is
runners/web/shared-dom-walk.test.mjs.

Run: python3 validation/self-dogfood/test_runner_selector_space.py
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "runners" / "source"
SHARED = SOURCE / "shared" / "dom-walk.mjs"
BUNDLES = (
    ROOT / "runners" / "web" / "runner.mjs",
    ROOT / "runners" / "electron.mjs",
    ROOT / "runners" / "tauri.mjs",
    ROOT / "runners" / "tauri-snapshot.mjs",
    ROOT / "runners" / "shared" / "dom-walk.mjs",
)
# The rejected tappable grammar, verbatim. It excluded five `<input>` types from
# the index space, which made those fields unaddressable and therefore any
# finding on one unreportable; it was also not the rule it claimed to be, since
# `<textarea>` and `<select>` still entered through the tabIndex fallback.
REJECTED_GRAMMAR = re.compile(
    r"!\s*\[\s*'text'\s*,\s*'password'\s*,\s*'email'\s*,\s*'number'\s*,\s*'search'\s*\]"
    r"\s*\.\s*includes"
)
DEFINITION = re.compile(r"(?:const\s+interactive\s*=\s*\(|function\s+interactive\s*\()")


def strip_comments(text: str) -> str:
    """Drop // line comments outside string literals, and /* */ blocks."""
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    out = []
    for line in text.splitlines():
        quote = None
        cut = len(line)
        i = 0
        while i < len(line) - 1:
            char = line[i]
            if quote:
                if char == "\\":
                    i += 2
                    continue
                if char == quote:
                    quote = None
            elif char in "\"'`":
                quote = char
            elif char == "/" and line[i + 1] == "/":
                cut = i
                break
            i += 1
        out.append(line[:cut])
    return "\n".join(out)


def body_at(text: str, start: int) -> str:
    """The brace-matched function body beginning at or after `start`."""
    open_at = text.index("{", start)
    depth = 0
    for i in range(open_at, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[open_at : i + 1]
    raise AssertionError("unterminated function body")


def normalize(body: str) -> str:
    return re.sub(r"\s+", " ", strip_comments(body)).strip()


def predicates() -> dict[str, list[str]]:
    """Every authored `interactive` predicate, keyed by its normalized body."""
    found: dict[str, list[str]] = {}
    for path in sorted(SOURCE.rglob("*.mjs")):
        text = path.read_text(encoding="utf8")
        for match in DEFINITION.finditer(strip_comments(text)):
            body = normalize(body_at(strip_comments(text), match.start()))
            line = strip_comments(text)[: match.start()].count("\n") + 1
            found.setdefault(body, []).append(
                f"{path.relative_to(ROOT).as_posix()}:{line}"
            )
    return found


class SelectorSpaceTests(unittest.TestCase):
    def test_one_tappable_grammar_across_every_dom_runner(self) -> None:
        found = predicates()
        self.assertTrue(found, "no interactive() predicate found at all")
        self.assertEqual(
            len(found),
            1,
            "role:<role>#<idx> must mean one thing. Found "
            f"{len(found)} tappable grammars: "
            + "; ".join(sorted(sites[0] for sites in found.values())),
        )

    def test_the_one_grammar_is_the_shared_one(self) -> None:
        shared = strip_comments(SHARED.read_text(encoding="utf8"))
        match = DEFINITION.search(shared)
        self.assertIsNotNone(match, f"{SHARED} defines no interactive() predicate")
        canonical = normalize(body_at(shared, match.start()))
        for body, sites in predicates().items():
            self.assertEqual(
                body, canonical, f"{sites[0]} does not match shared/dom-walk.mjs"
            )

    def test_text_fields_occupy_a_slot_in_the_index_space(self) -> None:
        shared = strip_comments(SHARED.read_text(encoding="utf8"))
        canonical = normalize(body_at(shared, DEFINITION.search(shared).start()))
        self.assertIn(
            "tag === 'input' || tag === 'textarea'",
            canonical,
            "a text field must be addressable, or a finding on one is unreportable",
        )

    def test_no_shipped_bundle_carries_the_rejected_grammar(self) -> None:
        for bundle in BUNDLES:
            self.assertTrue(bundle.exists(), f"{bundle} is not built")
            self.assertIsNone(
                REJECTED_GRAMMAR.search(bundle.read_text(encoding="utf8")),
                f"{bundle.relative_to(ROOT)} still ships the rejected tappable "
                "grammar, so its selector space disagrees with the others",
            )

    def test_the_content_bug_oracle_has_one_authored_copy(self) -> None:
        sites = [
            path.relative_to(ROOT).as_posix()
            for path in sorted(SOURCE.rglob("*.mjs"))
            if re.search(
                r"function detectContentBugs\s*\(",
                strip_comments(path.read_text(encoding="utf8")),
            )
        ]
        self.assertEqual(
            sites,
            ["runners/source/shared/dom-walk.mjs"],
            "detectContentBugs must have exactly one authored copy, in shared/",
        )

    def test_an_unkeyed_element_still_produces_a_content_bug_finding(self) -> None:
        shared = strip_comments(SHARED.read_text(encoding="utf8"))
        self.assertIn(
            "'tag:' + tag + '#' + n",
            shared,
            "an unkeyed <span>[object Object]</span> must not be silently dropped",
        )
        self.assertNotIn(
            "const key = keyOf(el);\n    if (!key) continue;",
            shared,
            "the keyed-only scan is the defect, not the fix",
        )
        for bundle in BUNDLES:
            text = bundle.read_text(encoding="utf8")
            if "detectContentBugs" not in text and "object Object" not in text:
                continue
            self.assertIn(
                "tag:",
                text,
                f"{bundle.relative_to(ROOT)} scans content bugs without the "
                "unkeyed tag-index fallback",
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
