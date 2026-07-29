#!/usr/bin/env python3
"""Contract tests for the bounded Android field campaign helpers."""

from __future__ import annotations

import importlib.util
import io
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

MODULE_PATH = Path(__file__).with_name("nextplayer_permission_loop.py")
SPEC = importlib.util.spec_from_file_location("nextplayer_permission_loop", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class AndroidFieldContractTests(unittest.TestCase):
    def test_stop_removes_avd_when_adb_cleanup_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            avd_home = root / "avd"
            avd_home.mkdir()
            (avd_home / "owned-state").write_text("test", encoding="utf-8")
            device = MODULE.Device(root / "sdk", avd_home, root)
            emulator_log = io.StringIO()
            device.emulator_log = emulator_log

            with mock.patch.object(device, "adb_run", side_effect=OSError("adb failed")):
                device.stop()

            self.assertFalse(avd_home.exists())
            self.assertTrue(emulator_log.closed)

    def test_field_runner_rejects_unowned_driver(self) -> None:
        runner = Path(__file__).with_name("run_android_field_driver.sh")
        result = subprocess.run(
            ["bash", str(runner)],
            check=False,
            capture_output=True,
            text=True,
            env={"REPROIT_FIELD_DRIVER": "../unowned.py"},
        )

        self.assertEqual(result.returncode, 2)
        self.assertIn("unsupported Android field driver", result.stderr)


if __name__ == "__main__":
    unittest.main()
