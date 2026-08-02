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

    def test_stall_marker_fails_fast_with_named_reason(self) -> None:
        # The measured CI shape: the Dart VM service line is the LAST output,
        # then silence. The stall bound must fire, named, well before the
        # generic idle timeout would have.
        command = (
            "import time; "
            "print('The Dart VM service is listening on http://x', flush=True); "
            "time.sleep(30)"
        )
        import time as time_module

        started = time_module.monotonic()
        result = run_contract(
            "--idle-timeout-seconds",
            "8",
            "--stall-marker",
            "The Dart VM service is listening on",
            "--stall-timeout-seconds",
            "1",
            "--stall-name",
            "vm-service connect",
            "--",
            sys.executable,
            "-c",
            command,
        )
        elapsed = time_module.monotonic() - started

        self.assertEqual(result.returncode, 121)
        self.assertIn("vm-service connect stall after 1 seconds", result.stderr)
        self.assertLess(elapsed, 6, "stall bound did not preempt the idle timeout")

    def test_stall_bound_disarms_after_next_output(self) -> None:
        # Output after the marker means the connect happened; a later silence
        # must be judged by the generic idle timeout, not the stall bound.
        command = (
            "import time; "
            "print('The Dart VM service is listening on http://x', flush=True); "
            "time.sleep(0.3); "
            "print('connected, running', flush=True); "
            "time.sleep(30)"
        )
        result = run_contract(
            "--idle-timeout-seconds",
            "2",
            "--stall-marker",
            "The Dart VM service is listening on",
            "--stall-timeout-seconds",
            "8",
            "--",
            sys.executable,
            "-c",
            command,
        )

        self.assertEqual(result.returncode, 124)
        self.assertIn("idle timeout after 2 seconds", result.stderr)

    def test_stall_arguments_must_come_together(self) -> None:
        result = run_contract(
            "--idle-timeout-seconds",
            "2",
            "--stall-marker",
            "x",
            "--",
            sys.executable,
            "-c",
            "pass",
        )

        self.assertEqual(result.returncode, 2)
        self.assertIn("go together", result.stderr)

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
