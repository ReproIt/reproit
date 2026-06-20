#!/usr/bin/env python3
"""Cross-language parity gate for the Python desktop runners.

Loads the canonical golden vectors (`signature_vectors.json` at the repo root)
and asserts that BOTH Python runners' signature functions reproduce every
`expected_sig` bit-for-bit, exactly like the Rust oracle's
`tests::golden_vectors_match` (crates/reproit/src/model/signature.rs).

The runner modules are named with hyphens (`windows-uia.py`, `linux-atspi.py`),
which are not importable as normal module names, so they are loaded by path via
importlib. Crucially, both runners defer their platform import (`uiautomation` /
`gi`+`Atspi`) into `main()`, so importing them here just to reach the signature
core works on any OS, with no Windows/Linux a11y stack present.

Run:
    python3 runners/test_signature.py
"""

import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(HERE)
VECTORS_PATH = os.path.join(REPO_ROOT, "signature_vectors.json")


def load_module(name, filename):
    """Load a runner module by file path (handles the hyphenated filenames)."""
    path = os.path.join(HERE, filename)
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def load_vectors():
    with open(VECTORS_PATH, "r", encoding="utf-8") as f:
        return json.load(f)


def run_runner(name, mod, vectors):
    """Assert mod.signature(anchor, Node) == expected_sig for every vector.
    Returns (passed, failures)."""
    failures = []
    for v in vectors:
        anchor = v.get("anchor")  # str | None
        tree = mod.Node.from_json(v["tree"])
        got = mod.signature(anchor, tree)
        expected = v["expected_sig"]
        if got != expected:
            failures.append(
                "  [{}] {}\n      descriptor = {!r}\n      expected {} got {}".format(
                    name,
                    v.get("description", "<no description>"),
                    mod.descriptor(anchor, tree),
                    expected,
                    got,
                )
            )
    return (len(vectors) - len(failures), failures)


def run_value_state_checks(name, mod):
    """Lock in the Layer 1/2/3 behavior the runners add on top of the canonical
    golden vectors (these exercise the value-state plumbing that the live capture
    feeds, which the OS-specific tree walk cannot be unit-tested without a host).
    Returns a list of failure strings."""
    fails = []

    def check(label, got, want):
        if got != want:
            fails.append("  [{}] value-state: {}\n      expected {!r} got {!r}".format(
                name, label, want, got))

    Node = mod.Node

    # value_class buckets (locale-safe, strict period decimal).
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
        check("value_class(%r)" % s, mod.value_class(s), want)

    # A value-less / chrome tree is byte-identical to the pre-value-state form.
    tf = Node(role="textfield", id="email")
    check("value-less textfield body", mod.descriptor(None, tf), "A:\n0:textfield@email")
    hdr = Node(role="header", id="title", value="Welcome")  # chrome carrying a value
    check("chrome header ignores value", mod.descriptor(None, hdr), "A:\n0:header@title")

    # A value-bearing node emits a V: section; a status node normalizes to node.
    tfv = Node(role="textfield", id="email", value="a@b.com")
    check("textfield V: section", mod.descriptor(None, tfv),
          "A:\n0:textfield@email\nV:key:email=NONEMPTY")
    st = Node(role="status", id="count", value="5")
    check("status node V: section", mod.descriptor(None, st),
          "A:\n0:node@count\nV:key:count=POS1")

    # Layer 3 opt-in: a chrome `text` is not value-bearing unless value_node set.
    t = Node(role="text", id="display", value="42")
    check("opt-out text body", mod.descriptor(None, t), "A:\n0:text@display")
    t2 = Node(role="text", id="display", value="42", value_node=True)
    check("opt-in value_node V:", mod.descriptor(None, t2),
          "A:\n0:text@display\nV:key:display=POS2")

    # apply_value_nodes resolves a selector to the flag (Layer 3 config path).
    screen = Node(role="screen", children=[Node(role="text", id="score", value="7")])
    mod.apply_value_nodes(screen, ["key:score"])
    check("apply_value_nodes selector", mod.descriptor(None, screen),
          "A:\n0:screen;1:text@score\nV:key:score=POS1")

    # Layer 1 content fingerprint distinguishes two same-bucket values (5 vs 6,
    # both POS1) even though the canonical signature is identical.
    a = Node(role="status", id="count", value="5")
    b = Node(role="status", id="count", value="6")
    same_sig = mod.signature(None, a) == mod.signature(None, b)
    diff_content = mod.content_fingerprint(None, a) != mod.content_fingerprint(None, b)
    check("Layer1: 5 vs 6 same canonical sig", same_sig, True)
    check("Layer1: 5 vs 6 differ in content fingerprint", diff_content, True)

    # Layer 2 runner cap: > 8 DISTINCT value-class combinations of one structural
    # node fall back to the structural-only signature. A single value node tops
    # out at the 8 bucket count, so the cap is exercised with TWO value children
    # whose bucket combinations exceed 8.
    def two_field_screen(va, vb):
        return Node(role="screen", children=[
            Node(role="textfield", id="a", value=va),
            Node(role="textfield", id="b", value=vb),
        ])

    capn = mod.ValueCap()
    struct_only = mod.signature(None, Node(role="screen", children=[
        Node(role="textfield", id="a"), Node(role="textfield", id="b"),
    ]))
    buckets = ["", "0", "-3", "3", "10", "100", "1000", "abc"]  # 8 distinct buckets
    # 9 distinct (a,b) bucket combinations -> the 9th blows the cap of 8: eight
    # combos vary `a` over all buckets (b fixed), then a ninth flips `b`.
    combos = [(buckets[k], buckets[0]) for k in range(8)] + [(buckets[0], buckets[1])]
    distinct = len(set((mod.value_class(a), mod.value_class(b)) for a, b in combos))
    sigs = [capn.effective_signature(None, two_field_screen(a, b)) for a, b in combos]
    check("ValueCap test data has >8 distinct combos", distinct >= 9, True)
    check("ValueCap keeps value sig under cap", sigs[0] != struct_only, True)
    check("ValueCap returns structural-only after cap", sigs[-1], struct_only)
    # Once capped, the node stays structural-only even for an already-seen combo.
    check("ValueCap is sticky once blown",
          capn.effective_signature(None, two_field_screen(buckets[0], buckets[0])), struct_only)

    return fails


def main():
    vectors = load_vectors()
    if len(vectors) != 24:
        print("FAIL: expected exactly 24 golden vectors, got {}".format(len(vectors)))
        return 1

    runners = [
        ("windows-uia", load_module("reproit_windows_uia", "windows-uia.py")),
        ("linux-atspi", load_module("reproit_linux_atspi", "linux-atspi.py")),
    ]

    all_failures = []
    for name, mod in runners:
        passed, failures = run_runner(name, mod, vectors)
        all_failures.extend(failures)
        status = "PASS" if not failures else "FAIL"
        print("{}: {} {}/{} vectors".format(status, name, passed, len(vectors)))

    # Value-state plumbing checks (Layer 1/2/3) on top of the canonical vectors.
    for name, mod in runners:
        vfails = run_value_state_checks(name, mod)
        all_failures.extend(vfails)
        status = "PASS" if not vfails else "FAIL"
        print("{}: {} value-state plumbing (Layer 1/2/3)".format(status, name))

    if all_failures:
        print("\nMismatches:")
        for f in all_failures:
            print(f)
        return 1

    print(
        "\nAll {} vectors pass for both runners (windows-uia, linux-atspi), "
        "plus Layer 1/2/3 value-state plumbing.".format(len(vectors))
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
