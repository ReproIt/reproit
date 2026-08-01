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

- Maturity: Preview
- Scope: x86_64 Linux container with AT-SPI on Qt Quick/QML
- Promotion standard: schema-3
- Native gates: linux-atspi-toolkits
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux-container
- Architectures: x86_64
- Runtimes: AT-SPI 2, Qt 6, QML engine
- Frameworks: Qt Quick/QML
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed, and the two candidates that
    build were both executed against their triggers and did not separate their revisions. kalk
    940abaf versus b452c5a: under de_DE.UTF-8 the affected build already evaluates the comma decimal
    '1,5+1' to '2,5' read through the AT-SPI text interface, because libqalculate parses the
    separator itself, so the normalisation the fix adds changes nothing observable. kclock ac9abd2
    versus 033e713: with a preset seeded into kclockrc the preset row is showing on both revisions,
    so the isMobile gate the fix removes is not gating anything on this worker. Both are discarded
    rather than forced
  - [incomplete-evidence] a third Qt Quick application has to be mined, because the qualified pool
    is now exhausted. marknote needs KF 6.21 against trixie's 6.13; keysmith 477812 is severity
    wishlist rather than a defect; kalk and kclock are the two discarded above. elisa 18e843d versus
    5b191f1 is the only qualified candidate left and its build was started but not completed: it
    wants KF6Codecs and a further chain a music player pulls in, which is materially larger than the
    two calculators. Its trigger is otherwise sound on paper, since the fix removes a key event
    filter and the observable is whether Space toggles a focused player-control button
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

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
    15004, affected de637847 and fixed 623da377, ran on a pixel_6 AVD on android-36 google_apis
    x86_64 recreated per run and booted with -wipe-data -no-snapshot, inside the pinned worker image
    with Docker network mode none: three affected reproductions all landing on react-native-
    navigation:hardware-back-exits-app-after-deleted-notebook, three fixed controls all reaching the
    same observation and returning to the note list, neighbouring legal behaviour holding on both
    revisions, and a passing cleanup audit. The second application is not complete. The
    MissingCore/Music campaign seeds correctly now, with MediaStore confirmed to hold all four
    fixtures before the application launches, and it stops on the application: the media permission
    is granted and the library still reports zero tracks for the full 300 second bound. What makes
    this application ingest an already-populated volume, whether its onboarding scan runs before the
    grant lands and is never retried or whether it needs a rescan driven from settings, is the exact
    remaining input
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## React Native iOS

- Maturity: Preview
- Scope: React Native on a disposable iOS simulator through Appium XCUITest
- Promotion standard: schema-3
- Native gates: react-native-ios
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: ios-simulator
- Architectures: arm64
- Runtimes: Hermes, Appium XCUITest
- Frameworks: React Native
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] the build blocker is closed and the observable is measured, but no
    campaign has been run. Both revisions of joplin-note-row-touch-target-15972 build, install and
    launch on a disposable simulator. XCUITest reports the note row as an XCUIElementTypeButton
    whose frame is the pressable the fix changes: affected x=16 y=127 w=370 h=20, fixed x=0 y=111
    w=402 h=52, both centred at (201, 137). One tap at (201, 118) is swallowed on the affected build
    and opens the note on the fixed one, executed on both. A campaign driver is committed on that
    observable and has never obtained a WebDriverAgent session: four device and server
    configurations were executed and each reaches 'Launching WebDriverAgent on the device' and then
    waits on connect ECONNREFUSED, while the standalone probes that measured the observable obtain
    sessions reliably against the same Appium, driver, derived data and bundles. Stale xcodebuild
    runners were one real cause and are reaped now, but were not sufficient. Isolating the
    difference between the probe and the driver by bisection is the first missing input; a second
    independent application, since BlueWallet is excluded outright, is the second
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

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
