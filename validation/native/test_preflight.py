import importlib.util
import os
import re
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

PATH = Path(__file__).with_name("preflight.py")
SPEC = importlib.util.spec_from_file_location("native_preflight", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MODULE)
ROOT = PATH.parents[2]


class PreflightTest(unittest.TestCase):
    def test_executable_docker_recipes_follow_stable_rust(self) -> None:
        invalid_image = re.compile(r"\brust(?::(?:\d+\.\d+|stable-)|@sha256:)")
        rust_image = re.compile(r"\brust:(?:bookworm|trixie)\b")
        offenders = []
        stale_defaults = []
        for candidate in ROOT.rglob("*"):
            if not candidate.is_file():
                continue
            if candidate.suffix != ".sh" and "Dockerfile" not in candidate.name:
                continue
            if "target" in candidate.parts:
                continue
            source = candidate.read_text(encoding="utf-8")
            if invalid_image.search(source):
                offenders.append(str(candidate.relative_to(ROOT)))
            if rust_image.search(source) and "rustup update stable" not in source:
                stale_defaults.append(str(candidate.relative_to(ROOT)))
        self.assertEqual(offenders, [], f"fixed Rust Docker images: {offenders}")
        self.assertEqual(
            stale_defaults,
            [],
            f"Rust Docker images without stable refresh: {stale_defaults}",
        )

    @patch.object(
        MODULE,
        "output",
        side_effect=["rustc 1.97.1 (abc)", "rustc 1.97.1 (abc)"],
    )
    def test_active_rust_must_match_stable(self, _output: object) -> None:
        MODULE.require_stable_rust()

    @patch.object(
        MODULE,
        "output",
        side_effect=["rustc 1.88.0 (abc)", "rustc 1.97.1 (def)"],
    )
    def test_old_active_rust_is_rejected(self, _output: object) -> None:
        with self.assertRaisesRegex(ValueError, "not the installed stable toolchain"):
            MODULE.require_stable_rust()

    @patch.object(
        MODULE,
        "output",
        side_effect=["rustc 1.97.1 (abc)", "rustc 1.97.1 (abc)", "v24.9.0"],
    )
    def test_node_major_is_pinned(self, _output: object) -> None:
        MODULE.validate_versions("linux-hosted", {"rustChannel": "stable", "nodeMajor": 24})

    @patch.object(MODULE, "require_appium_driver")
    @patch.object(
        MODULE,
        "output",
        side_effect=[
            "rustc 1.97.1 (abc)",
            "rustc 1.97.1 (abc)",
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
                "rustChannel": "stable",
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
