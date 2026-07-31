#!/usr/bin/env python3
"""Negative controls for the per-runner oracle coverage ledger."""

from __future__ import annotations

import copy
import importlib.util
import json
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check.py")
LEDGER = Path(__file__).with_name("coverage.json")
SPEC = importlib.util.spec_from_file_location("oracle_check", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SCRIPT}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class OracleCoverageLedgerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ledger = json.loads(LEDGER.read_text(encoding="utf-8"))

    def row(self, ledger: dict, marker: str) -> dict:
        return next(row for row in ledger["oracles"] if row["marker"] == marker)

    def test_current_ledger_states_every_pair(self) -> None:
        self.assertEqual(MODULE.validate(self.ledger), [])
        pairs = sum(
            len(row["evaluated"]) + len(row["unavailable"]) + len(row["unimplemented"])
            for row in self.ledger["oracles"]
        )
        self.assertEqual(pairs, len(self.ledger["oracles"]) * len(self.ledger["runners"]))

    def test_a_new_parsed_marker_cannot_arrive_without_a_row(self) -> None:
        original = MODULE.parsed_markers
        MODULE.parsed_markers = lambda: original() | {"EXPLORE:ZERODAY"}
        try:
            errors = MODULE.validate(self.ledger)
            self.assertTrue(any("EXPLORE:ZERODAY" in error for error in errors), errors)
        finally:
            MODULE.parsed_markers = original

    def test_an_unstated_runner_fails_closed(self) -> None:
        ledger = copy.deepcopy(self.ledger)
        row = self.row(ledger, "EXPLORE:DUPSUBMIT")
        row["unavailable"].pop("tauri")
        errors = MODULE.validate(ledger)
        self.assertTrue(
            any("EXPLORE:DUPSUBMIT/tauri: stated 0 times" in error for error in errors),
            errors,
        )

    def test_claiming_an_oracle_a_runner_does_not_emit_fails(self) -> None:
        ledger = copy.deepcopy(self.ledger)
        row = self.row(ledger, "EXPLORE:DUPSUBMIT")
        row["unavailable"].pop("tauri")
        row["evaluated"]["tauri"] = "runners/source/tauri/part-01.mjs"
        errors = MODULE.validate(ledger)
        self.assertTrue(
            any("claims evaluated but emits no marker" in error for error in errors),
            errors,
        )

    def test_declaring_a_gap_a_runner_actually_covers_fails(self) -> None:
        ledger = copy.deepcopy(self.ledger)
        row = self.row(ledger, "EXPLORE:DUPSUBMIT")
        row["evaluated"].pop("web")
        row["unimplemented"].append("web")
        errors = MODULE.validate(ledger)
        self.assertTrue(
            any("declared unimplemented but" in error for error in errors), errors
        )

    def test_an_unavailable_claim_needs_a_known_reason(self) -> None:
        ledger = copy.deepcopy(self.ledger)
        self.row(ledger, "EXPLORE:DUPSUBMIT")["unavailable"]["tauri"] = "because"
        errors = MODULE.validate(ledger)
        self.assertTrue(any("unknown reason id because" in error for error in errors), errors)

    def test_a_new_platform_needs_a_runner(self) -> None:
        original = MODULE.registered_platforms
        MODULE.registered_platforms = lambda: original() | {"holographic"}
        try:
            errors = MODULE.validate(self.ledger)
            self.assertTrue(any("holographic" in error for error in errors), errors)
        finally:
            MODULE.registered_platforms = original


if __name__ == "__main__":
    unittest.main()
