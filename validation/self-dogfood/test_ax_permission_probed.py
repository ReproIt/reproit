#!/usr/bin/env python3
"""Regression test: the AX gate PROVES Accessibility, it does not accept a claim.

`preflight.py --require-ax-permission` used to gate on
REPROIT_AX_PERMISSION_CONFIRMED=1, an environment variable the workflow itself
sets. It never asked macOS anything, so a runner whose grant was missing,
revoked, or attached to a different binary passed preflight and failed later
somewhere less obvious.

TCC makes that sharper than a generic assumption: the grant is evaluated per
process AT LAUNCH and attributed to the responsible app bundle, so it can lapse
with nothing in this repository changing. Rebuild or move the bundle, or restart
the service before the grant exists, and the attestation still reads "1".

The variable is kept as the ACKNOWLEDGEMENT, because a machine granting desktop
control to CI should say so deliberately. `AXIsProcessTrusted()` is the EVIDENCE.

This test runs everywhere, including on Linux and on a macOS host with no grant,
because it substitutes the probe rather than depending on the host's real TCC
state. A test that only passed on a permissioned machine would be one nobody
could trust anywhere else.
"""

from __future__ import annotations

import importlib.util
import os
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PREFLIGHT = ROOT / "validation/native/preflight.py"
ACKNOWLEDGEMENT = "REPROIT_AX_PERMISSION_CONFIRMED"


def load_preflight():
    spec = importlib.util.spec_from_file_location("reproit_preflight", PREFLIGHT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class AxPermissionProbedTests(unittest.TestCase):
    def setUp(self) -> None:
        self.preflight = load_preflight()
        self.saved = os.environ.get(ACKNOWLEDGEMENT)

    def tearDown(self) -> None:
        if self.saved is None:
            os.environ.pop(ACKNOWLEDGEMENT, None)
        else:
            os.environ[ACKNOWLEDGEMENT] = self.saved

    def test_an_untrusted_process_is_refused_even_when_the_variable_says_yes(self) -> None:
        # The whole point. Before this, the variable alone decided.
        os.environ[ACKNOWLEDGEMENT] = "1"
        self.preflight.ax_process_trusted = lambda: False
        with self.assertRaisesRegex(ValueError, "AXIsProcessTrusted"):
            self.preflight.require_ax_permission()

    def test_a_trusted_process_is_refused_without_the_deliberate_acknowledgement(self) -> None:
        # Granting a CI daemon control of the desktop should be stated on
        # purpose, so a real grant alone is deliberately not enough.
        os.environ.pop(ACKNOWLEDGEMENT, None)
        self.preflight.ax_process_trusted = lambda: True
        with self.assertRaisesRegex(ValueError, ACKNOWLEDGEMENT):
            self.preflight.require_ax_permission()

    def test_both_together_are_accepted(self) -> None:
        os.environ[ACKNOWLEDGEMENT] = "1"
        self.preflight.ax_process_trusted = lambda: True
        self.preflight.require_ax_permission()

    def test_the_probe_calls_the_real_api_rather_than_reading_the_variable(self) -> None:
        # Guards the obvious wrong fix: a probe that consults the same
        # environment variable would pass every test above while proving
        # nothing at all. It must reach ApplicationServices.
        source = PREFLIGHT.read_text(encoding="utf-8", errors="replace")
        self.assertIn("AXIsProcessTrusted", source)
        self.assertIn("ApplicationServices", source)


def main() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(AxPermissionProbedTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    # Case accounting: an empty or short run is the failure this file exists for.
    if result.testsRun != 4:
        print(f"expected 4 cases, ran {result.testsRun}", file=sys.stderr)
        return 1
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
