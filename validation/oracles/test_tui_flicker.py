#!/usr/bin/env python3
"""Native PTY positive, fixed, and adversarial controls for TUI flicker."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = Path(__file__).with_name("fixtures") / "tui_flicker.py"
BIN = ROOT / "target" / "debug" / "reproit"


def run_fixture(mode: str) -> str:
    with tempfile.TemporaryDirectory(prefix="reproit-tui-flicker-") as work:
        config = Path(work) / "fuzz.json"
        config.write_text(json.dumps({"replay": ["key:a"]}), encoding="utf-8")
        env = {
            **os.environ,
            "REPROIT_TUI_CMD": f"{sys.executable} {FIXTURE} {mode}",
            "REPROIT_FUZZ_CONFIG": str(config),
        }
        result = subprocess.run(
            [str(BIN), "__tui"],
            env=env,
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        )
        return result.stdout


class TuiFlickerNativeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not BIN.is_file():
            raise unittest.SkipTest(f"build {BIN} before the native PTY gate")

    def test_persisted_transient_overshoot_fires(self) -> None:
        output = run_fixture("positive")
        self.assertIn("EXPLORE:FLICKER ", output)
        self.assertIn("EXPLORE:RERENDER ", output)

    def test_direct_settled_update_stays_silent(self) -> None:
        output = run_fixture("fixed")
        self.assertNotIn("EXPLORE:FLICKER ", output)
        self.assertNotIn("EXPLORE:RERENDER ", output)

    def test_synchronized_atomic_redraw_stays_silent(self) -> None:
        output = run_fixture("synchronized-adversarial")
        self.assertNotIn("EXPLORE:FLICKER ", output)
        self.assertNotIn("EXPLORE:RERENDER ", output)

    def test_idle_full_redraw_stays_silent(self) -> None:
        output = run_fixture("idle-redraw")
        self.assertNotIn("EXPLORE:RERENDER ", output)


if __name__ == "__main__":
    unittest.main()
