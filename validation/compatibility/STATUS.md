# Compatibility qualification status

Generated from `validation/support-manifest.json`. Do not edit by hand.

## Backend contracts

- Maturity: Preview
- Scope: Bounded backend scan, fuzz, replay, proof, and runtime capture
- Promotion standard: schema-3
- Native gates: backend-contract
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux
- Architectures: x86_64
- Runtimes: HTTP, OpenAPI
- Frameworks: Backend services
- Qualifications:
  - cleanCorpus: ci-gate
  - adversarialCorpus: ci-gate
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed. 6 candidate defects across 4
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## Jetpack Compose Android

- Maturity: Preview
- Scope: Jetpack Compose on a reset Android emulator through Appium UiAutomator2
- Promotion standard: schema-3
- Native gates: compose-android
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: android-emulator
- Architectures: x86_64
- Runtimes: ART, Appium UiAutomator2
- Frameworks: Jetpack Compose
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 2
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

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
- Scope: Flutter on a reset Android emulator
- Promotion standard: schema-3
- Native gates: flutter-android
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: android-emulator
- Architectures: x86_64
- Runtimes: Dart VM service, flutter drive
- Frameworks: Flutter
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 2
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target
  - [unsupported-capability] a Flutter release APK is AOT compiled with the Dart VM service removed,
    so the declared runtime bound is not reachable from the release artifact. The campaign must
    either observe through a profile-mode build or the bound must be restated

## Flutter iOS

- Maturity: Preview
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
  - [incomplete-evidence] the flutter-ios native gate passed the current candidate working tree, but
    the dirty tree has no exact-commit gate result and the host Xcode 26.2 does not match the pinned
    Xcode 16.4 preflight

## Linux GTK

- Maturity: Preview
- Scope: x86_64 Linux container with AT-SPI on GTK
- Promotion standard: schema-3
- Native gates: linux-atspi-gtk
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux-container
- Architectures: x86_64
- Runtimes: AT-SPI 2, GLib main loop
- Frameworks: GTK 3, GTK 4
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [environment-unreachable] the Linux GTK gate builds on the local x86_64 worker, but both owned
    fixture processes remain absent from the AT-SPI application bus before the first action
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 4
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

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
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 5
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## Linux Qt Widgets

- Maturity: Preview
- Scope: x86_64 Linux container with AT-SPI on Qt Widgets
- Promotion standard: schema-3
- Native gates: linux-atspi-toolkits
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux-container
- Architectures: x86_64
- Runtimes: AT-SPI 2, Qt 6
- Frameworks: Qt Widgets
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 3
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## Linux wxWidgets

- Maturity: Preview
- Scope: x86_64 Linux container with AT-SPI on wxWidgets
- Promotion standard: schema-3
- Native gates: linux-atspi-toolkits
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: linux-container
- Architectures: x86_64
- Runtimes: AT-SPI 2, GTK backend
- Frameworks: wxWidgets
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 5
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

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
  - [incomplete-evidence] no exact-commit evidence is recorded for the macos-ax native gate. The
    execution infrastructure is proven reachable on this host (macos-ax); the gate has simply not
    been run and retained against the candidate commit
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
- Field benchmark: incomplete
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
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 3
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
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
  - [incomplete-evidence] no application campaign has been executed. 9 candidate defects across 2
    independent applications (BlueWallet, Joplin) are qualified with verified revisions, but neither
    has three clean affected reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## SwiftUI iOS

- Maturity: Preview
- Scope: SwiftUI on a disposable iOS simulator through Appium XCUITest
- Promotion standard: schema-3
- Native gates: swiftui-ios
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: ios-simulator
- Architectures: arm64
- Runtimes: Swift runtime, Appium XCUITest
- Frameworks: SwiftUI
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: missing
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed. 10 candidate defects across 6
    independent applications are qualified with verified revisions, but none has three clean
    affected reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

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
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 3
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

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
- Production-to-local: Unqualified
- Production-to-local evidence: none
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
- Production-to-local: Unqualified
- Production-to-local evidence: none
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
- Production-to-local: Unqualified
- Production-to-local evidence: none
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

- Maturity: Preview
- Scope: Native x86_64 Windows UI Automation on Avalonia
- Promotion standard: schema-3
- Native gates: windows-uia
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: windows-x86_64-interactive
- Architectures: x86_64
- Runtimes: .NET, UI Automation
- Frameworks: Avalonia
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no exact-commit evidence is recorded for the windows-uia native gate. The
    execution infrastructure is proven reachable on this host (windows-vm); the gate has simply not
    been run and retained against the candidate commit
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 2
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## Windows WinUI 3

- Maturity: Preview
- Scope: Native x86_64 Windows UI Automation on WinUI 3
- Promotion standard: schema-3
- Native gates: windows-uia
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: windows-x86_64-interactive
- Architectures: x86_64
- Runtimes: .NET, UI Automation, WinAppSDK
- Frameworks: WinUI 3
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no exact-commit evidence is recorded for the windows-uia native gate. The
    execution infrastructure is proven reachable on this host (windows-vm); the gate has simply not
    been run and retained against the candidate commit
  - [incomplete-evidence] no application campaign has been executed. 8 candidate defects across 2
    independent applications (DLSS Swapper, UniGetUI) are qualified with verified revisions, but
    neither has three clean affected reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

## Windows WPF

- Maturity: Preview
- Scope: Native x86_64 Windows UI Automation on WPF
- Promotion standard: schema-3
- Native gates: windows-uia
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: windows-x86_64-interactive
- Architectures: x86_64
- Runtimes: .NET, UI Automation
- Frameworks: WPF
- Qualifications:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] no exact-commit evidence is recorded for the windows-uia native gate. The
    execution infrastructure is proven reachable on this host (windows-vm); the gate has simply not
    been run and retained against the candidate commit
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 2
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] no per-target clean and adversarial corpus gate exists, so no false-
    positive rate is measured for this target

A Stable target requires exact-commit native evidence, two independent
affected-versus-fixed applications, three clean affected reproductions,
three reached-observation fixed controls, exact identity preservation,
verified minimization, and neighboring legal behavior. Targets on the
schema-3 standard additionally require a clean corpus, an adversarial
corpus, a clean package installation, and a confirmed manual review.
