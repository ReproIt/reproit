#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("run-output-contract.py")


def run_contract(*arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *arguments],
        check=False,
        capture_output=True,
        text=True,
        timeout=10,
    )


class RunOutputContractTests(unittest.TestCase):
    def test_returns_child_status_and_relays_output(self) -> None:
        result = run_contract(
            "--idle-timeout-seconds",
            "2",
            "--",
            sys.executable,
            "-c",
            "print('visible output'); raise SystemExit(7)",
        )

        self.assertEqual(result.returncode, 7)
        self.assertIn("visible output", result.stdout)

    def test_stops_after_all_success_markers(self) -> None:
        command = (
            "import sys,time; "
            "print('JOURNEY DONE', flush=True); "
            "print('All tests passed', flush=True); "
            "time.sleep(30)"
        )
        result = run_contract(
            "--idle-timeout-seconds",
            "2",
            "--success-marker",
            "JOURNEY DONE",
            "--success-marker",
            "All tests passed",
            "--",
            sys.executable,
            "-c",
            command,
        )

        self.assertEqual(result.returncode, 0)
        self.assertIn("output contract satisfied", result.stderr)

    def test_idle_command_is_stopped(self) -> None:
        result = run_contract(
            "--idle-timeout-seconds",
            "1",
            "--",
            sys.executable,
            "-c",
            "import time; time.sleep(30)",
        )

        self.assertEqual(result.returncode, 124)
        self.assertIn("idle timeout after 1 seconds", result.stderr)


if __name__ == "__main__":
    unittest.main()
