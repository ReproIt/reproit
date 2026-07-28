# Reproit

Reproit turns a software failure into an exact local reproduction, then keeps the fixed case as a
regression guard.

The product goal has two separate measurements:

- High recall: capture and attempt the broadest useful set of bugs developers actually face.
- Exact reproduction: a reproduced result must match the original failure identity. A different
  crash, timeout, or error is not a success.

Reproit measures the complete funnel as `observed -> captured -> eligible -> executed -> exact ->
minimized -> fixed -> guarded`. Work is prioritized by the largest measured drop-off, not by the
number of unverified findings.

## The core loop

```sh
reproit init
reproit doctor
reproit find
reproit fnd_...
# fix the bug
reproit keep fnd_...
reproit check
```

Four commands carry the normal workflows:

- `capture` preserves a known failure from a configured app, command, or signed support bundle.
- `find` discovers unknown failures, confirms exact identity, and minimizes successful cases.
- `check` replays saved cases and distinguishes pass, exact failure, different failure, flaky,
  stale, and infrastructure failure.
- `list` shows guards, blocked candidates, or confirmed production bugs.

Direct ids such as `fnd_...`, `occ_...`, and saved `@names` reproduce one exact case. `init`,
`doctor`, `keep`, and `login` support the loop. Older specialist commands remain compatibility
aliases for existing automation, but are not separate product workflows.

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

The first web run provisions its Playwright runner. Native targets need their platform toolchain.
`reproit doctor` reports missing prerequisites and a concrete repair for each failed check.

## Capture a known failure

Capture a failing command:

```sh
reproit capture --include-output -- npm test -- checkout.test.ts
```

Capture an already-running configured application:

```sh
reproit capture --attach --title "checkout freezes" --record-video
```

Import a signed offline support bundle:

```sh
reproit capture --bundle support.rpb
reproit occ_...
```

Evidence describes facts and requirements. It cannot provide an executable command. Commands,
working directories, environment values, timeouts, and cleanup actions must come from a trusted
adapter or the current checkout. Concurrency, distributed-system, performance, hardware, kernel,
and environment-dependent requirements bind only to providers that explicitly declare that
capability. Missing support produces a typed blocker.

## Find, fix, and guard

```sh
reproit find --record-video
reproit fnd_0123456789abcdef
# edit the application
reproit fnd_0123456789abcdef
reproit keep fnd_0123456789abcdef --as checkout-freeze
reproit check
reproit list
```

Reproit does not count a clean run as a fix until the affected revision reproduces repeatedly, the
fixed revision repeatedly does not reproduce, the exact observation point is reached in both
campaigns, the case is minimized, and neighboring behavior remains intact.

## Compatibility

| Platform | 1.0 release | Compatibility | Backend |
|---|---:|---|---|
| Web DOM apps, Chromium | Released | Stable | Playwright Chromium |
| Web DOM apps, Firefox and WebKit | Released | Preview | Playwright |
| Flutter | Released | Preview | flutter drive and VM service |
| React Native and native mobile | Released | Preview | Appium |
| macOS native | Released | Preview | Accessibility, validated with SwiftUI |
| Windows native | Released | Preview | UI Automation, validated with WPF, Avalonia, WinUI 3 |
| Linux native | Released | Preview | AT-SPI, validated with GTK, Qt Widgets, Qt Quick/QML, wxWidgets |
| Terminal UIs | Released | Preview | PTY and VT parser |
| Electron | Released | Preview | Chromium and CDP |
| Tauri | Released | Preview | system webview through tauri-driver |

Preview is atomic. One passing toolkit does not promote a family. Promotion requires independent
affected and fixed application campaigns, repeated clean resets, exact identity, minimization,
neighbor checks, manual review, and exact-commit evidence from the native execution environment.
The generated status is in
[`validation/compatibility/STATUS.md`](validation/compatibility/STATUS.md).

## Shared causal capture contract

All shipped SDKs register against one source-neutral `CaptureBatch` contract. The Rust owner
generates its JSON Schema:

```sh
cargo run -q -p reproit-protocol --bin capture-schema
```

CI validates semantic parsing, unknown-field rejection, complete SDK registration, and the exact
SHA-256 of the canonical fixture. See [the SDK guide](sdk/README.md).

## Evidence and privacy

Reproit separates source claims from executable plans and from oracle verdicts. Restricted values
must be redacted before they can become replayable or exportable. Every run is bounded by explicit
timeouts, event limits, output limits, reset policy, and cleanup ownership.

Read:

- [CLI reference](docs/cli.md)
- [compatibility and promotion](docs/compatibility.md)
- [causal capture and reproduction](docs/causal-capsules.md)
- [data handling](docs/data-handling.md)
- [architecture](docs/architecture.md)
- [oracle authority](docs/oracles.md)
- [1.x stability](docs/stability.md)

Apache-2.0. See [SUPPORT.md](SUPPORT.md) for support and security reporting.
