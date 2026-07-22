# ReproIt 1.x compatibility

ReproIt uses three product support tiers:

- Stable: covered by the 1.x compatibility promise and release-gated against
  owned fixtures plus the published independent-application field gate.
- Preview: built, tested, and exercised against an owned native fixture, but
  not covered by the 1.x field-compatibility promise.
- Experimental: available for evaluation, but not covered by the 1.x stability
  promise.

These tiers establish that Reproit's adapter works with a controlled application
on the named runtime. They are not a claim that arbitrary third-party
applications have been validated. Stable field compatibility requires clean
evidence from at least two independent real applications per target. Every
target below is released in 1.0 as a checksummed artifact. Chromium web is the
focused 1.0 stable compatibility target. Every other released adapter remains
preview compatibility until its field gate closes without weakening its native
fixture gate.

| Target | 1.0 support | Native fixture evidence | Field evidence |
| --- | --- | --- | --- |
| Web Chromium | stable | Chromium gate and captured log | VERT and Slidev gate complete |
| Web Firefox and WebKit | preview | Playwright engine gates | two-app matrix open |
| React Native Android | preview | reset emulator, Appium, UiAutomator2 | two-app matrix open |
| Jetpack Compose Android | preview | reset emulator, Appium, UiAutomator2 | two-app matrix open |
| Flutter iOS | preview | disposable simulator and Flutter drive | two-app matrix open |
| SwiftUI iOS | preview | disposable simulator, Appium, XCUITest | two-app matrix open |
| Windows desktop | preview | native x86_64 UIA on WPF, Avalonia, WinUI 3 | two-app matrix open |
| Linux desktop | preview | x86_64 containers with AT-SPI fixtures | two-app matrix open |
| Terminal UI | preview | real PTY and VT parser gate | clean reverification open |
| Electron and Tauri | preview | packaged fixtures on Linux workers | two-app matrix open |
| macOS AX | preview | permissioned SwiftUI fixture and captured log | two-app matrix open |
| Dear ImGui and Clay | preview | instrumented native fixtures | two-app matrix open |
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
