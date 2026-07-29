# Compose Android exact native evidence

This directory retains the canonical `compose-android` native release gate for
commit `1091520c930c073abacbf33f4c100561638b2402`.

- Executor: native Linux x86_64
- Android device: reset API 36 x86_64 AVD
- Automation: Appium 3.5.2 with UiAutomator2 8.0.0
- Result: `compose-android.json`
- Captured log: `compose-android.log`
- Validated summary: `validated-summary.json`
- Run metadata: `run-metadata.json`
- Result SHA-256:
  `a109cc2776cbb1167e0b1486f9b857020d37b517f9502a9762815458d61940c2`
- Log SHA-256:
  `5d101e4b04e4c818ad3c6237e5c19e29a1bf216d807d9b80e41dfdbe81dbb3dd`
- Validated summary SHA-256:
  `45544b08ed82dc064a0cf731ccf7249f92328d72bfe1059d09a3884e40613522`
- Run metadata SHA-256:
  `88d54b4fd85d8d628ecf92720a381eafe3d5f4dfa335faf76e30c7786051d581`

The gate passed all required output markers through the real Compose fixture
and Appium product path. The remote cleanup audit found no owned containers,
emulator or Appium processes, run directories, or trial evidence directories.
