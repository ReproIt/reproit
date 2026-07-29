# Reproit

Reproit turns a software failure into an exact local reproduction, minimizes the
trigger, and keeps the fixed case as a regression guard.

```sh
reproit init
reproit doctor
reproit find
reproit fnd_0123456789abcdef
# fix the bug
reproit keep fnd_0123456789abcdef --as checkout-freeze
reproit check
```

## Product workflows

- `capture` preserves a known failure from an application, bounded command, or
  signed support bundle.
- `find` discovers failures, confirms their identities, and minimizes successful
  reproductions.
- `check` replays saved cases and distinguishes an exact failure, a different
  failure, flakiness, stale evidence, and infrastructure failure.
- `list` shows guards, blocked candidates, and confirmed production bugs.

Direct finding, occurrence, and saved-reproduction IDs run one exact case.
`init`, `doctor`, `keep`, and `login` support the four workflows. Older
specialist commands remain compatibility aliases, not separate product paths.

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/ReproIt/reproit/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/ReproIt/reproit/main/install.ps1 | iex
```

From source:

```sh
cargo install --git https://github.com/ReproIt/reproit --locked reproit
```

Run `reproit doctor` after installation. It checks the selected platform and
reports concrete repairs for missing prerequisites.

## Exactness and recall

Reproit measures the full funnel:

```text
observed -> captured -> eligible -> executed -> exact -> minimized -> fixed -> guarded
```

A result is `reproduced` only when a clean run reaches the observation point and
matches the original failure identity. A similar crash, timeout, or error is a
different failure. Missing evidence and unsupported capabilities produce typed
blockers.

Evidence describes facts and requirements. Executable commands, directories,
timeouts, environment values, and cleanup actions come only from trusted
adapters or the current checkout. Local execution may use a host process,
container, simulator, emulator, VM, or permissioned hardware without mutating
production.

## Compatibility

<!-- generated:compatibility -->

| Target | Compatibility | Backend | Production-to-local |
|---|---|---|---|
| Backend contracts | Stable | HTTP, OpenAPI | Unqualified |
| Jetpack Compose Android | Stable | ART, Appium UiAutomator2 | Unqualified |
| Electron Linux | Stable | Chromium, Node.js, CDP | Unqualified |
| Flutter Android | Preview | Dart VM service, flutter drive | Unqualified |
| Flutter iOS | Stable | Dart VM service, flutter drive | Unqualified |
| Linux GTK | Preview | AT-SPI 2, GLib main loop | Unqualified |
| Linux Qt Quick/QML | Preview | AT-SPI 2, Qt 6, QML engine | Unqualified |
| Linux Qt Widgets | Preview | AT-SPI 2, Qt 6 | Unqualified |
| Linux wxWidgets | Preview | AT-SPI 2, GTK backend | Unqualified |
| macOS Accessibility | Preview | Swift runtime, Accessibility API | Unqualified |
| React Native Android | Preview | Hermes, Appium UiAutomator2 | Unqualified |
| React Native iOS | Preview | Hermes, Appium XCUITest | Unqualified |
| SwiftUI iOS | Preview | Swift runtime, Appium XCUITest | Unqualified |
| Tauri Linux | Preview | WebKitGTK, tauri-driver | Unqualified |
| Terminal UI | Stable | PTY, VT parser | Unqualified |
| Web Chromium | Stable | Node.js 20+, Playwright CDP | FixtureQualified |
| Web Firefox | Stable | Node.js 20+, Playwright | FixtureQualified |
| Web WebKit | Stable | Node.js 20+, Playwright | FixtureQualified |
| Windows Avalonia | Preview | .NET, UI Automation | Unqualified |
| Windows WinUI 3 | Preview | .NET, UI Automation, WinAppSDK | Unqualified |
| Windows WPF | Stable | .NET, UI Automation | Unqualified |

<!-- /generated:compatibility -->

Compatibility is atomic. A target becomes Stable only after two independent
affected-versus-fixed application campaigns, repeated clean runs, exact identity
matching, minimization, neighboring-behavior checks, manual review, and
exact-commit native evidence. The canonical state is generated from
[`validation/support-manifest.json`](validation/support-manifest.json).

## Causal capture

Every shipped SDK registers against one generated `CaptureBatch` schema. CI
checks semantic parsing, unknown-field rejection, SDK registration, and the
canonical fixture bytes:

```sh
cargo run -q -p reproit-protocol --bin capture-schema
```

Captured values are classified and redacted before they become replayable or
exportable. Work, retries, output, memory growth, and cleanup are bounded.

## Documentation

- [CLI reference](docs/cli.md)
- [compatibility and promotion](docs/compatibility.md)
- [causal capsules](docs/causal-capsules.md)
- [architecture](docs/architecture.md)
- [oracle reference](docs/oracles.md)
- [data handling](docs/data-handling.md)
- [release contract](docs/release.md)

Apache-2.0. See [SUPPORT.md](SUPPORT.md) for support and security reporting.
