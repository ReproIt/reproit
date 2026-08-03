# Supported platform targets

Generated from `validation/support-manifest.json`. Do not edit by hand.

Reproit supports 21 atomic targets.

## Backend contracts

- Target id: `backend-contract`
- Family: backend
- Scope: Bounded backend scan, fuzz, replay, proof, and runtime capture
- Native gates: backend-contract
- Release evidence directories: backend-contract in linux-hosted
- Field benchmark: validation/field/backend-contract.json
- Platforms: Linux, macOS, Windows
- Runtimes: HTTP, OpenAPI
- Frameworks: Backend services
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Jetpack Compose Android

- Target id: `compose-android`
- Family: native-mobile
- Scope: Jetpack Compose on a reset Android emulator through Appium UiAutomator2
- Native gates: compose-android
- Release evidence directories: compose-android in android
- Field benchmark: validation/field/compose-android.json
- Platforms: Android
- Runtimes: ART, Appium UiAutomator2
- Frameworks: Jetpack Compose
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Electron

- Target id: `electron-linux`
- Family: desktop-webview
- Scope: Packaged Electron applications on Linux workers
- Native gates: electron
- Release evidence directories: electron in linux-hosted
- Field benchmark: validation/field/electron-linux.json
- Platforms: Linux, macOS, Windows
- Runtimes: Chromium, Node.js, CDP
- Frameworks: Electron
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Flutter Android

- Target id: `flutter-android`
- Family: flutter
- Scope: Flutter profile-mode builds on a reset Android emulator, driven through Appium
  UiAutomator2. A release APK is AOT compiled with no Dart VM service; a profile APK exposes the
  service, but its tree dumps carry nothing and evaluate is refused, so the observable is read from
  the platform accessibility hierarchy that attaching a UiAutomation makes Flutter generate. Every
  one of those facts was measured rather than assumed
- Native gates: flutter-android
- Release evidence directories: flutter-android in android
- Field benchmark: validation/field/flutter-android.json
- Platforms: Android
- Runtimes: Dart VM service (profile-mode build, liveness and isolate only), Appium UiAutomator2
- Frameworks: Flutter
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Flutter iOS

- Target id: `flutter-ios`
- Family: flutter
- Scope: Flutter on a disposable iOS simulator through flutter drive
- Native gates: flutter-ios
- Release evidence directories: flutter-ios in flutter
- Field benchmark: validation/field/flutter-ios.json
- Platforms: iOS
- Runtimes: Dart VM service, flutter drive
- Frameworks: Flutter
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Linux GTK

- Target id: `linux-gtk`
- Family: desktop
- Scope: x86_64 Linux container with AT-SPI on GTK
- Native gates: linux-atspi-gtk
- Release evidence directories: linux-atspi-gtk in linux-containers
- Field benchmark: validation/field/linux-gtk.json
- Platforms: Linux
- Runtimes: AT-SPI 2, GLib main loop
- Frameworks: GTK 3, GTK 4
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Linux Qt Quick/QML

- Target id: `linux-qt-quick`
- Family: desktop
- Scope: x86_64 Linux container with AT-SPI on Qt Quick/QML
- Native gates: linux-atspi-toolkits
- Release evidence directories: linux-atspi-toolkits in linux-containers
- Field benchmark: validation/field/linux-qt-quick.json
- Platforms: Linux
- Runtimes: AT-SPI 2, Qt 6, QML engine
- Frameworks: Qt Quick/QML
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Linux Qt Widgets

- Target id: `linux-qt-widgets`
- Family: desktop
- Scope: x86_64 Linux container with AT-SPI on Qt Widgets
- Native gates: linux-atspi-toolkits
- Release evidence directories: linux-atspi-toolkits in linux-containers
- Field benchmark: validation/field/linux-qt-widgets.json
- Platforms: Linux
- Runtimes: AT-SPI 2, Qt 6
- Frameworks: Qt Widgets
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Linux wxWidgets

- Target id: `linux-wxwidgets`
- Family: desktop
- Scope: x86_64 Linux container with AT-SPI on wxWidgets
- Native gates: linux-atspi-toolkits
- Release evidence directories: linux-atspi-toolkits in linux-containers
- Field benchmark: validation/field/linux-wxwidgets.json
- Platforms: Linux
- Runtimes: AT-SPI 2, GTK backend
- Frameworks: wxWidgets
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## macOS Accessibility

- Target id: `macos-ax`
- Family: desktop
- Scope: Permissioned macOS accessibility on SwiftUI
- Native gates: macos-ax
- Release evidence directories: macos-ax in macos
- Field benchmark: none recorded
- Platforms: macOS
- Runtimes: Swift runtime, Accessibility API
- Frameworks: SwiftUI, AppKit
- Evidence slots:
  - cleanCorpus: missing
  - adversarialCorpus: missing
  - packageInstall: missing
  - manualReview: field-benchmark

## React Native Android

- Target id: `react-native-android`
- Family: native-mobile
- Scope: React Native on a reset Android emulator through Appium UiAutomator2
- Native gates: react-native-android
- Release evidence directories: react-native-android in android
- Field benchmark: validation/field/react-native-android.json
- Platforms: Android
- Runtimes: Hermes, Appium UiAutomator2
- Frameworks: React Native
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## React Native iOS

- Target id: `react-native-ios`
- Family: native-mobile
- Scope: React Native on a disposable iOS simulator through Appium XCUITest
- Native gates: react-native-ios
- Release evidence directories: react-native-ios in swiftui
- Field benchmark: validation/field/react-native-ios.json
- Platforms: iOS
- Runtimes: Hermes, Appium XCUITest
- Frameworks: React Native
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## SwiftUI iOS

- Target id: `swiftui-ios`
- Family: native-mobile
- Scope: SwiftUI on a disposable iOS simulator through Appium XCUITest
- Native gates: swiftui-ios
- Release evidence directories: swiftui-ios in swiftui
- Field benchmark: validation/field/swiftui-ios.json
- Platforms: iOS
- Runtimes: Swift runtime, Appium XCUITest
- Frameworks: SwiftUI
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Tauri

- Target id: `tauri-linux`
- Family: desktop-webview
- Scope: Tauri on Linux WebKitGTK workers
- Native gates: tauri
- Release evidence directories: tauri in linux-containers
- Field benchmark: none recorded
- Platforms: Linux, Windows
- Runtimes: WebKitGTK, tauri-driver
- Frameworks: Tauri
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Terminal UI

- Target id: `tui`
- Family: tui
- Scope: Terminal applications on a real PTY with the VT parser
- Native gates: tui-pty
- Release evidence directories: tui-pty in linux-hosted
- Field benchmark: validation/field/tui.json
- Platforms: Linux, macOS
- Runtimes: PTY, VT parser
- Frameworks: Terminal applications
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Web Chromium

- Target id: `web-chromium`
- Family: web
- Scope: Chromium web through Playwright CDP on Linux
- Native gates: web-chromium
- Release evidence directories: web-chromium in linux-hosted
- Field benchmark: validation/field/benchmark.json
- Platforms: Linux, macOS, Windows
- Runtimes: Node.js 20+, Playwright CDP
- Frameworks: DOM applications
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Web Firefox

- Target id: `web-firefox`
- Family: web
- Scope: Firefox through Playwright on Linux
- Native gates: web-engines
- Release evidence directories: web-engines in linux-hosted
- Field benchmark: validation/field/web-firefox.json
- Platforms: Linux, macOS, Windows
- Runtimes: Node.js 20+, Playwright
- Frameworks: DOM applications
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Web WebKit

- Target id: `web-webkit`
- Family: web
- Scope: WebKit through Playwright on Linux
- Native gates: web-engines
- Release evidence directories: web-engines in linux-hosted
- Field benchmark: validation/field/web-webkit.json
- Platforms: Linux, macOS, Windows
- Runtimes: Node.js 20+, Playwright
- Frameworks: DOM applications
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Windows Avalonia

- Target id: `windows-avalonia`
- Family: desktop
- Scope: Native x86_64 Windows UI Automation on Avalonia
- Native gates: windows-uia
- Release evidence directories: windows-uia in windows
- Field benchmark: validation/field/windows-avalonia.json
- Platforms: Windows
- Runtimes: .NET, UI Automation
- Frameworks: Avalonia
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Windows WinUI 3

- Target id: `windows-winui`
- Family: desktop
- Scope: Native x86_64 Windows UI Automation on WinUI 3
- Native gates: windows-uia
- Release evidence directories: windows-uia in windows
- Field benchmark: validation/field/windows-winui.json
- Platforms: Windows
- Runtimes: .NET, UI Automation, WinAppSDK
- Frameworks: WinUI 3
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark

## Windows WPF

- Target id: `windows-wpf`
- Family: desktop
- Scope: Native x86_64 Windows UI Automation on WPF
- Native gates: windows-uia
- Release evidence directories: windows-uia in windows
- Field benchmark: validation/field/windows-wpf.json
- Platforms: Windows
- Runtimes: .NET, UI Automation
- Frameworks: WPF
- Evidence slots:
  - cleanCorpus: evidence
  - adversarialCorpus: evidence
  - packageInstall: ci-gate
  - manualReview: field-benchmark
