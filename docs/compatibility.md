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

Qualification is derived, never hand-authored. A target is `qualified` only
when its independent field benchmark is complete and every required evidence
slot is present. Otherwise it is `preview`. The generated status shows the
exact evidence gap. Both levels remain native-gated. Preview preserves the
documented 1.x configuration and wire contracts. It joins the 1.0 behavior
support claim only when its evidence is complete.

## Supported targets

<!-- generated:targets -->

Declared atomic targets: 21.

| Target | Qualification | Framework | Platforms | Driving runtime |
|---|---|---|---|---|
| Backend contracts | qualified | Backend services | Linux, macOS, Windows | HTTP, OpenAPI |
| Jetpack Compose Android | qualified | Jetpack Compose | Android | ART, Appium UiAutomator2 |
| Electron | qualified | Electron | Linux, macOS, Windows | Chromium, Node.js, CDP |
| Flutter Android | qualified | Flutter | Android | Dart VM service (profile-mode build, liveness and isolate only), Appium UiAutomator2 |
| Flutter iOS | qualified | Flutter | iOS | Dart VM service, flutter drive |
| Linux GTK | qualified | GTK 3, GTK 4 | Linux | AT-SPI 2, GLib main loop |
| Linux Qt Quick/QML | qualified | Qt Quick/QML | Linux | AT-SPI 2, Qt 6, QML engine |
| Linux Qt Widgets | qualified | Qt Widgets | Linux | AT-SPI 2, Qt 6 |
| Linux wxWidgets | qualified | wxWidgets | Linux | AT-SPI 2, GTK backend |
| macOS Accessibility | qualified | SwiftUI, AppKit | macOS | Swift runtime, Accessibility API |
| React Native Android | qualified | React Native | Android | Hermes, Appium UiAutomator2 |
| React Native iOS | preview | React Native | iOS | Hermes, Appium XCUITest |
| SwiftUI iOS | qualified | SwiftUI | iOS | Swift runtime, Appium XCUITest |
| Tauri | preview | Tauri | Linux, Windows | WebKitGTK, tauri-driver |
| Terminal UI | qualified | Terminal applications | Linux, macOS | PTY, VT parser |
| Web Chromium | qualified | DOM applications | Linux, macOS, Windows | Node.js 20+, Playwright CDP |
| Web Firefox | qualified | DOM applications | Linux, macOS, Windows | Node.js 20+, Playwright |
| Web WebKit | qualified | DOM applications | Linux, macOS, Windows | Node.js 20+, Playwright |
| Windows Avalonia | qualified | Avalonia | Windows | .NET, UI Automation |
| Windows WinUI 3 | qualified | WinUI 3 | Windows | .NET, UI Automation, WinAppSDK |
| Windows WPF | qualified | WPF | Windows | .NET, UI Automation |

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
