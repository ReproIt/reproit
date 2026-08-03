#!/usr/bin/env python3
"""Prove the flutter-ios retry tiers against stubbed tools, no simulator.

The measured CI flake is a VM-service connect stall on a hosted runner; it
cannot be reproduced on demand. These tests drive run-flutter-drive.sh with a
fake flutter that stalls after printing the Dart VM service line a configured
number of times, proving the tier ladder (initial, erase-reboot, fresh
simulator with a new UDID) fires in order, stays bounded at three attempts,
and lands its per-attempt evidence in the gate output.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import textwrap
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

ROOT = Path(__file__).resolve().parents[2]
ORIGINAL_UDID = "AAAAAAAA-0000-0000-0000-000000000000"
FRESH_UDID = "FFFFFFFF-0000-0000-0000-000000000000"


def write_executable(path: Path, body: str) -> None:
    path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + body, encoding="utf-8")
    path.chmod(0o755)


class FlutterDriveRetryContractTest(unittest.TestCase):
    def run_gate(self, stalls_before_success: int) -> subprocess.CompletedProcess[str]:
        with TemporaryDirectory() as directory:
            temporary = Path(directory)
            fake_root = temporary / "root"
            backends = fake_root / "validation/backends"
            backends.mkdir(parents=True)
            for name in (
                "run-flutter-drive.sh",
                "run-output-contract.py",
                "sample-stalled-tools.sh",
            ):
                shutil.copy(ROOT / "validation/backends" / name, backends / name)
            fixture = fake_root / "fixtures/flutter-fixture/lib"
            fixture.mkdir(parents=True)
            (fixture / "main.dart").write_text("// fixture\n", encoding="utf-8")
            (fake_root / "target/debug").mkdir(parents=True)
            write_executable(fake_root / "target/debug/reproit", "exit 0\n")

            stubs = temporary / "stubs"
            stubs.mkdir()
            counter = temporary / "drive-count"
            counter.write_text("0", encoding="utf-8")
            fresh_state = temporary / "fresh-exists"
            write_executable(
                stubs / "flutter",
                textwrap.dedent(
                    f"""\
                    if [[ "$1" == "create" ]]; then
                      mkdir -p "${{@: -1}}/lib"
                      exit 0
                    fi
                    count=$(( $(cat {counter!s}) + 1 ))
                    printf '%s' "$count" > {counter!s}
                    echo "The Dart VM service is listening on http://127.0.0.1:1/x/"
                    if [[ "$count" -le {stalls_before_success} ]]; then
                      sleep 30
                    fi
                    echo "EXPLORE:STATE s0"
                    echo "EXPLORE:EDGE s0 s1"
                    echo "key:s:toggle"
                    echo "Detail revealed"
                    echo "JOURNEY DONE"
                    echo "All tests passed"
                    """
                ),
            )
            write_executable(stubs / "cargo", "exit 0\n")
            write_executable(
                stubs / "xcrun",
                textwrap.dedent(
                    f"""\
                    if [[ "$1 $2 $3" == "simctl list devices" ]]; then
                      printf '%s' '{{"devices":{{"iOS-18-5":['
                      printf '%s' '{{"udid":"{ORIGINAL_UDID}","state":"Booted"}}'
                      if [[ -e {fresh_state!s} ]]; then
                        printf '%s' ',{{"udid":"{FRESH_UDID}","state":"Booted"}}'
                      fi
                      printf '%s\\n' ']}}}}'
                    elif [[ "$1 $2" == "simctl create" ]]; then
                      touch {fresh_state!s}
                      echo "{FRESH_UDID}"
                    elif [[ "$1 $2" == "simctl delete" ]]; then
                      rm -f {fresh_state!s}
                    fi
                    exit 0
                    """
                ),
            )

            environment = os.environ.copy()
            environment["PATH"] = f"{stubs}:{environment['PATH']}"
            environment["REPROIT_IOS_UDID"] = ORIGINAL_UDID
            environment["REPROIT_IOS_RUNTIME_ID"] = "iOS-18-5"
            environment["REPROIT_IOS_DEVICE_TYPE_ID"] = "iPhone-16-Pro"
            environment["REPROIT_FLUTTER_VM_CONNECT_TIMEOUT_SECONDS"] = "1"
            environment["REPROIT_FLUTTER_IDLE_TIMEOUT_SECONDS"] = "5"
            return subprocess.run(
                ["bash", str(backends / "run-flutter-drive.sh")],
                cwd=fake_root,
                env=environment,
                text=True,
                capture_output=True,
                timeout=120,
            )

    def attempts(self, stdout: str) -> dict[str, object]:
        lines = [
            line
            for line in stdout.splitlines()
            if line.startswith("FLUTTER_GATE_ATTEMPTS ")
        ]
        self.assertEqual(len(lines), 1, stdout)
        return json.loads(lines[0].removeprefix("FLUTTER_GATE_ATTEMPTS "))

    def test_two_stalls_escalate_to_fresh_simulator(self) -> None:
        result = self.run_gate(stalls_before_success=2)

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # run_drive folds the contract's stderr into the teed gate log.
        self.assertIn("vm-service connect stall after 1 seconds", result.stdout)
        self.assertEqual(
            self.attempts(result.stdout),
            {
                "attempts": [
                    {"tier": "initial", "outcome": "vm-service-connect-stall", "dds": True},
                    {
                        "tier": "erase-reboot",
                        "outcome": "vm-service-connect-stall",
                        "dds": False,
                    },
                    {"tier": "fresh-simulator", "outcome": "passed", "dds": False},
                ],
                "succeededTier": "fresh-simulator",
            },
        )
        self.assertIn(f"FlutterDrive fresh simulator: created {FRESH_UDID}", result.stdout)
        self.assertIn(
            f"FlutterDrive fresh simulator cleanup: deleted {FRESH_UDID}", result.stdout
        )

    def test_clean_pass_records_single_initial_attempt(self) -> None:
        result = self.run_gate(stalls_before_success=0)

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            self.attempts(result.stdout),
            {
                "attempts": [{"tier": "initial", "outcome": "passed", "dds": True}],
                "succeededTier": "initial",
            },
        )
        self.assertNotIn("fresh simulator: created", result.stdout)

    def test_three_stalls_fail_bounded_with_evidence(self) -> None:
        result = self.run_gate(stalls_before_success=3)

        self.assertEqual(result.returncode, 121, result.stdout + result.stderr)
        self.assertEqual(
            self.attempts(result.stdout),
            {
                "attempts": [
                    {"tier": "initial", "outcome": "vm-service-connect-stall", "dds": True},
                    {
                        "tier": "erase-reboot",
                        "outcome": "vm-service-connect-stall",
                        "dds": False,
                    },
                    {
                        "tier": "fresh-simulator",
                        "outcome": "vm-service-connect-stall",
                        "dds": False,
                    },
                ],
                "succeededTier": None,
            },
        )


if __name__ == "__main__":
    unittest.main()
