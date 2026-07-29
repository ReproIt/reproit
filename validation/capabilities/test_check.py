#!/usr/bin/env python3
"""Regression tests for the capture-to-replay capability ledger."""

from __future__ import annotations

import copy
import importlib.util
import json
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check.py")
LEDGER = Path(__file__).with_name("coverage.json")
SPEC = importlib.util.spec_from_file_location("capability_check", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SCRIPT}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class CapabilityLedgerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ledger = json.loads(LEDGER.read_text(encoding="utf-8"))

    def test_current_ledger_is_complete_and_honest(self) -> None:
        report = MODULE.validate(self.ledger)
        self.assertEqual(report["capabilities"], 22)

    def test_new_protocol_capability_cannot_be_silently_omitted(self) -> None:
        original = MODULE.protocol_capabilities
        MODULE.protocol_capabilities = lambda: original() + ["zero-day-channel"]
        try:
            with self.assertRaisesRegex(MODULE.LedgerError, "zero-day-channel"):
                MODULE.validate(self.ledger)
        finally:
            MODULE.protocol_capabilities = original

    def test_incomplete_claim_requires_a_named_blocker(self) -> None:
        ledger = copy.deepcopy(self.ledger)
        ledger["claims"][0]["blockers"] = []
        with self.assertRaisesRegex(MODULE.LedgerError, "declares no blocker"):
            MODULE.validate(ledger)

    def test_missing_evidence_fails_closed(self) -> None:
        ledger = copy.deepcopy(self.ledger)
        ledger["claims"][0]["evidence"] = ["validation/capabilities/missing.json"]
        with self.assertRaisesRegex(MODULE.LedgerError, "does not exist"):
            MODULE.validate(ledger)


if __name__ == "__main__":
    unittest.main()
