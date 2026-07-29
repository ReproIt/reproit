import os
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "validation/backends/with-ios-simulator.sh"
UDID = "11111111-2222-3333-4444-555555555555"


class IosSimulatorRunnerContractTest(unittest.TestCase):
    def run_wrapper(self, *, delete_succeeds: bool) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            state = temporary / "device-exists"
            state.touch()
            xcrun = temporary / "xcrun"
            xcrun.write_text(
                textwrap.dedent(
                    f"""\
                    #!/usr/bin/env bash
                    set -euo pipefail
                    state={state!s}
                    if [[ "$1 $2 $3" == "simctl list devices" ]]; then
                      if [[ -e "$state" ]]; then
                        printf '%s' '{{"devices":{{'
                        printf '%s' '"com.apple.CoreSimulator.SimRuntime.iOS-18-5":['
                        printf '%s' '{{"udid":"{UDID}","isAvailable":true,'
                        printf '%s' '"deviceTypeIdentifier":"com.apple.CoreSimulator.'
                        printf '%s\\n' 'SimDeviceType.iPhone-16-Pro"}}]}}}}'
                      else
                        printf '%s' '{{"devices":{{'
                        printf '%s\\n' '"com.apple.CoreSimulator.SimRuntime.iOS-18-5":[]}}}}'
                      fi
                    elif [[ "$1 $2" == "simctl create" ]]; then
                      printf '%s\\n' '{UDID}'
                    elif [[ "$1 $2" == "simctl delete" ]]; then
                      {"rm -f \"$state\"" if delete_succeeds else "exit 1"}
                    else
                      exit 0
                    fi
                    """
                ),
                encoding="utf-8",
            )
            xcrun.chmod(0o755)
            environment = os.environ.copy()
            environment["PATH"] = f"{temporary}:{environment['PATH']}"
            return subprocess.run(
                ["bash", str(RUNNER), "bash", "-c", "test -n \"$REPROIT_IOS_UDID\""],
                cwd=ROOT,
                env=environment,
                text=True,
                capture_output=True,
                timeout=20,
            )

    def test_success_marker_requires_absent_simulator(self) -> None:
        result = self.run_wrapper(delete_succeeds=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"iOS simulator cleanup: deleted {UDID}", result.stdout)

    def test_remaining_simulator_fails_cleanup_without_success_marker(self) -> None:
        result = self.run_wrapper(delete_succeeds=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("iOS simulator cleanup: deleted", result.stdout)
        self.assertIn("iOS simulator cleanup: device still exists", result.stderr)


if __name__ == "__main__":
    unittest.main()
