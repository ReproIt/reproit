#!/usr/bin/env python3

from __future__ import annotations

import os
import sys
import unittest

import gate


@unittest.skipIf(os.name == "nt", "POSIX signal cleanup contract")
class GateTerminationTests(unittest.TestCase):
    def test_timeout_allows_owned_cleanup_before_kill(self) -> None:
        command = (
            "import signal\n"
            "import time\n"
            "def stop(*_args):\n"
            "    print('cleanup complete', flush=True)\n"
            "    raise SystemExit(0)\n"
            "signal.signal(signal.SIGTERM, stop)\n"
            "print('ready', flush=True)\n"
            "time.sleep(30)\n"
        )

        status, exit_code, output = gate.execute(
            [sys.executable, "-u", "-c", command],
            timeout_seconds=1,
        )

        self.assertEqual(status, "timed-out")
        self.assertIsNone(exit_code)
        self.assertIn(b"cleanup complete", output)


if __name__ == "__main__":
    unittest.main()
