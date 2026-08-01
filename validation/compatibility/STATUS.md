# Compatibility qualification status

Generated from `validation/support-manifest.json`. Do not edit by hand.

## Backend contracts

- Maturity: Stable
- Scope: Bounded backend scan, fuzz, replay, proof, and runtime capture
- Promotion standard: schema-3
- Native gates: backend-contract
- Field benchmark: validation/field/backend-contract.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux
- Architectures: x86_64
- Runtimes: HTTP, OpenAPI
- Frameworks: Backend services
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Jetpack Compose Android

- Maturity: Stable
- Scope: Jetpack Compose on a reset Android emulator through Appium UiAutomator2
- Promotion standard: schema-3
- Native gates: compose-android
- Field benchmark: validation/field/compose-android.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: android-emulator
- Architectures: x86_64
- Runtimes: ART, Appium UiAutomator2
- Frameworks: Jetpack Compose
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Electron Linux

- Maturity: Stable
- Scope: Packaged Electron applications on Linux workers
- Promotion standard: schema-3
- Native gates: electron
- Field benchmark: validation/field/electron-linux.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux
- Architectures: x86_64
- Runtimes: Chromium, Node.js, CDP
- Frameworks: Electron
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Flutter Android

- Maturity: Preview
- Scope: Flutter profile-mode builds on a reset Android emulator. A release APK is AOT compiled with
  no Dart VM service, and a profile APK exposes the service for tree dumps and IO profiles but
  neither expression evaluation nor the widget inspector. Both facts were measured rather than
  assumed
- Promotion standard: schema-3
- Native gates: flutter-android
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: android-emulator
- Architectures: x86_64
- Runtimes: Dart VM service (profile-mode build, dump and profile RPCs only), flutter drive
- Frameworks: Flutter
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed. The scenario is now designed
    against what the profile channel can actually show, and it needs no second device: a prepare-
    upload request on loopback carrying exactly one text file makes ReceiveSessionState.message
    return that text, which puts the receive page into the state under test, and the observable is
    the subtitle string, 'sent you a link:' when isLink is true against 'sent you a message:' when
    it is false, plus the open-link button that only exists under if (vm.isLink). A bare URL with no
    whitespace stays a link on both revisions and is the neighbouring legal control. None of this
    has been run, so the pair is not yet verified to discriminate
  - [incomplete-evidence] the build input reopened and the cause is measured. The lane image now
    ships Flutter 3.41.6, and at that version LocalSend's own lockfile is unsolvable: flutter_test
    from the SDK pins matcher 0.12.19 and test_api 0.7.10 while the application pins mockito 5.5.0
    and test ^1.26.2, so pub reports version solving failed. Checking out the 3.38.10 the
    application pins in .fvmrc inside the build container resolves it and pub reports Got
    dependencies. The APK build itself is long, because cargokit compiles the Rust core for
    android-x64, and had not finished when this was recorded, so no profile APK exists at either
    revision right now
  - [incomplete-evidence] only one application is designed and the second is untouched. gopeed
    remains the recorded second candidate and no revision of it has been fetched, built or run
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## Flutter iOS

- Maturity: Stable
- Scope: Flutter on a disposable iOS simulator through flutter drive
- Promotion standard: schema-3
- Native gates: flutter-ios
- Field benchmark: validation/field/flutter-ios.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: ios-simulator
- Architectures: arm64
- Runtimes: Dart VM service, flutter drive
- Frameworks: Flutter
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Linux GTK

- Maturity: Stable
- Scope: x86_64 Linux container with AT-SPI on GTK
- Promotion standard: schema-3
- Native gates: linux-atspi-gtk
- Field benchmark: validation/field/linux-gtk.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux-container
- Architectures: x86_64
- Runtimes: AT-SPI 2, GLib main loop
- Frameworks: GTK 3, GTK 4
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Linux Qt Quick/QML

- Maturity: Stable
- Scope: x86_64 Linux container with AT-SPI on Qt Quick/QML
- Promotion standard: schema-3
- Native gates: linux-atspi-toolkits
- Field benchmark: validation/field/linux-qt-quick.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux-container
- Architectures: x86_64
- Runtimes: AT-SPI 2, Qt 6, QML engine
- Frameworks: Qt Quick/QML
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Linux Qt Widgets

- Maturity: Stable
- Scope: x86_64 Linux container with AT-SPI on Qt Widgets
- Promotion standard: schema-3
- Native gates: linux-atspi-toolkits
- Field benchmark: validation/field/linux-qt-widgets.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux-container
- Architectures: x86_64
- Runtimes: AT-SPI 2, Qt 6
- Frameworks: Qt Widgets
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Linux wxWidgets

- Maturity: Stable
- Scope: x86_64 Linux container with AT-SPI on wxWidgets
- Promotion standard: schema-3
- Native gates: linux-atspi-toolkits
- Field benchmark: validation/field/linux-wxwidgets.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux-container
- Architectures: x86_64
- Runtimes: AT-SPI 2, GTK backend
- Frameworks: wxWidgets
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## macOS Accessibility

- Maturity: Preview
- Scope: Permissioned macOS accessibility on SwiftUI
- Promotion standard: schema-3
- Native gates: macos-ax
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: macos
- Architectures: arm64
- Runtimes: Swift runtime, Accessibility API
- Frameworks: SwiftUI, AppKit
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: missing
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed. 9 candidate defects across 5
    independent applications are qualified with verified revisions, but none has three clean
    affected reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## React Native Android

- Maturity: Preview
- Scope: React Native on a reset Android emulator through Appium UiAutomator2
- Promotion standard: schema-3
- Native gates: react-native-android
- Field benchmark: validation/field/react-native-android.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: android-emulator
- Architectures: x86_64
- Runtimes: Hermes, Appium UiAutomator2
- Frameworks: React Native
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] one of the two required application campaigns is complete. joplin, issue
    15004, affected de637847 and fixed 623da377: three affected reproductions all landing on react-
    native-navigation:hardware-back-exits-app-after-deleted-notebook, three fixed controls all
    returning to the note list, neighbouring legal behaviour holding on both revisions, and a
    passing cleanup audit. MissingCore/Music is discarded rather than retried: with all four
    fixtures confirmed in MediaStore before launch and the media permission granted, its library
    reports zero tracks for the full 300 second bound, which is a property of that application and
    not a gap in the harness. streetwriters/notesnook replaces it. It is qualified offline with a
    skippable signup, its release signingConfig uses the committed debug.keystore, and notesnook-
    unlink-notebook-10053 has a verified pair, affected 14f727d6 and fixed 7c3fdab6, whose diff is
    confined to one screen: in single-select mode the fix writes an explicit deselected state so a
    notebook the user removed no longer stays linked. Both revisions are bootstrapped. Its Android
    build has not completed: syspolicyd on the build host is wedged and processes Gradle spawns
    sleep in _dyld_start, so the autolinking node and the editor bundle's vite both stall, though
    the same commands run instantly from a shell. The autolink config is fed from a file to avoid
    one of them and the build is driven by a bounded retry loop because Gradle resumes, which has
    carried it into compilation
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## React Native iOS

- Maturity: Preview
- Scope: React Native on a disposable iOS simulator through Appium XCUITest
- Promotion standard: schema-3
- Native gates: react-native-ios
- Field benchmark: validation/field/react-native-ios.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: ios-simulator
- Architectures: arm64
- Runtimes: Hermes, Appium XCUITest
- Frameworks: React Native
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] one of the two required application campaigns is complete, and the corpus
    is retained and passing. joplin, issue 15972, affected 7d90db0b and fixed 2fa45a5a, ran on
    disposable iPhone 16 Pro simulators on iOS 26.2: three affected reproductions all landing on
    react-native-layout:note-row-padding-outside-touch-target with the note row hit area measured at
    370 by 20 and the tap at (201, 118) swallowed, three fixed controls all reaching the same
    observation with the hit area at 402 by 52 and the identical coordinate opening the note,
    neighbouring legal behaviour holding on both revisions, and the owned simulator deleted. The
    corpus is one clean and two adversarial subjects with zero false positives. The second
    application is what remains: BlueWallet is excluded outright because a pinned dependency
    repository returns 404, and streetwriters/notesnook is bootstrapped with its pods resolving at
    both revisions but is not built

## SwiftUI iOS

- Maturity: Stable
- Scope: SwiftUI on a disposable iOS simulator through Appium XCUITest
- Promotion standard: schema-3
- Native gates: swiftui-ios
- Field benchmark: validation/field/swiftui-ios.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: ios-simulator
- Architectures: arm64
- Runtimes: Swift runtime, Appium XCUITest
- Frameworks: SwiftUI
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Tauri Linux

- Maturity: Preview
- Scope: Tauri on Linux WebKitGTK workers
- Promotion standard: schema-3
- Native gates: tauri
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux
- Architectures: x86_64
- Runtimes: WebKitGTK, tauri-driver
- Frameworks: Tauri
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] one of the two required independent application campaigns is executed. cc-
    switch issue 4302 has three clean affected reproductions on one identity, three reached-
    observation fixed controls, a minimized trigger, and both controls, and the clean and
    adversarial corpus now measures the oracle on known-good subjects. The second application is
    missing for a measured reason, not an untried one: this probe observes only the WebKitGTK
    webview DOM, and readest and note-gen both need a native GTK window, readest to seed a library
    and note-gen to observe a file chooser. readest builds in the worker and its argv path was
    measured to open books transiently without importing them, so its library cannot be seeded
    through this channel

## Terminal UI

- Maturity: Stable
- Scope: Terminal applications on a real PTY with the VT parser
- Promotion standard: schema-3
- Native gates: tui-pty
- Field benchmark: validation/field/tui.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux
- Architectures: x86_64
- Runtimes: PTY, VT parser
- Frameworks: Go, TypeScript, Python terminal apps
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Web Chromium

- Maturity: Stable
- Scope: Chromium web through Playwright CDP on Linux
- Promotion standard: schema-3
- Native gates: web-chromium
- Field benchmark: validation/field/benchmark.json
- Production-to-local: FixtureQualified
- Production-to-local evidence: validation/field/evidence/production/web-chromium/record.json
- Operating systems: linux
- Architectures: x86_64
- Runtimes: Node.js 20+, Playwright CDP
- Frameworks: DOM applications
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Web Firefox

- Maturity: Stable
- Scope: Firefox through Playwright on Linux
- Promotion standard: schema-3
- Native gates: web-engines
- Field benchmark: validation/field/web-firefox.json
- Production-to-local: FixtureQualified
- Production-to-local evidence: validation/field/evidence/production/web-firefox/record.json
- Operating systems: linux
- Architectures: x86_64
- Runtimes: Node.js 20+, Playwright
- Frameworks: DOM applications
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Web WebKit

- Maturity: Stable
- Scope: WebKit through Playwright on Linux
- Promotion standard: schema-3
- Native gates: web-engines
- Field benchmark: validation/field/web-webkit.json
- Production-to-local: FixtureQualified
- Production-to-local evidence: validation/field/evidence/production/web-webkit/record.json
- Operating systems: linux
- Architectures: x86_64
- Runtimes: Node.js 20+, Playwright
- Frameworks: DOM applications
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Windows Avalonia

- Maturity: Stable
- Scope: Native x86_64 Windows UI Automation on Avalonia
- Promotion standard: schema-3
- Native gates: windows-uia
- Field benchmark: validation/field/windows-avalonia.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: windows-x86_64-interactive
- Architectures: x86_64
- Runtimes: .NET, UI Automation
- Frameworks: Avalonia
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Windows WinUI 3

- Maturity: Stable
- Scope: Native x86_64 Windows UI Automation on WinUI 3
- Promotion standard: schema-3
- Native gates: windows-uia
- Field benchmark: validation/field/windows-winui.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: windows-x86_64-interactive
- Architectures: x86_64
- Runtimes: .NET, UI Automation, WinAppSDK
- Frameworks: WinUI 3
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

## Windows WPF

- Maturity: Stable
- Scope: Native x86_64 Windows UI Automation on WPF
- Promotion standard: schema-3
- Native gates: windows-uia
- Field benchmark: validation/field/windows-wpf.json
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: windows-x86_64-interactive
- Architectures: x86_64
- Runtimes: .NET, UI Automation
- Frameworks: WPF
- Qualifications:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - None

A Stable target requires exact-commit native evidence, two independent
affected-versus-fixed applications, three clean affected reproductions,
three reached-observation fixed controls, exact identity preservation,
verified minimization, and neighboring legal behavior. Targets on the
schema-3 standard additionally require a clean corpus, an adversarial
corpus, a clean package installation, and a confirmed manual review.
