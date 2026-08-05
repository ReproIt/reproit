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
- `bounds`: the platforms it reaches, plus the runtimes and frameworks it
  covers. Platforms are declared per target, never derived from the gates: a
  gate names where CI executes it, which says nothing about where a user's
  application runs.
- `fieldBenchmark`: the affected-versus-fixed application campaign record.
- `cleanCorpus` and `adversarialCorpus`: the false-positive measurement behind
  the target's oracles.
- `packageInstall` and `manualReview`: the CI gate or retained artifact for
  each.

`policy` records the evidence rules that hold for every target: exact identity
preservation, a verified minimized trigger, a passing neighboring-behavior
control, and exact-commit native evidence.

The evidence record is derived, never hand-authored. Each target lists its
native gates, its release evidence directories, its field benchmark, and its
retained evidence slots. The generated status records any absent evidence as
a named gap. A gap names work to do. It is not a status label.

## Supported targets

<!-- generated:targets -->

Declared atomic targets: 21.

| Target | Framework | Platforms | Driving runtime |
|---|---|---|---|
| Backend contracts | Backend services | Linux, macOS, Windows | HTTP, OpenAPI |
| Jetpack Compose Android | Jetpack Compose | Android | ART, Appium UiAutomator2 |
| Electron | Electron | Linux, macOS, Windows | Chromium, Node.js, CDP |
| Flutter Android | Flutter | Android | Dart VM service (profile-mode build, liveness and isolate only), Appium UiAutomator2 |
| Flutter iOS | Flutter | iOS | Dart VM service, flutter drive |
| Linux GTK | GTK 3, GTK 4 | Linux | AT-SPI 2, GLib main loop |
| Linux Qt Quick/QML | Qt Quick/QML | Linux | AT-SPI 2, Qt 6, QML engine |
| Linux Qt Widgets | Qt Widgets | Linux | AT-SPI 2, Qt 6 |
| Linux wxWidgets | wxWidgets | Linux | AT-SPI 2, GTK backend |
| macOS Accessibility | SwiftUI, AppKit | macOS | Swift runtime, Accessibility API |
| React Native Android | React Native | Android | Hermes, Appium UiAutomator2 |
| React Native iOS | React Native | iOS | Hermes, Appium XCUITest |
| SwiftUI iOS | SwiftUI | iOS | Swift runtime, Appium XCUITest |
| Tauri | Tauri | Linux, Windows | WebKitGTK, tauri-driver |
| Terminal UI | Terminal applications | Linux, macOS | PTY, VT parser |
| Web Chromium | DOM applications | Linux, macOS, Windows | Node.js 20+, Playwright CDP |
| Web Firefox | DOM applications | Linux, macOS, Windows | Node.js 20+, Playwright |
| Web WebKit | DOM applications | Linux, macOS, Windows | Node.js 20+, Playwright |
| Windows Avalonia | Avalonia | Windows | .NET, UI Automation |
| Windows WinUI 3 | WinUI 3 | Windows | .NET, UI Automation, WinAppSDK |
| Windows WPF | WPF | Windows | .NET, UI Automation |

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
