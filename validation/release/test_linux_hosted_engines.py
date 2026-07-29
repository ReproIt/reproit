#!/usr/bin/env python3
"""Regression tests for Linux hosted browser dependency selection."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path

RELEASE = Path(__file__).resolve().parent
SELECTOR = RELEASE / "linux_hosted_engines.py"
COLLECTOR = RELEASE / "run-linux-x86-remote.sh"
SPEC = importlib.util.spec_from_file_location("linux_hosted_engines", SELECTOR)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SELECTOR}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class LinuxHostedEngineTests(unittest.TestCase):
    def test_isolated_backend_contract_selects_chromium(self) -> None:
        self.assertEqual(MODULE.required_engines("backend-contract"), ["chromium"])

    def test_combined_browser_gates_select_each_engine_once(self) -> None:
        gates = "web-engines,backend-contract,web-chromium,electron"
        self.assertEqual(
            MODULE.required_engines(gates),
            ["chromium", "firefox", "webkit"],
        )

    def test_non_browser_gate_selects_no_engine(self) -> None:
        self.assertEqual(MODULE.required_engines("tui-pty"), [])

    def test_cli_emits_one_engine_per_line(self) -> None:
        result = subprocess.run(
            [sys.executable, str(SELECTOR), "backend-contract,web-engines"],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.stdout, "chromium\nfirefox\nwebkit\n")

    def test_remote_worker_uses_the_tested_selector(self) -> None:
        source = COLLECTOR.read_text(encoding="utf-8")
        self.assertIn(
            'python3 validation/release/linux_hosted_engines.py '
            '"$REPROIT_HOSTED_GATES"',
            source,
        )


if __name__ == "__main__":
    unittest.main()
