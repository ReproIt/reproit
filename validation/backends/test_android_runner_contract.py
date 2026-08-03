#!/usr/bin/env python3

from __future__ import annotations

import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("run-flutter-drive-android.sh")
REACT_NATIVE_SCRIPT = Path(__file__).with_name("run-react-native-android.sh")
COMPOSE_SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "fixtures"
    / "compose-fixture"
    / "compose-appium-smoke.sh"
)


class AndroidFlutterRunnerContractTests(unittest.TestCase):
    def test_uses_the_configured_cargo_target_directory(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"', source)
        self.assertIn('"$CARGO_TARGET_DIR/debug/reproit" init', source)

    def test_offline_mode_disables_flutter_network_resolution(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn("FLUTTER_CREATE_ARGS+=(--no-pub)", source)
        self.assertIn("flutter pub get --offline", source)
        self.assertIn("FLUTTER_DRIVE_ARGS+=(--no-pub)", source)

    def test_clears_stale_vm_service_logs_immediately_before_drive(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        clear_index = source.index("adb_run logcat -c")
        drive_index = source.index('(cd "$APP" && flutter drive')

        self.assertLess(clear_index, drive_index)
        between = source[clear_index:drive_index]
        self.assertNotIn("flutter ", between)
        self.assertNotIn("adb_run shell am start", between)

    def test_appium_gates_force_launch_after_server_bootstrap(self) -> None:
        for script in (REACT_NATIVE_SCRIPT, COMPOSE_SCRIPT):
            with self.subTest(script=script.name):
                source = script.read_text(encoding="utf-8")
                self.assertIn('"appium:forceAppLaunch":true', source)

    def test_compose_fixture_launch_is_owned_by_appium(self) -> None:
        source = COMPOSE_SCRIPT.read_text(encoding="utf-8")

        self.assertIn(
            "adb_device shell am force-stop com.reproit.composefixture",
            source,
        )
        self.assertNotIn("adb_device shell am start", source)
        self.assertNotIn("KEYCODE_HOME", source)

    def test_react_native_runner_verifies_prepared_offline_template(self) -> None:
        source = REACT_NATIVE_SCRIPT.read_text(encoding="utf-8")
        self.assertIn("REPROIT_RN_TEMPLATE_DIR", source)
        self.assertIn("expected_template_sha256", source)
        self.assertIn("actual_template_sha256", source)
        self.assertIn("React Native template cache verified", source)


if __name__ == "__main__":
    unittest.main()
