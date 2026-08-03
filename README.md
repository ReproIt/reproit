# Reproit

Make software failures reproducible.

A report tells you what happened. A repro makes it happen again. Logs, screenshots, and
tickets are passive evidence: someone still has to rebuild the actions, state,
dependencies, timing, and environment, and hope the rebuild is faithful. Reproit captures
the failure as an executable reproduction instead: run it, fix the bug, run it again to
verify, keep it as a regression guard.

```sh
reproit occ_8f3a2c91    # the production failure, re-executed on your machine
# fix the bug
reproit occ_8f3a2c91    # Fixed
reproit keep occ_8f3a2c91
reproit check           # every kept repro, replayed in CI
```

## What a repro is made of

Four properties turn a failure into something you can run:

1. **Trigger**: the request, input sequence, or test invocation, re-fired exactly.
2. **Dependency boundary**: every outbound call recorded with its response, served back
   at replay, so the repro runs with the database stopped and the network denied.
3. **Determinism envelope**: clock, RNG seed, timezone, and runtime identity pinned.
4. **Oracle**: a machine-checkable statement of what failing means.

## From an unexplained failure to a verified fix

Replay re-executes your code with the recorded boundary and verdicts the result:
**Reproduced**, **Fixed**, **Diverged** (the code now makes different calls, named), or
**Inconclusive**. A similar crash is a different failure. Unknown calls fail closed with
the first mismatch named; there are no similarity scores. A repro is `reproduced` only
when a clean run reaches the observation point and matches the original failure identity.

A repro is portable: it replays from a copy of the checkout at any path, on another
machine, with dependencies down. Kept repros run in CI as regression guards; drifted
guards quarantine and report instead of silently re-baselining.

## Install

macOS and Linux:

```sh
curl -fsSL https://reproit.com/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://reproit.com/install.ps1 | iex
```

From source:

```sh
cargo install --git https://github.com/ReproIt/reproit --locked reproit
```

`reproit init` sets up a project; `reproit doctor` checks the platform and reports
concrete repairs.

## Where failures come from

- **Production**: a backend SDK records the failing operation and its outbound exchanges
  in place; the occurrence id reproduces it locally.
- **CI**: a failing test spools its capsule as a job artifact; `reproit check <capsule>`
  re-executes that exact run on a laptop.
- **A known failure you can point at**: `reproit capture` preserves it from an
  application, bounded command, or signed support bundle.
- **Discovery**: `reproit find` can also search for failures you have not seen yet, and
  confirms and minimizes anything it reports.

Execution stays on your side of the line. The cloud groups production failures into
bugs and hands out occurrence ids; it never executes your code and never holds your
source. Captured values are classified and redacted at the source before they become
replayable.

## Backend services are where this goes deepest

Eight server SDKs record a failing operation together with its outbound exchanges, and
serve those exchanges back at replay: **node, python, rust, go, java, dotnet, php, ruby**.
Each one ships its own acceptance test that pins all four verdicts on a real failure
(reproduced with the dependencies stopped, fixed once the bug is repaired, reproduced
again after the fix is reverted, diverged when a recorded exchange goes missing), so the
capsule any of them writes re-executes on another machine with the database down and the
network denied. `sdk/INVENTORY.json` gates that list: an SDK in this repository runs its
own suite on every push, and a failure fails the build.

The UI targets below use the same capture, replay, and verdict machinery through their
own runners. Per-target gates, bounds, and evidence live in
[compatibility](docs/compatibility.md).

## Where Reproit runs

<!-- generated:platforms -->

**Backend services**

- Backend contracts

**Web**

- Web Chromium
- Web Firefox
- Web WebKit

**Desktop webview**

- Electron Linux
- Tauri Linux

**Desktop native**

- Linux GTK
- Linux Qt Quick/QML
- Linux Qt Widgets
- Linux wxWidgets
- macOS Accessibility
- Windows Avalonia
- Windows WinUI 3
- Windows WPF

**Mobile**

- Jetpack Compose Android
- React Native Android
- React Native iOS
- SwiftUI iOS

**Flutter**

- Flutter Android
- Flutter iOS

**Terminal**

- Terminal UI

<!-- /generated:platforms -->

## Documentation

- [CLI reference](docs/cli.md)
- [compatibility](docs/compatibility.md)
- [causal capsules](docs/causal-capsules.md)
- [architecture](docs/architecture.md)
- [oracle reference](docs/oracles.md)
- [data handling](docs/data-handling.md)
- [release contract](docs/release.md)

Apache-2.0. See [SUPPORT.md](SUPPORT.md) for support and security reporting.
