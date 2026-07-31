# Android exact native evidence

This directory retains the canonical `flutter-android` and
`react-native-android` native release gates for commit
`414c1a14c7d60e1127a7738834e995020366fed0`.

- Executor: native Linux x86_64 (Fedora host `strix`), Docker engine
  `x86_64/linux`, KVM
- Route: `ssh black@zgx-5a09.local`, then `ssh strix`
- Collector: `validation/release/run-android-x86-remote.sh --gate
  flutter-android --gate react-native-android`
- Device: run-scoped API 36 x86_64 AVD, reset with `-wipe-data` and
  `-no-snapshot`; identity in `device.json`
- Network policy: dependency preparation online, gate runtime on Docker
  network `none`
- Results: `flutter-android.json`, `react-native-android.json`
- Captured logs: `flutter-android.log`, `react-native-android.log`
- Validated summary: `validated-summary.json`
- Run metadata: `run-metadata.json`
- Log SHA-256:
  - `flutter-android.log`:
    `e993b1bc79b2e4583023a29fdd063492335146debecbea04ad14d931e9a71dad`
  - `react-native-android.log`:
    `a5c71ea88b56c93a4ab293973e7d69633a81e579dd7c6515c088f77ad6918d03`
- Validated summary SHA-256:
  `c678e7cdb820562e31b57fa06eefdc1043c24e2ecec85e78dc2446997922a234`
- Run metadata SHA-256:
  `d242df2b0a06ded3687ec2b15ebed990b6036ca0673cff6330c31c2cc370c51b`

Both gates passed every required output marker with exit code zero through the
real product path. The collector's traps removed the containers, emulator, ADB,
Xvfb, AVD, and remote run directory.

Native fixture success does not promote either target. Flutter Android and
React Native Android still need their two-independent-application field
campaigns and per-target corpus records, and Flutter Android additionally
carries an unresolved runtime-bound blocker for release-mode APKs.
