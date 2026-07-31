#!/usr/bin/env python3
"""Regression test: a CI gate depends on neither ambient env nor an optional dep.

Two gates were red on main for the same underlying reason, and both were green
on a developer laptop, which is why neither was noticed from local runs:

  1. `signature-parity` ran `node runners/signature_test.mjs`, which imports the
     host-pure signatureOf/descriptorOf from each runner bundle. The React
     Native bundle imported `webdriverio` at TOP LEVEL, so loading those pure
     functions required the Appium driver. That job installs no npm packages at
     all, so it failed with "Cannot find package 'webdriverio'". Locally it
     passed only because runners/rn/node_modules happened to exist.

  2. `sdk-backend-core` ran the Python SDK suite, which pins an exact
     `deployment` shape. `Capture.resolve_commit` deliberately falls back to
     REPROIT_COMMIT and then GITHUB_SHA, and GITHUB_SHA is always set on a
     GitHub runner, so the batch carried an extra `commit` key in CI and none
     locally.

The shared rule: a gate must state the environment it needs rather than inherit
it, and must not require an optional dependency to load code that does not use
it. This check is deliberately structural, in the same spirit as
check-harness-integrity.py: it cannot prove either gate is correct, it refuses
the exact shapes that already cost this project two silently red jobs.
"""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

RN_BUNDLE = ROOT / "runners/rn/runner.mjs"
PY_CONFTEST = ROOT / "sdk/reproit-backend-py/tests/conftest.py"
AMBIENT_CODE_IDENTITY = ("REPROIT_COMMIT", "GITHUB_SHA")

# A static ESM import of webdriverio, e.g. `import{remote as Yt}from"webdriverio"`.
STATIC_WEBDRIVERIO = re.compile(r"(?<!await )\bimport\s*\{[^}]*\}\s*from\s*[\"']webdriverio[\"']")
DYNAMIC_WEBDRIVERIO = re.compile(r"import\s*\(\s*[\"']webdriverio[\"']\s*\)")


class GateEnvironmentIndependenceTests(unittest.TestCase):
    def test_the_rn_bundle_loads_without_the_appium_driver(self) -> None:
        bundle = RN_BUNDLE.read_text(encoding="utf-8", errors="replace")
        self.assertIsNone(
            STATIC_WEBDRIVERIO.search(bundle),
            "runners/rn/runner.mjs imports webdriverio statically, so its pure "
            "signature exports cannot load without the Appium driver and the "
            "signature-parity job (which installs no npm packages) fails",
        )

    def test_the_rn_bundle_still_reaches_the_driver_when_it_needs_one(self) -> None:
        # Guards the obvious wrong fix: deleting the import instead of deferring
        # it, which would pass the check above and break every real Appium run.
        bundle = RN_BUNDLE.read_text(encoding="utf-8", errors="replace")
        self.assertIsNotNone(
            DYNAMIC_WEBDRIVERIO.search(bundle),
            "runners/rn/runner.mjs no longer imports webdriverio at all; the "
            "session path needs it, so the static import must become a dynamic "
            "one, not disappear",
        )

    def test_the_python_capture_suite_clears_ambient_code_identity(self) -> None:
        self.assertTrue(
            PY_CONFTEST.is_file(),
            "sdk/reproit-backend-py/tests/conftest.py is missing, so the suite "
            "inherits GITHUB_SHA from the runner and its exact-shape deployment "
            "assertions fail in CI while passing locally",
        )
        conftest = PY_CONFTEST.read_text(encoding="utf-8", errors="replace")
        for name in AMBIENT_CODE_IDENTITY:
            self.assertIn(
                name,
                conftest,
                f"the conftest does not neutralize {name}, which the SDK reads "
                "as code identity",
            )
        self.assertIn("delenv", conftest, "the conftest names the variables but never clears them")


def main() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(GateEnvironmentIndependenceTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    # Case accounting: an empty or short run is the failure this file exists for.
    if result.testsRun != 3:
        print(f"expected 3 cases, ran {result.testsRun}", file=sys.stderr)
        return 1
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
