import importlib.util
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
