# Mobile and Apple native working-tree evidence

This records native working-tree diagnostics. It is not release evidence and
does not qualify any target as Stable.

## Source identity

- Recorded `HEAD`: `f696236f3d89f24d08f454e6e2e741348dae4263`
- The repository contained uncommitted candidate changes during every run.
- Result JSON files therefore name `HEAD`, but do not describe that exact Git
  tree. They must not be accepted by the exact-commit release validator.
- Raw results and logs are under
  `target/reproit-validation/mobile-working-tree/`.

## Toolchain

- Host: macOS 26.1 build 25B78, arm64
- Xcode: 26.2 build 17C52
- Flutter: 3.41.6, Dart 3.11.4
- Appium: 3.5.2
- XCUITest driver: 11.16.2
- UiAutomator2 driver: 8.0.0
- Android platform tools: ADB 36.0.0

## Android lane

The installed `ReproitValidation_API36` AVD was started with `-wipe-data`,
`-no-snapshot`, `-no-boot-anim`, and `-no-window`. Readiness was accepted only
after ADB reported `device` and `sys.boot_completed=1`.

- Serial: `emulator-5554`
- AVD: `ReproitValidation_API36`
- API: 36
- ABI: `arm64-v8a`
- Model: `sdk_gphone64_arm64`
- Network: validated, unmetered `AndroidWifi` through the emulator's default
  host NAT. Appium control was bound to host loopback.
- Reset evidence: cold data wipe plus boot-complete check
- Cleanup evidence: all ReproIt and Appium packages were uninstalled, then
  `adb -s emulator-5554 emu kill` completed and the device disappeared from
  `adb devices`.

Application permissions:

- `compose-android`, package `com.reproit.composefixture`: `INTERNET`,
  `ACCESS_NETWORK_STATE`, and its package-local dynamic receiver.
- `flutter-android`, package `com.example.reproit_flutter_fixture`: `INTERNET`
  and its package-local dynamic receiver.
- `react-native-android`, package `com.reproitrnfixture`: `INTERNET` and its
  package-local dynamic receiver.

Results:

- `compose-android`: passed the native arm64 product path. Log SHA-256:
  `a7a7494a16dbd59fd879a09b342e06de14b66136fd1c6750b3f052b2e9056ff1`
- `flutter-android`: failed before the journey due to Flutter tool
  infrastructure. Log SHA-256:
  `572784a0627d7451634d4a8c44c2c8123abbd23c78949e0ebcc74f1c53ff2488`
- `react-native-android`: passed the native arm64 product path. Log SHA-256:
  `582592d71b5646577458ece567082eb2ff1c2f4d591651c943d54aa09f2843dc`

The first Flutter attempt showed that Flutter 3.41 could parse a stale Dart VM
service announcement after its API 36 log filter failed. The Android runner now
clears the device log immediately before `flutter drive`, with a regression
test enforcing the ordering. On the clean rerun, Flutter found the current VM
service, then its own `getVersion` request failed with `Service has
disappeared`. The generated application remained alive. No ReproIt journey
started, so this is not a ReproIt product capability failure.

The first React Native attempt lost ADB and the UiAutomator2 instrumentation
process during exploration. ADB then remained stable through ten readiness
checks. After removing the stale UiAutomator2 packages, the complete rerun
passed. The retained result is the passing rerun.

These arm64 runs do not satisfy the manifest's required x86_64 Android
architecture. The native x86_64 `strix` host has AMD-V, accessible `/dev/kvm`,
ADB 35.0.2, and an owned Android SDK at
`/home/black/reproit-validation/android-sdk`. Its API 36 Google APIs x86_64 AVD
is named `ReproitValidation_API36_x86_64`. This is a viable release lane, not an
unreachable platform.

## iOS lane

Every iOS gate created and booted a disposable iPhone 16 Pro simulator on iOS
18.5, installed a fresh application, ran against arm64, then deleted the
simulator in its exit trap.

- `flutter-ios`: simulator `06DB9D60-53CC-442C-ACCB-C3CC454A6026`,
  bundle `com.example.reproitFlutterFixture`, passed. Log SHA-256:
  `59b43498f74617d06386727714fd5fe5345f62876da636be578aca80dc55ec61`
- `react-native-ios`: simulator `61885D26-EEBF-4DDD-A967-625C9AB90D9E`,
  bundle `org.reactjs.native.example.ReproitRnFixture`, passed. Log SHA-256:
  `b5b775bd5a3d919c1e6c2cf5471286a64b5a181890f487ee93f5b44ba54430ed`
- `swiftui-ios`: simulator `C95C0263-8DB6-46AC-8335-1D7594CC0346`,
  bundle `com.reproit.swiftuifixture`, passed. Log SHA-256:
  `e832215c20ec1967625dd1d40ac922da1b999d03917ff8e23f019885fc92d20e`

The fixtures declare no protected-resource usage descriptions and the harness
seeded no TCC grants. Runtime interaction was offline. Appium and
WebDriverAgent used host loopback; the simulator otherwise retained its
default network policy. Each retained log contains both its exact simulator
creation marker and matching deletion marker.

## macOS accessibility lane

- Target: `macos-ax`
- Bundle id: `com.reproit.macswiftuifixture`
- Runtime: native macOS 26.1 arm64
- Permission: the invoking terminal/Swift process had the required macOS
  Accessibility authorization; the fixture requested no protected resources.
- Network: no runtime network dependency
- Outcome: passed
- Log SHA-256:
  `dc3f44d5509d5751b6682b43bf3e7149ea35df0d7870f3355afb440e4ac1c425`
- Cleanup: the fixture process was terminated and its temporary build and fuzz
  state were removed by the exit trap.

## Release consequences

Before any of these results can support promotion:

1. Create one exact candidate commit containing the intended source.
2. Rerun the Apple gates against that clean commit.
3. Transfer that exact commit to the native x86_64 Android lane and rerun all
   three Android gates after a cold data wipe.
4. Resolve the Flutter Android VM service failure on the x86_64 lane or prove a
   separate product defect.
5. Validate retained results and log digests with
   `validation/release/check-native-evidence.py`.
