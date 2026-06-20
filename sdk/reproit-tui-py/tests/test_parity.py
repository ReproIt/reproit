#!/usr/bin/env python3
"""Canonical TUI screen-signature PARITY gate for the reproit-tui Python SDK.

This is the TUI mirror of the Rust crate's own tests (crates/tui-sig/src/lib.rs),
the Go SDK gate (sdk/reproit-tui-go/signature_test.go), and the a11y parity gates
(runners/test_signature.py, sdk/test/signature_test.js). It LOADS
tui_signature_vectors.json (at the repo root) and, for every vector, asserts that
the SDK's structural_sig(contents, cursor) and content_fingerprint(contents,
cursor) equal the values the REAL Rust code produced for the same screen (see the
JSON's _derivation note). That proves the SDK's screen descriptor + hashing is
byte-for-byte identical to the runner's, in the TUI namespace (NOT
signature_vectors.json, which is the a11y Node-tree namespace).

It also asserts the cross-vector relationships the spec promises (locale
invariance, POS1 collapse, cursor-as-structure, value-only effect) and unit-tests
the capture-to-text mapping with synthetic screens.

Run:
    python3 sdk/reproit-tui-py/tests/test_parity.py

No em dashes anywhere, per project rules.
"""

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PKG_PARENT = os.path.dirname(HERE)  # sdk/reproit-tui-py
# repo root is two levels up from the package dir (sdk/reproit-tui-py -> sdk -> root)
REPO_ROOT = os.path.dirname(os.path.dirname(PKG_PARENT))
VECTORS_PATH = os.path.join(REPO_ROOT, "tui_signature_vectors.json")

# Import the SDK package whether the test is run from the repo root or in place.
sys.path.insert(0, PKG_PARENT)
from reproit_tui_py import (  # noqa: E402
    structural_sig,
    content_fingerprint,
    value_class,
    numeric_value_classes,
    skeleton_of,
    labels_of,
    ScreenContents,
    Cell,
    MAX_VALUE_CLASSES,
)


def load_vectors():
    with open(VECTORS_PATH, "r", encoding="utf-8") as f:
        return json.load(f)["vectors"]


def run_golden(vectors):
    """Assert structural_sig == expected_sig and content_fingerprint ==
    expected_fp for every vector. Returns (passed, failures)."""
    failures = []
    for v in vectors:
        contents = v["contents"]
        cursor = (v["cursor"][0], v["cursor"][1])
        got_sig = structural_sig(contents, cursor)
        got_fp = content_fingerprint(contents, cursor)
        if got_sig != v["expected_sig"]:
            failures.append(
                "  [%s] sig: expected %s got %s\n      skeleton=%r"
                % (v["name"], v["expected_sig"], got_sig, skeleton_of(contents))
            )
        if got_fp != v["expected_fp"]:
            failures.append(
                "  [%s] fp: expected %s got %s"
                % (v["name"], v["expected_fp"], got_fp)
            )
    return (len(vectors) - len(failures), failures)


def run_relationships(by_name):
    """Lock in the cross-vector facts the spec promises (the JSON's
    _relationships note). Returns a list of failure strings."""
    fails = []

    def sig(name):
        v = by_name[name]
        return structural_sig(v["contents"], (v["cursor"][0], v["cursor"][1]))

    def fp(name):
        v = by_name[name]
        return content_fingerprint(v["contents"], (v["cursor"][0], v["cursor"][1]))

    def check(label, cond):
        if not cond:
            fails.append("  relationship: %s" % label)

    # locale invariance: the same layout in EN and DE hashes the same.
    check("login_en sig == login_de sig (locale-invariant)", sig("login_en") == sig("login_de"))
    # but the display-only fingerprints differ (raw words differ).
    check("login_en fp != login_de fp (raw words differ)", fp("login_en") != fp("login_de"))
    # POS1 collapse: 1, 3, 7 all bucket POS1 -> one sig.
    check("count1 == count3 == count7 (all POS1)",
          sig("count1") == sig("count3") == sig("count7"))
    # distinct buckets: 0 (ZERO), 1 (POS1), 12 (POS2) are three distinct sigs.
    check("count0 != count1 != count12 (three buckets)",
          len({sig("count0"), sig("count1"), sig("count12")}) == 3)
    # same POS3 bucket + same skeleton -> identical sig, but fingerprints DIFFER.
    check("hits100 sig == hits101 sig (POS3 bucket)", sig("hits100") == sig("hits101"))
    check("hits100 fp != hits101 fp (value-only effect)", fp("hits100") != fp("hits101"))
    # cursor cell is structural: same screen, different focused row -> different sig.
    check("login_en sig != login_en_cur3 sig (cursor is structure)",
          sig("login_en") != sig("login_en_cur3"))

    return fails


def run_value_class_checks():
    """Lock in the value_class buckets and the bounded numeric_value_classes
    extraction (the same checks the Rust crate's tests assert). Returns a list of
    failure strings."""
    fails = []

    def check(label, got, want):
        if got != want:
            fails.append("  value-state: %s: expected %r got %r" % (label, want, got))

    cases = {
        "": "EMPTY", "   ": "EMPTY", "0": "ZERO", "0.0": "ZERO", "-0": "ZERO",
        "-3": "NEG", "-0.5": "NEG", "3": "POS1", "9.99": "POS1", "+7": "POS1",
        "10": "POS2", "99": "POS2", "100": "POS3", "999.99": "POS3",
        "1000": "POSL", "123456": "POSL", "  42  ": "POS2",
        "1,234": "NONEMPTY", "1.234.567": "NONEMPTY", "$5": "NONEMPTY",
        "5%": "NONEMPTY", "1e3": "NONEMPTY", "3.": "NONEMPTY", ".5": "NONEMPTY",
        "--5": "NONEMPTY", "hello": "NONEMPTY",
    }
    for s, want in cases.items():
        check("value_class(%r)" % s, value_class(s), want)

    # numeric_value_classes scan + sort + bound (matches the Rust crate test).
    check("nvc('0')", numeric_value_classes("0"), ["ZERO"])
    check("nvc('-3')", numeric_value_classes("-3"), ["NEG"])
    check("nvc('42')", numeric_value_classes("42"), ["POS2"])
    check("nvc('1000')", numeric_value_classes("1000"), ["POSL"])
    check("nvc('a 7 b 0 c 50')", numeric_value_classes("a 7 b 0 c 50"),
          ["POS1", "POS2", "ZERO"])
    # "1,234" splits on the comma (not a strict-decimal char): "1" -> POS1, then
    # "234" -> POS3, sorted -> ["POS1", "POS3"]. This matches the Rust scan, which
    # also splits grouped numbers on the comma.
    check("nvc('1,234') splits on comma", numeric_value_classes("1,234"),
          ["POS1", "POS3"])
    check("nvc('no numbers here') empty", numeric_value_classes("no numbers here"), [])
    many = "".join("%d " % n for n in range(50))
    check("nvc bounded at MAX_VALUE_CLASSES", len(numeric_value_classes(many)),
          MAX_VALUE_CLASSES)

    return fails


def run_capture_checks():
    """Unit-test the capture-to-text mapping with synthetic screens: grid trailing
    trim, trailing-row trim, gap spaces, wide-cell spacer skip, from_rows, and
    from_text passthrough. Returns a list of failure strings."""
    fails = []

    def check(label, got, want):
        if got != want:
            fails.append("  capture: %s: expected %r got %r" % (label, want, got))

    # from_text is verbatim passthrough.
    check("from_text passthrough", ScreenContents.from_text("a\nb\n", (1, 0)).text(), "a\nb\n")
    check("from_text cursor", ScreenContents.from_text("x", (3, 5)).cursor, (3, 5))

    # from_rows: a string row's characters become written cells; a written space
    # is real content (only never-written trailing cells are trimmed), so internal
    # spaces are preserved exactly as vt100 keeps written cells.
    sc = ScreenContents.from_rows(["a b", "cd"], (0, 0))
    check("from_rows preserves written spaces", sc.text(), "a b\ncd")

    # trailing blank rows (rows with no written cells) are trimmed off the whole
    # string, matching vt100 screen().contents() trailing-blank-row trimming.
    sc = ScreenContents.from_rows(["hi", "", ""], (0, 0))
    check("from_rows trailing-row trim", sc.text(), "hi")

    # a gap (empty cell) BEFORE a later non-empty cell becomes a space.
    grid = [[Cell("a"), Cell(""), Cell("b")]]
    check("gap before non-empty -> space", ScreenContents(grid=grid).text(), "a b")

    # a wide cell occupies two columns: emit its grapheme once, skip the spacer.
    grid = [[Cell("欢", wide=True), Cell(""), Cell("x")]]
    check("wide cell skips spacer column", ScreenContents(grid=grid).text(), "欢x")

    # the synthetic grid reproduces the login_en vector's contents string, proving
    # the capture model feeds the signature the same bytes the runner hashes.
    login_rows = [
        "┌─────────┐",
        "│ Login    │",
        "│ User:    │",
        "│ Pass:    │",
        "│ [o] Okay │",
        "└─────────┘",
    ]
    sc = ScreenContents.from_rows(login_rows, (2, 8))
    golden_contents = (
        "┌─────────┐\n│ Login    │\n│ User:    │\n│ Pass:    │\n│ [o] Okay │\n└─────────┘\n"
    )
    # The cell grid (6 rows, no blank trailing row) reproduces the golden contents
    # MINUS its single trailing newline: vt100 screen().contents() trims trailing
    # blank rows, so a grid with no blank row below cannot carry that final '\n'.
    # The golden string carries it because the captured screen had a blank line
    # below the box; the from_text path (the documented Rich-export path) preserves
    # it verbatim. Both sign to the golden once the same bytes are present.
    check("login grid -> contents (trailing-blank-row trimmed)",
          sc.text(), golden_contents.rstrip("\n"))
    # The from_text path is verbatim, so feeding the exact golden contents string
    # (trailing '\n' included) signs to the golden login_en signature.
    sc_text = ScreenContents.from_text(golden_contents, (2, 8))
    check("login from_text -> structural_sig",
          structural_sig(sc_text.text(), sc_text.cursor), "2d66ce98")

    return fails


def main():
    vectors = load_vectors()
    by_name = {v["name"]: v for v in vectors}
    if len(vectors) < 18:
        print("FAIL: need >= 18 golden TUI vectors, got %d" % len(vectors))
        return 1

    all_failures = []

    passed, failures = run_golden(vectors)
    all_failures.extend(failures)
    status = "PASS" if not failures else "FAIL"
    print("%s: golden vectors %d/%d (sig + fp)" % (status, passed, len(vectors)))

    rfails = run_relationships(by_name)
    all_failures.extend(rfails)
    print("%s: cross-vector relationships" % ("PASS" if not rfails else "FAIL"))

    vfails = run_value_class_checks()
    all_failures.extend(vfails)
    print("%s: value-class buckets + bounded numeric extraction"
          % ("PASS" if not vfails else "FAIL"))

    cfails = run_capture_checks()
    all_failures.extend(cfails)
    print("%s: capture-to-text mapping (synthetic screens)"
          % ("PASS" if not cfails else "FAIL"))

    if all_failures:
        print("\nMismatches:")
        for f in all_failures:
            print(f)
        return 1

    print("\nAll %d golden TUI vectors pass (structural_sig == expected_sig and "
          "content_fingerprint == expected_fp), plus cross-vector relationships, "
          "value-class buckets, and the capture-to-text mapping." % len(vectors))
    return 0


if __name__ == "__main__":
    sys.exit(main())
