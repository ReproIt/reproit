# ReproIt 1.x compatibility

ReproIt uses three implementation support tiers:

- Release-gated fixture: built, tested, and exercised against an owned native
  fixture before a release can publish.
- CI-gated: built and tested on every change, with native execution in the
  dedicated native-gates workflow.
- Experimental: available for evaluation, but not covered by the 1.x stability
  promise.

These tiers establish that Reproit's adapter works with a controlled application
on the named runtime. They are not a claim that arbitrary third-party
applications have been validated. Broad field compatibility requires clean
evidence from at least two independent real applications per target. That field
matrix is not complete for 1.0, so the 1.0 contract is the documented adapter
and fixture behavior only.

| Target | 1.0 tier | Native fixture evidence | Field evidence |
| --- | --- | --- | --- |
| Web Chromium | release-gated fixture | Chromium gate and captured log | two-app matrix open |
| Web Firefox and WebKit | nightly fixture | Playwright engine gates | two-app matrix open |
| React Native Android | release-gated fixture | reset emulator, Appium, UiAutomator2 | two-app matrix open |
| Jetpack Compose Android | nightly fixture | reset emulator, Appium, UiAutomator2 | two-app matrix open |
| Flutter iOS | release-gated fixture | disposable simulator and Flutter drive | two-app matrix open |
| SwiftUI iOS | nightly fixture | disposable simulator, Appium, XCUITest | two-app matrix open |
| Windows desktop | release-gated fixture | native x86_64 UIA on WPF, Avalonia, WinUI 3 | two-app matrix open |
| Linux desktop | nightly fixture | x86_64 containers with AT-SPI fixtures | two-app matrix open |
| Terminal UI | change-gated fixture | real PTY and VT parser gate | clean reverification open |
| Electron and Tauri | nightly fixture | packaged fixtures on Linux workers | two-app matrix open |
| macOS AX | release-gated fixture | permissioned SwiftUI fixture and captured log | two-app matrix open |
| Dear ImGui and Clay | CI-gated | instrumented native fixtures | two-app matrix open |
| Backend contract discovery | experimental | bounded opt-in fixtures only | not promised |
| Experimental specialist oracles | experimental | explicit invocation only | not promised |

The public-issue ledger in `docs/issue-reproduction-audit.md` is deliberately
negative evidence: reviewed reports do not become Reproit findings until the
application was run and the result was cleanly reproduced. Legacy evidence is a
candidate only and must be reverified before it can enter the field matrix.

Release archives are produced for macOS arm64 and x86_64, Linux arm64 and
x86_64, and Windows arm64 and x86_64. Native behavior evidence may use a single
documented architecture when the platform API is architecture-independent;
archive build and installer smoke still run on every shipped architecture.

Supported host prerequisites:

- Node.js 18 or later for the web runner.
- Current stable Rust for source builds.
- Platform SDKs and simulators named by `reproit doctor` for mobile targets.
- PostgreSQL for ReproIt Cloud. SQLite and MySQL are not supported Cloud stores.
