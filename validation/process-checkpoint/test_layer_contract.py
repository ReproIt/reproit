#!/usr/bin/env python3
"""Check the process-layer rules for checkpoint replay."""

from __future__ import annotations

import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SECCOMP = ROOT / "runners" / "process-shim" / "reproit_seccomp.c"
SHIM = ROOT / "runners" / "process-shim" / "reproit_shim.c"
GATE = Path(__file__).resolve().parent / "run.sh"


class ProcessLayerContractTests(unittest.TestCase):
    def test_framework_control_does_not_enter_target_observations(self) -> None:
        seccomp = SECCOMP.read_text(encoding="utf-8")
        shim = SHIM.read_text(encoding="utf-8")
        self.assertIn("reproit_seccomp_start(seccomp_env)", shim)
        self.assertNotIn('getenv("REPROIT_SECCOMP")', seccomp)

    def test_libc_capture_selects_libc_replay(self) -> None:
        seccomp = SECCOMP.read_text(encoding="utf-8")
        gate = GATE.read_text(encoding="utf-8")
        self.assertIn("replay_was_captured_without_seccomp()", seccomp)
        self.assertIn("REPROIT_SECCOMP=0", gate)


if __name__ == "__main__":
    unittest.main()
