# ReproIt 1.x compatibility

ReproIt uses three support tiers:

- Release-gated: built, tested, and exercised on the native runtime before a
  release can publish.
- CI-gated: built and tested on every change, with native execution in the
  dedicated native-gates workflow.
- Experimental: available for evaluation, but not covered by the 1.x stability
  promise.

| Target | 1.0 tier | Release evidence |
| --- | --- | --- |
| Web DOM | release-gated | Chromium, Firefox, and WebKit native gates |
| Android and React Native | release-gated | reset x86_64 Android emulator, Appium, UiAutomator2 |
| iOS and Flutter | release-gated | installed iOS simulator, Appium, XCUITest, Flutter drive |
| Windows desktop | release-gated | private native x86_64 Windows UIA against WPF, Avalonia, and WinUI 3 |
| Linux desktop | release-gated | x86_64 containers with AT-SPI fixtures |
| Terminal UI | release-gated | real PTY and VT parser gate |
| Electron and Tauri | release-gated | packaged fixture gates on native Linux workers |
| macOS AX | release-gated | permissioned SwiftUI fixture; evidence digest is attached to the release |
| Dear ImGui and Clay | CI-gated | instrumented native fixtures |
| Backend contract discovery | experimental | bounded opt-in fixtures only |
| Specialist oracles marked experimental | experimental | explicit invocation only |

Release archives are produced for macOS arm64 and x86_64, Linux arm64 and
x86_64, and Windows arm64 and x86_64. Native behavior evidence may use a single
documented architecture when the platform API is architecture-independent;
archive build and installer smoke still run on every shipped architecture.

Supported host prerequisites:

- Node.js 18 or later for the web runner.
- Current stable Rust for source builds.
- Platform SDKs and simulators named by `reproit doctor` for mobile targets.
- PostgreSQL for ReproIt Cloud. SQLite and MySQL are not supported Cloud stores.
