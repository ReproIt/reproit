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
GREENSTASH = Path(__file__).with_name("greenstash_currency_rotation.py")
LOCALSEND = Path(__file__).with_name("localsend_receive_link.py")
GOPEED = Path(__file__).with_name("gopeed_proxy_credentials.py")


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

    def test_real_app_ui_is_driven_through_appium(self) -> None:
        nextplayer = MODULE_PATH.read_text(encoding="utf-8")
        greenstash = GREENSTASH.read_text(encoding="utf-8")

        self.assertIn('"appium:automationName": "UiAutomator2"', nextplayer)
        self.assertIn('session.set_orientation("LANDSCAPE")', greenstash)
        self.assertNotIn('"uiautomator",', nextplayer)
        self.assertNotIn('"input", "tap"', nextplayer)
        self.assertNotIn('"input", "text"', greenstash)
        self.assertNotIn('"wm", "user-rotation"', greenstash)

    def test_flutter_drivers_are_owned_and_read_the_platform_hierarchy(self) -> None:
        runner = Path(__file__).with_name("run_android_field_driver.sh").read_text(
            encoding="utf-8"
        )

        for driver in (LOCALSEND, GOPEED):
            self.assertIn(driver.name, runner)
            source = driver.read_text(encoding="utf-8")
            # The profile-mode VM service carries no tree, so the observable is
            # read from the hierarchy Appium exposes, never from a dump RPC.
            self.assertIn("session.source()", source)
            self.assertNotIn('"uiautomator",', source)
            self.assertNotIn("ext.flutter.debugDump", source)

    def test_runner_requires_exact_cli_commit_provenance(self) -> None:
        runner = Path(__file__).with_name("run_android_field_driver.sh")
        source = runner.read_text(encoding="utf-8")

        self.assertIn('--cli-commit "$REPROIT_FIELD_CLI_COMMIT"', source)

    def test_only_device_offline_transport_is_retried(self) -> None:
        device = mock.Mock()
        device.reset_and_start.side_effect = [
            {"attempt": 1},
            {"attempt": 2},
        ]
        observation = mock.Mock(
            side_effect=[
                RuntimeError("Appium session failed: adb: device offline"),
                {"status": "reproduced"},
            ]
        )

        record = MODULE.run_with_reset(device, "affected-1", observation)

        self.assertEqual(record["infrastructureAttempts"], 2)
        self.assertEqual(len(record["infrastructureRetryReasons"]), 1)
        self.assertEqual(device.reset_and_start.call_count, 2)

    def test_semantic_failure_is_not_retried(self) -> None:
        device = mock.Mock()
        device.reset_and_start.return_value = {"attempt": 1}
        observation = mock.Mock(side_effect=RuntimeError("identity mismatch"))

        with self.assertRaisesRegex(RuntimeError, "identity mismatch"):
            MODULE.run_with_reset(device, "affected-1", observation)

        observation.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
