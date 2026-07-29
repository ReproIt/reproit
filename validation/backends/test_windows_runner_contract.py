#!/usr/bin/env python3
"""Contract tests for native Windows desktop gate containment."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "validation/backends/run-windows-desktop.ps1"


class WindowsRunnerContractTests(unittest.TestCase):
    def test_cargo_target_is_unique_to_the_gate_process_and_cleaned(self) -> None:
        source = RUNNER.read_text(encoding="utf-8")
        cleanup = (
            "Remove-Item -Recurse -Force $cargoTarget "
            "-ErrorAction SilentlyContinue"
        )

        self.assertIn('"reproit-backend-target-$PID"', source)
        self.assertNotIn(
            '$env:CARGO_TARGET_DIR = Join-Path $env:TEMP '
            '"reproit-backend-target"',
            source,
        )
        self.assertEqual(source.count(cleanup), 2)
        self.assertIn("$env:CARGO_TARGET_DIR = $cargoTarget", source)

        create_index = source.index(
            "New-Item -ItemType Directory -Path $cargoTarget"
        )
        assign_index = source.index("$env:CARGO_TARGET_DIR = $cargoTarget")
        build_index = source.index("cargo build -p reproit --release")
        self.assertLess(create_index, assign_index)
        self.assertLess(assign_index, build_index)

        finally_index = source.rindex("finally {")
        cleanup_index = source.rindex(cleanup)
        pop_location_index = source.rindex("Pop-Location")
        self.assertLess(finally_index, cleanup_index)
        self.assertLess(cleanup_index, pop_location_index)


if __name__ == "__main__":
    unittest.main()
