import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

PATH = Path(__file__).with_name("preflight.py")
SPEC = importlib.util.spec_from_file_location("native_preflight", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MODULE)


class PreflightTest(unittest.TestCase):
    def test_exact_version_is_required(self) -> None:
        MODULE.require_version("Rust", "rustc 1.88.0 (abc)", "1.88.0")
        with self.assertRaisesRegex(ValueError, "not pinned"):
            MODULE.require_version("Rust", "rustc 1.89.0 (abc)", "1.88.0")

    @patch.object(MODULE, "output", return_value="v24.9.0")
    def test_node_major_is_pinned(self, _output: object) -> None:
        MODULE.validate_versions("linux-hosted", {"rust": "v24.9.0", "nodeMajor": 24})

    @patch.object(MODULE, "require_appium_driver")
    @patch.object(
        MODULE,
        "output",
        side_effect=[
            "rustc 1.88.0 (abc)",
            "v24.9.0",
            "Appium 3.5.2",
            "ffmpeg version 7.1.5 Copyright",
            "Xcode 26.2\nBuild version 17C52",
        ],
    )
    def test_macos_appium_ffmpeg_is_pinned(
        self,
        _output: object,
        _require_appium_driver: object,
    ) -> None:
        MODULE.validate_versions(
            "macos-appium",
            {
                "rust": "1.88.0",
                "nodeMajor": 24,
                "appium": "3.5.2",
                "appiumDrivers": {"xcuitest": "11.16.2"},
                "xcodeByProfile": {"macos-appium": "26.2"},
                "ffmpeg": "7.1.5",
            },
        )

    @patch.object(MODULE.shutil, "which", return_value=None)
    def test_adb_is_found_under_android_home(self, _which: object) -> None:
        with tempfile.TemporaryDirectory() as directory:
            adb = Path(directory) / "platform-tools/adb"
            adb.parent.mkdir()
            adb.touch()
            with patch.dict(os.environ, {"ANDROID_HOME": directory}, clear=False):
                self.assertEqual(MODULE.prerequisite_path("adb"), adb)

    @patch.object(
        MODULE,
        "output",
        return_value='{"xcuitest":{"version":"11.16.2","installed":true}}',
    )
    def test_appium_driver_version_is_exact(self, _output: object) -> None:
        MODULE.require_appium_driver(
            "macos-appium",
            {"appiumDrivers": {"xcuitest": "11.16.2"}},
        )
        with self.assertRaisesRegex(ValueError, "not pinned"):
            MODULE.require_appium_driver(
                "macos-appium",
                {"appiumDrivers": {"xcuitest": "11.16.3"}},
            )


if __name__ == "__main__":
    unittest.main()
