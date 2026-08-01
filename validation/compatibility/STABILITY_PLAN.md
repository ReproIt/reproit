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

- Backend contracts: already satisfies every recorded qualification slot
- Jetpack Compose Android: already satisfies every recorded qualification slot
- Electron Linux: already satisfies every recorded qualification slot
- Flutter iOS: already satisfies every recorded qualification slot
- Linux GTK: already satisfies every recorded qualification slot
- Linux Qt Widgets: already satisfies every recorded qualification slot
- Linux wxWidgets: already satisfies every recorded qualification slot
- SwiftUI iOS: already satisfies every recorded qualification slot
- Terminal UI: already satisfies every recorded qualification slot
- Web Chromium: already satisfies every recorded qualification slot
- Web Firefox: already satisfies every recorded qualification slot
- Web WebKit: already satisfies every recorded qualification slot
- Windows Avalonia: already satisfies every recorded qualification slot
- Windows WinUI 3: already satisfies every recorded qualification slot
- Windows WPF: already satisfies every recorded qualification slot

## Preview target worklists

Execute these worklists in lane order. A target leaves this section only when its
complete target-specific record validates.

### Flutter Android

- Target id: `flutter-android`
- Current maturity: Preview
- Environment: android-emulator; x86_64
- Runtime bound: Dart VM service (profile-mode build, dump and profile RPCs only), flutter drive
- Framework bound: Flutter
- Native gates:
  - `flutter-android`: required-ci in .github/workflows/native-gates.yml job `android-hosted`
    ```sh
    bash validation/backends/run-flutter-drive-android.sh
    ```
- Field benchmark to create: `validation/field/flutter-android.json`
- Open blockers:
  - [incomplete-evidence] no application campaign has been executed. Both revisions of LocalSend now
    build profile APKs on the x86_64 executor and the profile observation channel is measured, but
    no scenario, no three affected reproductions and no three fixed controls exist. The scenario
    must observe through a tree dump or an IO profile: the profile build registers 26 extension RPCs
    including debugDumpApp, debugDumpRenderTree and the semantics dumps, exposes no widget
    inspector, and refuses evaluate with 'Debugger is disabled in AOT mode'
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
  - [environment-unreachable] the campaign host has no x86_64 Android system image, so the arch
    bound is satisfied only on the lane's own x86_64 executor (black@zgx-5a09.local then strix).
    That executor is now proven for this target: both LocalSend revisions build profile APKs for
    android-x64 there and the lane's AVD recipe boots an x86_64 API 36 emulator inside the worker
    image. What is not yet built there is the campaign runner itself, so no scenario has run
- Promotion gate:
  - Set `flutter-android.maturity` to `stable` only after the benchmark,
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
  - [incomplete-evidence] one of the two required applications has a freshly mined, verified-
    discriminating pair and the second does not exist yet. kalk 67e5d3d versus 662aa91, bugs.kde.org
    475907, separates cleanly through the AT-SPI text interface: after typing 1+1 and pressing
    equals twice the affected build shows nothing and the fixed build still shows 2, while the first
    equals yields 2 on both. What is missing is a second independent repository with a pair that
    separates, and then three affected reproductions, three fixed controls and a corpus
  - [incomplete-evidence] six candidate applications have been eliminated by running or building
    them, not by reading. kalk 507525 and kclock 505636 both fail to separate their revisions on
    this worker. elisa 476532 fails to separate because Space never reaches the focused player
    button on either revision, the global Play-Pause shortcut consuming it first, while Return
    activates the button on both. marknote needs KF 6.21 against trixie's 6.13. francis 480512
    cannot be linked at 1ccca90 on this toolchain, its static library and its executable both moc
    Controller, and patching the application under test would compromise the campaign. kclock
    464252, the timer add-a-minute defect, is Qt5-era at 1a02e4d and will not configure against a
    Qt6 image at all; a current-master kclock defect would additionally need its own kclockd
    running, because TimerModel reaches the daemon over D-Bus rather than a config file
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
- Promotion gate:
  - Set `linux-qt-quick.maturity` to `stable` only after the benchmark,
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
- Field benchmark: `validation/field/react-native-android.json`
- Open blockers:
  - [incomplete-evidence] one of the two required application campaigns is complete. joplin, issue
    15004, affected de637847 and fixed 623da377: three affected reproductions all landing on react-
    native-navigation:hardware-back-exits-app-after-deleted-notebook, three fixed controls all
    returning to the note list, neighbouring legal behaviour holding on both revisions, and a
    passing cleanup audit. MissingCore/Music is discarded rather than retried: with all four
    fixtures confirmed in MediaStore before launch and the media permission granted, its library
    reports zero tracks for the full 300 second bound, which is a property of that application and
    not a gap in the harness. streetwriters/notesnook replaces it and is no longer blocked on a
    build. Both release APKs are built on the strix x86_64 worker, affected 14f727d6 sha256
    0e4cc6f1e804b5a40da4e7e798a3201c9c88bfa2c1e8babc5c5e3b42a2d24805 and fixed 7c3fdab6 sha256
    06302f8e554df5460d2f870a76809dc4c2b8374e269b0ad509d83397f6dc09d8, which needed two inputs
    neither recorded before: Gradle 9.0.0 refuses JDK 21 with a missing JvmVendorSpec.IBM_SEMERU
    field and needs JDK 17, and npm run tx mobile:build has to build the workspace packages before
    the bundle or rspack compiles with 402 unresolved @notesnook imports, emits nothing, and
    repack's CLI exits 0 reporting only an ENOENT on index.bundle. The pair then discriminated on a
    pixel_6 x86_64 AVD inside the pinned worker image, on issue 7348: after one note is linked to
    notebook Alpha and then relinked to Beta in single-select mode, the affected build leaves the
    note row reading Alpha and Beta while the fixed build leaves it reading Beta alone. What is
    missing is the campaign itself: react_native_android_campaign.py has no notesnook observer, so
    no benchmark run has been performed and none is written up
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target. The subjects are chosen and follow from the notesnook
    observable, which is the pair of notebook sets linked before and after the relink: a clean
    subject is the fixed build's plain first link, and the two adversarial subjects are the affected
    build with the selection reverted by the header restore button, and the affected build relinking
    from two already-linked notebooks, where multi-select legitimately keeps them. Both need the
    notesnook observer that the first blocker names
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
- Field benchmark: `validation/field/react-native-ios.json`
- Open blockers:
  - [incomplete-evidence] one of the two required application campaigns is complete, and the corpus
    is retained and passing. joplin, issue 15972, affected 7d90db0b and fixed 2fa45a5a, ran on
    disposable iPhone 16 Pro simulators on iOS 26.2: three affected reproductions all landing on
    react-native-layout:note-row-padding-outside-touch-target with the note row hit area measured at
    370 by 20 and the tap at (201, 118) swallowed, three fixed controls all reaching the same
    observation with the hit area at 402 by 52 and the identical coordinate opening the note,
    neighbouring legal behaviour holding on both revisions, and the owned simulator deleted. The
    corpus is one clean and two adversarial subjects with zero false positives. The second
    application is what remains: BlueWallet is excluded outright because a pinned dependency
    repository returns 404, and streetwriters/notesnook is selected. Its defect is JavaScript, so
    the trigger and the observable proven on Android carry over unchanged: issue 7348, affected
    14f727d6 and fixed 7c3fdab6, one note relinked from notebook Alpha to Beta in single-select
    mode, where the affected build leaves the note in both. Its iOS build is the missing input. The
    bootstrap recipe is now known and is the project's own, npm ci --ignore-scripts then npm run
    bootstrap -- --scope=mobile then npm run tx mobile:build, without which the bundle cannot
    resolve @notesnook/intl. It did not complete on this host: syspolicyd has been wedged near a
    full core since 6 July, a sampled /bin/sh script phase sat in _dyld_start having executed no
    instruction, and the bootstrap and the xcodebuild script phases both stall there repeatedly
- Promotion gate:
  - Set `react-native-ios.maturity` to `stable` only after the benchmark,
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
  - [incomplete-evidence] one of the two required independent application campaigns is executed. cc-
    switch issue 4302 has three clean affected reproductions on one identity, three reached-
    observation fixed controls, a minimized trigger, and both controls, and the clean and
    adversarial corpus now measures the oracle on known-good subjects. The second application is
    missing for a measured reason, not an untried one: this probe observes only the WebKitGTK
    webview DOM, and readest and note-gen both need a native GTK window, readest to seed a library
    and note-gen to observe a file chooser. readest builds in the worker and its argv path was
    measured to open books transiently without importing them, so its library cannot be seeded
    through this channel
- Promotion gate:
  - Set `tauri-linux.maturity` to `stable` only after the benchmark,
    qualification slots, required-CI gates, and blockers validate together.
- Qualification dependency:
  - After Stable, run the target-specific fixture chain and then a distinct
    independent application chain. Retain and validate both records.

## All-target production-to-local qualification

The following checklist covers every target, including the targets that were Stable
when this plan was generated. Each row is complete only at `IndependentQualified`.

| Target | Current maturity | Current qualification | Required end state |
|---|---|---|---|
| `backend-contract` | Stable | Unqualified | Stable + `IndependentQualified` |
| `compose-android` | Stable | Unqualified | Stable + `IndependentQualified` |
| `electron-linux` | Stable | Unqualified | Stable + `IndependentQualified` |
| `flutter-android` | Preview | Unqualified | Stable + `IndependentQualified` |
| `flutter-ios` | Stable | Unqualified | Stable + `IndependentQualified` |
| `linux-gtk` | Stable | Unqualified | Stable + `IndependentQualified` |
| `linux-qt-quick` | Preview | Unqualified | Stable + `IndependentQualified` |
| `linux-qt-widgets` | Stable | Unqualified | Stable + `IndependentQualified` |
| `linux-wxwidgets` | Stable | Unqualified | Stable + `IndependentQualified` |
| `macos-ax` | Preview | Unqualified | Stable + `IndependentQualified` |
| `react-native-android` | Preview | Unqualified | Stable + `IndependentQualified` |
| `react-native-ios` | Preview | Unqualified | Stable + `IndependentQualified` |
| `swiftui-ios` | Stable | Unqualified | Stable + `IndependentQualified` |
| `tauri-linux` | Preview | Unqualified | Stable + `IndependentQualified` |
| `tui` | Stable | Unqualified | Stable + `IndependentQualified` |
| `web-chromium` | Stable | FixtureQualified | Stable + `IndependentQualified` |
| `web-firefox` | Stable | FixtureQualified | Stable + `IndependentQualified` |
| `web-webkit` | Stable | FixtureQualified | Stable + `IndependentQualified` |
| `windows-avalonia` | Stable | Unqualified | Stable + `IndependentQualified` |
| `windows-winui` | Stable | Unqualified | Stable + `IndependentQualified` |
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
