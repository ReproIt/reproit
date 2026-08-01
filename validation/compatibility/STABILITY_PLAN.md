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
- Linux Qt Quick/QML: already satisfies every recorded qualification slot
- Linux Qt Widgets: already satisfies every recorded qualification slot
- Linux wxWidgets: already satisfies every recorded qualification slot
- React Native Android: already satisfies every recorded qualification slot
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
  - [incomplete-evidence] the first application's pair is now verified to discriminate but no
    campaign has been run. LocalSend 3ec2d77 against 9e4a598, issue 2904, separates on the receive
    page: with the message 'https://example.com some extra text' delivered by a prepare-upload
    request on loopback, the affected build shows 'sent you a link:' and the fixed build shows 'sent
    you a message:'. A bare URL stays a link on both revisions, which is the neighbouring legal
    control. What is missing is the campaign runner on the executor, three affected reproductions
    and three fixed controls
  - [incomplete-evidence] only one application is verified and the second is untouched. gopeed
    remains the recorded second candidate and no revision of it has been fetched, built or run
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target. The adversarial subject is already identified and
    measured: a bare URL on the fixed build legitimately produces the link subtitle, which is what
    the defect produces
- Promotion gate:
  - Set `flutter-android.maturity` to `stable` only after the benchmark,
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
    affected reproductions and three reached-observation fixed controls. An execution attempt on
    2026-08-01 got no further than the first launch: see the environment-unreachable blocker below,
    which is now the input this one waits on
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
  - [environment-unreachable] the campaign cannot be executed on the only permissioned macOS host,
    and this is the same fault that stalls the required-CI runner rather than a second one. This
    host's syspolicyd has been pinned near 98 percent CPU since 6 July, and it performs Gatekeeper
    assessment, so any Mach-O file that has not been assessed at its current path stalls in
    _dyld_start at 0 percent CPU indefinitely. Ten bounded launches separate what runs from what
    does not: signing, ad-hoc re-signing, architecture, the Rosetta path, the directory, and the
    process that wrote the file all make no difference, and cp of a system binary that runs
    instantly in place also stalls at its new path, so the assessment is keyed on the file rather
    than on the code. It reaches builds too, not only launches: an xcodebuild of Platypus compiled
    and then stopped in a run-script phase with /bin/sh itself stuck in _dyld_start. No application
    can therefore be built or launched, so no reproduction run of any kind is possible. Nothing here
    disqualifies any of the 9 candidates: none was built, no accessibility tree was dumped, and no
    revision pair was tested for discrimination. Repairing syspolicyd needs root
- Promotion gate:
  - Set `macos-ax.maturity` to `stable` only after the benchmark,
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
| `linux-qt-quick` | Stable | Unqualified | Stable + `IndependentQualified` |
| `linux-qt-widgets` | Stable | Unqualified | Stable + `IndependentQualified` |
| `linux-wxwidgets` | Stable | Unqualified | Stable + `IndependentQualified` |
| `macos-ax` | Preview | Unqualified | Stable + `IndependentQualified` |
| `react-native-android` | Stable | Unqualified | Stable + `IndependentQualified` |
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
