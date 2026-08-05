# Reproit

Reproit turns a software failure into an executable reproduction. Run the
repro, fix the bug, run it again to verify, keep it as a regression guard.

```sh
reproit occ_8f3a2c91    # re-execute the production failure on your machine
# fix the bug
reproit occ_8f3a2c91    # Fixed
reproit keep occ_8f3a2c91
reproit check           # every kept repro, replayed in CI
```

## Use cases

- **A production failure**: a backend SDK records the failing operation and its
  outbound exchanges in place. The occurrence id reproduces it on your machine.
- **A red CI build**: a failing test spools its capsule as a job artifact.
  `reproit check <capsule>` re-executes that exact run on a laptop.
- **A failure you can point at**: `reproit capture` preserves it from an
  application, a bounded command, or a signed support bundle.
- **A failure nobody has reported yet**: `reproit find` searches for failures,
  confirms each one, and minimizes it.
- **Regression protection**: `reproit keep` stores a repro in the repository.
  `reproit check` replays every kept repro in CI.

## What a repro is made of

Four properties turn a failure into something you can run:

1. **Trigger**: the request, input sequence, or test invocation, re-fired exactly.
2. **Dependency boundary**: every outbound call recorded with its response and
   served back at replay. The repro runs with the database stopped and the
   network denied.
3. **Determinism envelope**: clock, RNG seed, timezone, and runtime identity pinned.
4. **Oracle**: a machine-checkable statement of what failing means.

Replay re-executes your code with the recorded boundary and verdicts the
result: **Reproduced**, **Fixed**, **Diverged** (the code now makes different
calls, named), or **Inconclusive**. A similar crash is a different failure.
Unknown calls fail closed with the first mismatch named. There are no
similarity scores.

A repro is portable. It replays from a copy of the checkout at any path, on
another machine, with dependencies down. A capsule-backed repro also needs its
capsule. Capsules are encrypted and machine-local by default. Share the store
and set `REPROIT_CAPSULE_KEY` to replay elsewhere. A guard that cannot reach
its pinned capsule reports stale. It does not pass.

Execution stays on your machine. The cloud groups production failures into
bugs and hands out occurrence ids. It never executes your code and never holds
your source. The SDK classifies and redacts captured values at the source,
before they become replayable.

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

`reproit init` sets up a project. `reproit doctor` checks the platform and
reports concrete repairs.

## Server SDKs

Eight languages record a failing operation together with its outbound
exchanges and serve those exchanges back at replay: node, python, rust, go,
java, dotnet, php, ruby. Each ships an acceptance test that pins all four
verdicts on a real failure. `sdk/INVENTORY.json` gates the list. An SDK in
this repository runs its own suite on every push. A failure fails the build.

Per-target gates, platforms, and evidence live in
[compatibility](docs/compatibility.md).

## Where Reproit runs

<!-- generated:platforms -->

- **Backend services**: Linux, macOS, Windows
- **DOM applications**: Linux, macOS, Windows
- **Jetpack Compose**: Android
- **React Native**: Android, iOS
- **Flutter**: Android, iOS
- **Electron**: Linux, macOS, Windows
- **Tauri**: Linux, Windows
- **GTK 3**: Linux
- **GTK 4**: Linux
- **Qt Quick/QML**: Linux
- **Qt Widgets**: Linux
- **wxWidgets**: Linux
- **SwiftUI**: macOS, iOS
- **AppKit**: macOS
- **Avalonia**: Windows
- **WinUI 3**: Windows
- **WPF**: Windows
- **Terminal applications**: Linux, macOS

<!-- /generated:platforms -->

## Documentation

- [CLI reference](docs/cli.md): every command, and the exit-code contract
- [ReproIt in CI](docs/ci.md): the gate, and capturing a red build
- [Add ReproIt to a backend](docs/backend-sdk.md): install and one middleware, per language
- [What a repro is made of](docs/repros.md): the proof contract and platform coverage
- [Oracle reference](docs/oracles.md): what counts as a bug, and why
- [Security and data handling](docs/security.md): what leaves your machine
- [Compatibility](docs/compatibility.md): per-target gates and evidence
- [Migrating to 1.0](docs/1.0-migration.md): compatibility and evidence changes
- [Screen signatures](docs/signature.md): the cross-SDK identity spec
- [Configuration examples](docs/examples): a `reproit.yaml` per framework, gated by a test
- [ReproIt Cloud](docs/cloud/README.md): signup, SDKs, the dashboard, the API

Decision records (why things are the way they are, not how to use them) live
in [docs/decisions](docs/decisions).

Apache-2.0. See [SUPPORT.md](SUPPORT.md) for support and security reporting.
