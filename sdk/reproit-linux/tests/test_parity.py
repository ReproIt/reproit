#!/usr/bin/env python3
"""Cross-language parity gate for the Linux SDK.

Loads the canonical golden vectors (signature_vectors.json at the repo root) and
asserts that reproit_linux.signature reproduces every `expected_sig`
bit-for-bit, exactly like the Rust oracle's tests::golden_vectors_match
(crates/reproit/src/model/signature.rs) and runners/test_signature.py.

The signature core has NO GTK/AT-SPI import, so this test runs on any host with
python3 (the live capture cannot be exercised headless; that is unit-tested
separately in test_capture.py with synthetic trees).

Run:
    python3 sdk/reproit-linux/tests/test_parity.py
"""

import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SDK_ROOT = os.path.dirname(HERE)        # sdk/reproit-linux

# Load the signature core module by path. We load the FILE directly (not via the
# `reproit_linux` package) so importing it never pulls in the GTK/AT-SPI capture
# layer, exactly like runners/test_signature.py loads the runner's core. (The
# package __init__ also re-exports a `signature` function, so `import
# reproit_linux.signature` is ambiguous; loading the file by path is clean.)
def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


sig = _load("reproit_linux_signature", os.path.join(SDK_ROOT, "reproit_linux", "signature.py"))


def find_vectors():
    """Locate signature_vectors.json. The task references it as
    `../../signature_vectors.json` relative to the SDK root (sdk/reproit-linux),
    which resolves to the repo root. We walk up from the SDK root to be robust to
    where the package is checked out."""
    # ../../ from the SDK root is the repo root.
    candidate = os.path.normpath(os.path.join(SDK_ROOT, "..", "..", "signature_vectors.json"))
    if os.path.exists(candidate):
        return candidate
    d = SDK_ROOT
    for _ in range(6):
        d = os.path.dirname(d)
        p = os.path.join(d, "signature_vectors.json")
        if os.path.exists(p):
            return p
    raise FileNotFoundError("signature_vectors.json not found above %s" % SDK_ROOT)


def main():
    path = find_vectors()
    with open(path, "r", encoding="utf-8") as f:
        vectors = json.load(f)

    if len(vectors) != 25:
        print("FAIL: expected exactly 25 golden vectors, got %d" % len(vectors))
        return 1

    failures = []
    for v in vectors:
        anchor = v.get("anchor")  # str | None
        tree = sig.Node.from_json(v["tree"])
        got = sig.signature(anchor, tree)
        expected = v["expected_sig"]
        if got != expected:
            failures.append(
                "  %s\n      descriptor = %r\n      expected %s got %s" % (
                    v.get("description", "<no description>"),
                    sig.descriptor(anchor, tree), expected, got,
                )
            )

    passed = len(vectors) - len(failures)
    status = "PASS" if not failures else "FAIL"
    print("%s: reproit-linux %d/%d vectors" % (status, passed, len(vectors)))

    if failures:
        print("\nMismatches:")
        for fl in failures:
            print(fl)
        return 1

    print("\nAll %d golden vectors reproduce byte-for-byte (reproit-linux)." % len(vectors))
    return 0


if __name__ == "__main__":
    sys.exit(main())
