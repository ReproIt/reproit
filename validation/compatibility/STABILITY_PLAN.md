# All-target stability plan

Generated from `validation/support-manifest.json` and
`validation/backends/evidence.json`. Do not edit by hand.

## Program end state

This program is complete only when all 21 atomic targets satisfy both axes:

- 21 of 21 targets are Stable under schema-3.
- 21 of 21 targets are `IndependentQualified` for production-to-local.
- No target has a typed promotion blocker or a missing qualification slot.
- The grandfathered schema-2 set is empty.
- Every native result, field campaign, and production chain names one exact commit.
- Generated compatibility surfaces agree with the canonical manifest.

Stable and production-to-local remain independent claims. Reaching Stable does not
silently grant production qualification, and a fixture replay never counts as an
independent application replay.

## Stable completion contract

A target becomes Stable only when the manifest validator can prove all of these:

1. Every owned native gate is required CI and passes at the exact CLI commit.
2. Two independent affected-versus-fixed application campaigns are retained.
3. Each application has three clean affected reproductions with one exact identity.
4. Each application has three reached-observation fixed controls.
5. Minimization and neighboring legal behavior are verified.
6. Clean and adversarial corpora, package installation, and manual review are retained.
7. Every typed blocker is removed because evidence closes it, never by prose alone.

## Qualification evidence contract

Before changing any `productionToLocal` value, extend the manifest schema so the value
is derived from a retained evidence record instead of a manually editable string.
Each record must bind the target id, qualification level, exact CLI and SDK commits,
application revision, origin type, Cloud occurrence identity, trusted local provider,
input and artifact hashes, replay command, behavioral assertion, reset, and cleanup.

The qualification levels have distinct gates:

1. `FixtureQualified`: run a disposable SDK-to-Cloud-to-trusted-local chain from a
   controlled fixture, retain every stage, assert exact local behavior, and clean up.
2. `IndependentQualified`: repeat the complete chain from a real independent affected
   application occurrence for the exact target. A renamed or modified built-in fixture
   does not qualify.

`validation/cloud/run-production-loop.sh` is the web fixture reference harness. It can
prove `FixtureQualified` only for the target it actually exercises. Add a bounded
target adapter, or an equally strict target-specific harness, before using the workflow
for mobile, desktop, terminal, or backend targets.

## Shared prerequisites and dependency order

1. Freeze one reviewable candidate commit. Update Cloud, SDK, package, and deployment
   pins to that exact commit before collecting promotion evidence.
2. Add evidence-backed qualification fields and validators before changing any
   `productionToLocal` state.
3. Close runner gaps per lane. Every runner must prove process ownership, readiness,
   reset, containment, bounded work, artifact retention, and cleanup on every exit.
4. Ratchet the existing Stable targets from schema-2 to schema-3. Keep them Stable only
   if the exact-commit gates and new corpus evidence pass.
5. Run owned native gates and field campaigns by lane, but validate and promote each
   target independently. Never use one framework's evidence for a neighboring target.
6. For every Stable target, retain a target-specific fixture production chain and advance
   only that target to `FixtureQualified`.
7. For every fixture-qualified target, retain a real independent application chain and
   advance only that target to `IndependentQualified`.
8. Regenerate all public surfaces and run the final all-target audit.

## Execution lanes

### Linux architecture matrix

- Route: arm64 containers on the local Docker worker; native x86_64 containers and host checks
  through `ssh black@zgx-5a09.local`, then `ssh strix`
- Targets: Backend contracts, Electron Linux, Linux GTK, Linux Qt Quick/QML, Linux Qt Widgets, Linux
  wxWidgets, Tauri Linux, Terminal UI, Web Chromium, Web Firefox, Web WebKit
- Lane prerequisite: Add a bounded native-x86 pack, execute, collect, and cleanup helper. Run Docker
  or Compose on `strix` for contained x86_64 applications. The local amd64 emulation failure is
  diagnostic only and cannot defer native Linux.
- Exit gate: every target-specific native command passes at the candidate
  commit, and retained reset and cleanup evidence validates.

### Android reset-AVD lane

- Route: Android Studio SDK, Appium, and UiAutomator2 on a reset installed AVD
- Targets: Jetpack Compose Android, Flutter Android, React Native Android
- Lane prerequisite: Record the AVD, API level, architecture, application id, permissions, network
  policy, snapshot state, and reset evidence for every campaign.
- Exit gate: every target-specific native command passes at the candidate
  commit, and retained reset and cleanup evidence validates.

### Apple native lane

- Route: Xcode simulators through `xcrun simctl`, Appium, and XCUITest for iOS; the local macOS host
  for Accessibility
- Targets: Flutter iOS, macOS Accessibility, React Native iOS, SwiftUI iOS
- Lane prerequisite: Record simulator or host identity, runtime, architecture, bundle id,
  permissions, network policy, boot state, and reset evidence.
- Exit gate: every target-specific native command passes at the candidate
  commit, and retained reset and cleanup evidence validates.

### Windows native x86_64 lane

- Route: `ssh black@zgx-5a09.local`, then `ssh strix`, then the forwarded native Windows guest via
  `validation/causal/run-windows-remote.sh`
- Targets: Windows Avalonia, Windows WinUI 3, Windows WPF
- Lane prerequisite: Use a fetchable exact commit. Prove the UIA session, process ownership,
  readiness, reset, bounded execution, artifact return, and cleanup.
- Exit gate: every target-specific native command passes at the candidate
  commit, and retained reset and cleanup evidence validates.

## Existing Stable ratchet

These targets are not finished merely because they are already Stable. They must pass
the current exact-commit gates, move to schema-3, and complete both production
qualification levels.

- Electron Linux: already satisfies every recorded qualification slot
- Flutter iOS: already satisfies every recorded qualification slot
- Terminal UI: already satisfies every recorded qualification slot
- Web Chromium: already satisfies every recorded qualification slot
- Web Firefox: already satisfies every recorded qualification slot
- Web WebKit: already satisfies every recorded qualification slot
- Windows WPF: already satisfies every recorded qualification slot

## Preview target worklists

Execute these worklists in lane order. A target leaves this section only when its
complete target-specific record validates.

### Backend contracts

- Target id: `backend-contract`
- Current maturity: Preview
- Environment: linux; x86_64
- Runtime bound: HTTP, OpenAPI
- Framework bound: Backend services
- Native gates:
  - `backend-contract`: required-ci in .github/workflows/native-gates.yml job `linux-hosted`
    ```sh
    bash validation/backend/cli-e2e/run.sh
    ```
- Field benchmark to create: `validation/field/backend-contract.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 6 candidate defects across 4
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `backend-contract.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### Jetpack Compose Android

- Target id: `compose-android`
- Current maturity: Preview
- Environment: android-emulator; x86_64
- Runtime bound: ART, Appium UiAutomator2
- Framework bound: Jetpack Compose
- Native gates:
  - `compose-android`: required-ci in .github/workflows/native-gates.yml job `android-hosted`
    ```sh
    bash validation/backends/with-appium.sh bash \
      examples/compose-fixture/compose-appium-smoke.sh
    ```
- Field benchmark to create: `validation/field/compose-android.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 2
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `compose-android.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### Flutter Android

- Target id: `flutter-android`
- Current maturity: Preview
- Environment: android-emulator; x86_64
- Runtime bound: Dart VM service, flutter drive
- Framework bound: Flutter
- Native gates:
  - `flutter-android`: required-ci in .github/workflows/native-gates.yml job `android-hosted`
    ```sh
    bash validation/backends/run-flutter-drive-android.sh
    ```
- Field benchmark to create: `validation/field/flutter-android.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 2
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
  - [unsupported-capability] a Flutter release APK is AOT compiled with the Dart VM service removed,
    so the declared runtime bound is not reachable from the release artifact. The campaign must
    either observe through a profile-mode build or the bound must be restated
- Promotion gate:
  - Set `flutter-android.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### Linux GTK

- Target id: `linux-gtk`
- Current maturity: Preview
- Environment: linux-container; x86_64
- Runtime bound: AT-SPI 2, GLib main loop
- Framework bound: GTK 3, GTK 4
- Native gates:
  - `linux-atspi-gtk`: required-ci in .github/workflows/native-gates.yml job `linux-containers`
    ```sh
    bash .github/scripts/atspi-scenario-e2e.sh
    ```
- Field benchmark to create: `validation/field/linux-gtk.json`
- Open blockers:
  - [environment-unreachable] the Linux GTK gate builds on the local x86_64 worker, but both owned
    fixture processes remain absent from the AT-SPI application bus before the first action
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 4
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `linux-gtk.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### Linux Qt Quick/QML

- Target id: `linux-qt-quick`
- Current maturity: Preview
- Environment: linux-container; x86_64
- Runtime bound: AT-SPI 2, Qt 6, QML engine
- Framework bound: Qt Quick/QML
- Native gates:
  - `linux-atspi-toolkits`: required-ci in .github/workflows/native-gates.yml job `linux-containers`
    ```sh
    bash examples/qt-fixture/qt-atspi-e2e.sh
    ```
- Field benchmark to create: `validation/field/linux-qt-quick.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 5
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `linux-qt-quick.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### Linux Qt Widgets

- Target id: `linux-qt-widgets`
- Current maturity: Preview
- Environment: linux-container; x86_64
- Runtime bound: AT-SPI 2, Qt 6
- Framework bound: Qt Widgets
- Native gates:
  - `linux-atspi-toolkits`: required-ci in .github/workflows/native-gates.yml job `linux-containers`
    ```sh
    bash examples/qt-fixture/qt-atspi-e2e.sh
    ```
- Field benchmark to create: `validation/field/linux-qt-widgets.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 3
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `linux-qt-widgets.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### Linux wxWidgets

- Target id: `linux-wxwidgets`
- Current maturity: Preview
- Environment: linux-container; x86_64
- Runtime bound: AT-SPI 2, GTK backend
- Framework bound: wxWidgets
- Native gates:
  - `linux-atspi-toolkits`: required-ci in .github/workflows/native-gates.yml job `linux-containers`
    ```sh
    bash examples/qt-fixture/qt-atspi-e2e.sh
    ```
- Field benchmark to create: `validation/field/linux-wxwidgets.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 5
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `linux-wxwidgets.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### macOS Accessibility

- Target id: `macos-ax`
- Current maturity: Preview
- Environment: macos; arm64
- Runtime bound: Swift runtime, Accessibility API
- Framework bound: SwiftUI, AppKit
- Native gates:
  - `macos-ax`: permissioned-self-hosted in .github/workflows/native-gates.yml job `macos-ax`
    ```sh
    bash validation/backends/run-macos-ax.sh
    ```
- Field benchmark to create: `validation/field/macos-ax.json`
- Open blockers:
  - [incomplete-evidence] no exact-commit evidence is recorded for the macos-ax native gate. The
    execution infrastructure is proven reachable on this host (macos-ax); the gate has simply not
    been run and retained against the candidate commit
  - [incomplete-evidence] no application campaign has been executed. 9 candidate defects across 5
    independent applications are qualified with verified revisions, but none has three clean
    affected reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `macos-ax.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### React Native Android

- Target id: `react-native-android`
- Current maturity: Preview
- Environment: android-emulator; x86_64
- Runtime bound: Hermes, Appium UiAutomator2
- Framework bound: React Native
- Native gates:
  - `react-native-android`: required-ci in .github/workflows/native-gates.yml job `android-hosted`
    ```sh
    bash validation/backends/with-appium.sh bash \
      validation/backends/run-react-native-android.sh
    ```
- Field benchmark to create: `validation/field/react-native-android.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 3
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `react-native-android.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### React Native iOS

- Target id: `react-native-ios`
- Current maturity: Preview
- Environment: ios-simulator; arm64
- Runtime bound: Hermes, Appium XCUITest
- Framework bound: React Native
- Native gates:
  - `react-native-ios`: required-ci in .github/workflows/native-gates.yml job `macos-swiftui`
    ```sh
    bash validation/backends/with-appium.sh bash validation/backends/with-ios-simulator.sh \
    bash validation/backends/run-react-native-ios.sh
    ```
- Field benchmark to create: `validation/field/react-native-ios.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 9 candidate defects across 2
    independent applications (BlueWallet, Joplin) are qualified with verified revisions, but neither
    has three clean affected reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `react-native-ios.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### SwiftUI iOS

- Target id: `swiftui-ios`
- Current maturity: Preview
- Environment: ios-simulator; arm64
- Runtime bound: Swift runtime, Appium XCUITest
- Framework bound: SwiftUI
- Native gates:
  - `swiftui-ios`: required-ci in .github/workflows/native-gates.yml job `macos-swiftui`
    ```sh
    bash validation/backends/with-appium.sh bash validation/backends/with-ios-simulator.sh \
    bash .github/scripts/appium-ios-swiftui-smoke.sh
    ```
- Field benchmark to create: `validation/field/swiftui-ios.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 10 candidate defects across 6
    independent applications are qualified with verified revisions, but none has three clean
    affected reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `swiftui-ios.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### Tauri Linux

- Target id: `tauri-linux`
- Current maturity: Preview
- Environment: linux; x86_64
- Runtime bound: WebKitGTK, tauri-driver
- Framework bound: Tauri
- Native gates:
  - `tauri`: required-ci in .github/workflows/native-gates.yml job `linux-containers`
    ```sh
    bash validation/backends/run-tauri.sh
    ```
- Field benchmark to create: `validation/field/tauri-linux.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 3
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `tauri-linux.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### Windows Avalonia

- Target id: `windows-avalonia`
- Current maturity: Preview
- Environment: windows-x86_64-interactive; x86_64
- Runtime bound: .NET, UI Automation
- Framework bound: Avalonia
- Native gates:
  - `windows-uia`: required-ci in .github/workflows/native-gates.yml job `windows-uia`; route:
    black@zgx-5a09.local -> strix -> reproit@localhost:2223
    ```sh
    powershell validation/backends/run-windows-desktop.ps1
    ```
- Field benchmark to create: `validation/field/windows-avalonia.json`
- Open blockers:
  - [incomplete-evidence] no exact-commit evidence is recorded for the windows-uia native gate. The
    execution infrastructure is proven reachable on this host (windows-vm); the gate has simply not
    been run and retained against the candidate commit
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 2
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `windows-avalonia.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

### Windows WinUI 3

- Target id: `windows-winui`
- Current maturity: Preview
- Environment: windows-x86_64-interactive; x86_64
- Runtime bound: .NET, UI Automation, WinAppSDK
- Framework bound: WinUI 3
- Native gates:
  - `windows-uia`: required-ci in .github/workflows/native-gates.yml job `windows-uia`; route:
    black@zgx-5a09.local -> strix -> reproit@localhost:2223
    ```sh
    powershell validation/backends/run-windows-desktop.ps1
    ```
- Field benchmark to create: `validation/field/windows-winui.json`
- Open blockers:
  - [incomplete-evidence] no exact-commit evidence is recorded for the windows-uia native gate. The
    execution infrastructure is proven reachable on this host (windows-vm); the gate has simply not
    been run and retained against the candidate commit
  - [incomplete-evidence] no application campaign has been executed. 8 candidate defects across 2
    independent applications (DLSS Swapper, UniGetUI) are qualified with verified revisions, but
    neither has three clean affected reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `windows-winui.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

## All-target production-to-local qualification

The following checklist covers every target, including the targets that were Stable
when this plan was generated. Each row is complete only at `IndependentQualified`.

| Target | Current maturity | Current qualification | Required end state |
|---|---|---|---|
| `backend-contract` | Preview | Unqualified | Stable + `IndependentQualified` |
| `compose-android` | Preview | Unqualified | Stable + `IndependentQualified` |
| `electron-linux` | Stable | Unqualified | Stable + `IndependentQualified` |
| `flutter-android` | Preview | Unqualified | Stable + `IndependentQualified` |
| `flutter-ios` | Stable | Unqualified | Stable + `IndependentQualified` |
| `linux-gtk` | Preview | Unqualified | Stable + `IndependentQualified` |
| `linux-qt-quick` | Preview | Unqualified | Stable + `IndependentQualified` |
| `linux-qt-widgets` | Preview | Unqualified | Stable + `IndependentQualified` |
| `linux-wxwidgets` | Preview | Unqualified | Stable + `IndependentQualified` |
| `macos-ax` | Preview | Unqualified | Stable + `IndependentQualified` |
| `react-native-android` | Preview | Unqualified | Stable + `IndependentQualified` |
| `react-native-ios` | Preview | Unqualified | Stable + `IndependentQualified` |
| `swiftui-ios` | Preview | Unqualified | Stable + `IndependentQualified` |
| `tauri-linux` | Preview | Unqualified | Stable + `IndependentQualified` |
| `tui` | Stable | Unqualified | Stable + `IndependentQualified` |
| `web-chromium` | Stable | FixtureQualified | Stable + `IndependentQualified` |
| `web-firefox` | Stable | FixtureQualified | Stable + `IndependentQualified` |
| `web-webkit` | Stable | FixtureQualified | Stable + `IndependentQualified` |
| `windows-avalonia` | Preview | Unqualified | Stable + `IndependentQualified` |
| `windows-winui` | Preview | Unqualified | Stable + `IndependentQualified` |
| `windows-wpf` | Stable | Unqualified | Stable + `IndependentQualified` |

For each row, use this atomic sequence:

1. Confirm the target's schema-3 Stable record at the candidate commit.
2. Run and retain the target-specific controlled fixture chain.
3. Validate the record and advance only that target to `FixtureQualified`.
4. Capture a real occurrence from an independent application at a pinned revision.
5. Ingest it through the SDK into a disposable isolated Cloud project.
6. Replay it locally with the declared trusted provider and assert exact behavior.
7. Retain immutable stage hashes plus reset and cleanup proof.
8. Validate the independent record and advance only that target to
   `IndependentQualified`.

## Final audit

- Assert Stable count: 21.
- Assert `IndependentQualified` count: 21.
- Assert Preview and Experimental counts: zero.
- Assert blocker count, missing qualification slots, and schema-2 targets: zero.
- Re-run every required-CI and native gate at the same exact commit.
- Review retained evidence for target identity, independent origin, hashes, reset,
  containment, and cleanup before publishing the generated claims.

## Required validation

```sh
python3 validation/compatibility/check.py --write
python3 validation/compatibility/check.py --check-generated
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

Then run every target's native command above on its declared environment and retain
the exact-commit evidence required by `validation/release/check-native-evidence.py`.
