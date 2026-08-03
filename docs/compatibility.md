# Compatibility

`validation/support-manifest.json` is the canonical atomic compatibility
contract. `validation/compatibility/check.py` validates it and generates the
[current status](../validation/compatibility/STATUS.md). Documentation cannot
add a target.

An atomic target is one exact combination of platform, toolkit, and observation
channel, for example Chromium through Playwright CDP on Linux. Targets are
atomic because families do not behave as a unit: browsers, operating systems,
desktop toolkits, mobile frameworks, and webview hosts each carry their own
native gates, their own bounds, and their own evidence.

Capture and replay coverage is tracked separately in the
[capability ledger](../validation/capabilities/README.md). Listing a target
cannot substitute for a missing collector, compiler requirement, or replay
provider.

## Per-target evidence

Every target in the manifest declares:

- `ownedGates`: the native fixtures it owns.
- `releaseGates`: where each owned gate's exact-commit evidence is retained.
  Every owned gate is release-gated.
- `bounds`: the runtimes and frameworks it covers. Operating systems and
  architectures are derived from the owned gates.
- `fieldBenchmark`: the affected-versus-fixed application campaign record.
- `cleanCorpus` and `adversarialCorpus`: the false-positive measurement behind
  the target's oracles.
- `packageInstall` and `manualReview`: the CI gate or retained artifact for
  each.

`policy` records the evidence rules that hold for every target: exact identity
preservation, a verified minimized trigger, a passing neighboring-behavior
control, and exact-commit native evidence.

## Supported targets

<!-- generated:targets -->

Supported atomic targets: 21.

| Target | Family | Native gates | OS | Architectures |
|---|---|---|---|---|
| Backend contracts | backend | backend-contract | linux | x86_64 |
| Jetpack Compose Android | native-mobile | compose-android | android-emulator | x86_64 |
| Electron Linux | desktop-webview | electron | linux | x86_64 |
| Flutter Android | flutter | flutter-android | android-emulator | x86_64 |
| Flutter iOS | flutter | flutter-ios | ios-simulator | arm64 |
| Linux GTK | desktop | linux-atspi-gtk | linux-container | x86_64 |
| Linux Qt Quick/QML | desktop | linux-atspi-toolkits | linux-container | x86_64 |
| Linux Qt Widgets | desktop | linux-atspi-toolkits | linux-container | x86_64 |
| Linux wxWidgets | desktop | linux-atspi-toolkits | linux-container | x86_64 |
| macOS Accessibility | desktop | macos-ax | macos | arm64 |
| React Native Android | native-mobile | react-native-android | android-emulator | x86_64 |
| React Native iOS | native-mobile | react-native-ios | ios-simulator | arm64 |
| SwiftUI iOS | native-mobile | swiftui-ios | ios-simulator | arm64 |
| Tauri Linux | desktop-webview | tauri | linux | x86_64 |
| Terminal UI | tui | tui-pty | linux | x86_64 |
| Web Chromium | web | web-chromium | linux | x86_64 |
| Web Firefox | web | web-engines | linux | x86_64 |
| Web WebKit | web | web-engines | linux | x86_64 |
| Windows Avalonia | desktop | windows-uia | windows-x86_64-interactive | x86_64 |
| Windows WinUI 3 | desktop | windows-uia | windows-x86_64-interactive | x86_64 |
| Windows WPF | desktop | windows-uia | windows-x86_64-interactive | x86_64 |

Every target's runtime and framework bounds, release evidence
directories, and retained evidence slots are listed in
[the generated status](../validation/compatibility/STATUS.md).

<!-- /generated:targets -->

## 1.x surface

1.x preserves documented flags, exit behavior, JSON field meaning, persisted
formats, event protocol version 1, release archives, and the published SDK
source APIs. Patch releases may add optional fields but do not remove fields,
reinterpret results, or broaden a finding predicate.

Hidden diagnostics, advanced causal reduction, and unpublished registry
coordinates remain outside that promise. They must fail closed and cannot
silently create a regression guard.

## Host requirements

- Node.js for browser-backed runners.
- Rust for source builds.
- The exact SDK, driver, simulator, and VM pins in
  `validation/native/toolchains.json`.
- The repairs reported by `reproit doctor` for the selected target.

Release archives cover macOS, Linux, and Windows on arm64 and x86_64. Native
behavior evidence records the architecture actually exercised.
