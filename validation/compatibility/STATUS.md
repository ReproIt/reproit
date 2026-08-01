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
  no Dart VM service, which was measured rather than assumed, so it carries no observation channel
  and is out of scope
- Promotion standard: schema-3
- Native gates: flutter-android
- Field benchmark: incomplete
- Production-to-local: Unqualified
- Production-to-local evidence: none
- Operating systems: android-emulator
- Architectures: x86_64
- Runtimes: Dart VM service (profile-mode build), flutter drive
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
  - [environment-unreachable] the arch bound is x86_64 because the flutter-android native gate
    declares x86_64, but every Android system image installed on the campaign host is arm64-v8a, so
    an application campaign run here cannot satisfy the declared architecture. Either install an
    x86_64 API 36 image and campaign on it, or run the campaign on the same x86_64 executor the gate
    uses

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
  - [incomplete-evidence] no application campaign has been executed. 5 candidate defects across 4
    application(s) are qualified with verified revisions, but none has three clean affected
    reproductions and three reached-observation fixed controls
  - [incomplete-evidence] only one of the five qualified candidates is admissible to the benchmark
    record. validation/field/check-benchmark.py accepts a GitHub HTTPS repository and a GitHub issue
    URL and nothing else, and four of the five candidates live on gitlab.gnome.org. rnote is the
    single GitHub-hosted subject, so a second GitHub-hosted GTK 4 application with a verified
    affected and fixed pair must be mined, or the benchmark and corpus validators must be extended
    to accept a named set of additional forges
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
  - [incomplete-evidence] none of the five qualified candidates is admissible to the benchmark
    record. validation/field/check-benchmark.py accepts a GitHub HTTPS repository and a GitHub issue
    URL and nothing else, and all five candidates live on invent.kde.org with bugs.kde.org issue
    ids. Two GitHub-hosted Qt Quick applications with verified affected and fixed pairs must be
    mined, or the benchmark and corpus validators must be extended to accept a named set of
    additional forges
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
  - [incomplete-evidence] one of the two required applications now has a worker image and a proven
    trigger and the other has neither. wxMaxima compiles at e5c410e884c3f1b24c54ac1179c16c3cf0283247
    and 684d2ed4e106fc0fc174f5537129bfd13ba68b93 in an x86_64 bookworm container, and the retained
    AT-SPI records show the Greek Letters view check menu item reporting checked on the affected
    revision and unchecked on the fixed one from a clean profile. What is missing is a second
    independent application: validation/field/linux-wxwidgets/ carries only the wxMaxima subject,
    and no benchmark record exists because a complete benchmark needs exactly two
  - [incomplete-evidence] the second application is chosen and its build is proven, but its probe is
    unwritten. poedit c4fe890ef72c8c8cbc6b9b8cc1784dba10447798 configures and links a real binary on
    Debian trixie with libwxgtk3.2-dev plus libwxgtk-webview3.2-dev and --without-cpprest --without-
    cld2; bookworm cannot host it because configure demands wxWidgets 3.2.4. Its observation channel
    is also settled by reading src/fileviewer.cpp at that revision: FileViewer::ShowError paints a
    wxStaticText reading 'Source code not found' when GetFilename resolves nothing, which is exactly
    what the empty BeforeLast(':') produces for a reference carrying no line number. What remains is
    a two-distribution worker image, a .po fixture whose entry references a source file without a
    line number, and three affected plus three fixed runs
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
    application(s) are qualified with verified revisions. The campaign module and its container
    entry point existed with no committed caller at all; the bounded remote field runner is now
    committed and drives the campaign over the same zgx gateway to strix route the Android release
    gates use, because every locally installed Android system image is arm64-v8a and this target's
    bound is android-emulator/x86_64. What remains is four application archives, an affected and a
    fixed APK for joplin and the same pair for MissingCore/Music; none was built in this session, so
    the runner is committed unexercised
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
  - [incomplete-evidence] no application campaign has been executed, and the qualified pool is now
    down to one buildable application. All five BlueWallet candidates are disqualified outright:
    package.json pins rn-qr-generator to an exact commit of https://github.com/BlueWallet/rn-qr-
    generator, which returns 404 upstream, so no working tree exists at any candidate revision
    without substituting a pinned dependency. Joplin installs, resolves its pods and compiles every
    native and application source file, but the Bundle React Native code and images phase fails
    because Metro addresses packages/app-mobile/index.js through /tmp while its haste map holds the
    resolved /private/tmp path; a build root outside that symlink is the only remaining input for an
    archive. Even with Joplin building, a second independent application is required and none is
    qualified
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
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: ci-gate
  - manualReview: field-benchmark
- Promotion blockers:
  - [incomplete-evidence] one of the two required independent application campaigns is executed. cc-
    switch issue 4302 has three clean affected reproductions on one identity, three reached-
    observation fixed controls, a minimized trigger, and both controls. The second application is
    missing: readest needs a pnpm 11 workspace build with setup-vendors and a dotenv-driven Next.js
    export before its Tauri artifact resolves offline, and note-gen cannot be used with this harness
    because its selected defect observes a GTK file chooser rather than the webview DOM
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
