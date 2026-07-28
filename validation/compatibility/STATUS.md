# Compatibility qualification status

Generated from `validation/support-manifest.json`. Do not edit by hand.

## Backend contracts

- Maturity: Preview
- Scope: Bounded backend scan, fuzz, replay, proof, and runtime capture
- Native gates: backend-contract
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete
  - production-to-local occurrence evidence corpus is incomplete

## Jetpack Compose Android

- Maturity: Preview
- Scope: Jetpack Compose on a reset Android emulator through Appium UiAutomator2
- Native gates: compose-android
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Electron Linux

- Maturity: Preview
- Scope: Packaged Electron applications on Linux workers
- Native gates: electron
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Flutter Android

- Maturity: Preview
- Scope: Flutter on a reset Android emulator
- Native gates: flutter-android
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Flutter iOS

- Maturity: Preview
- Scope: Flutter on a disposable iOS simulator through flutter drive
- Native gates: flutter-ios
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Linux GTK

- Maturity: Preview
- Scope: x86_64 Linux container with AT-SPI on GTK
- Native gates: linux-atspi-gtk
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Linux Qt Quick/QML

- Maturity: Preview
- Scope: x86_64 Linux container with AT-SPI on Qt Quick/QML
- Native gates: linux-atspi-toolkits
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Linux Qt Widgets

- Maturity: Preview
- Scope: x86_64 Linux container with AT-SPI on Qt Widgets
- Native gates: linux-atspi-toolkits
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Linux wxWidgets

- Maturity: Preview
- Scope: x86_64 Linux container with AT-SPI on wxWidgets
- Native gates: linux-atspi-toolkits
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## macOS Accessibility

- Maturity: Preview
- Scope: Permissioned macOS accessibility on SwiftUI
- Native gates: macos-ax
- Field benchmark: incomplete
- Promotion blockers:
  - native gate still requires an explicitly permissioned runner
  - independent affected-versus-fixed field benchmark is incomplete

## React Native Android

- Maturity: Preview
- Scope: React Native on a reset Android emulator through Appium UiAutomator2
- Native gates: react-native-android
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## React Native iOS

- Maturity: Preview
- Scope: React Native on a disposable iOS simulator through Appium XCUITest
- Native gates: react-native-ios
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## SwiftUI iOS

- Maturity: Preview
- Scope: SwiftUI on a disposable iOS simulator through Appium XCUITest
- Native gates: swiftui-ios
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Tauri Linux

- Maturity: Preview
- Scope: Tauri on Linux WebKitGTK workers
- Native gates: tauri
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Terminal UI

- Maturity: Preview
- Scope: Terminal applications on a real PTY with the VT parser
- Native gates: tui-pty
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Web Chromium

- Maturity: Stable
- Scope: Chromium web through Playwright CDP on Linux
- Native gates: web-chromium
- Field benchmark: validation/field/benchmark.json
- Promotion blockers:
  - None

## Web Firefox

- Maturity: Preview
- Scope: Firefox through Playwright on Linux
- Native gates: web-engines
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Web WebKit

- Maturity: Preview
- Scope: WebKit through Playwright on Linux
- Native gates: web-engines
- Field benchmark: incomplete
- Promotion blockers:
  - independent affected-versus-fixed field benchmark is incomplete

## Windows Avalonia

- Maturity: Preview
- Scope: Native x86_64 Windows UI Automation on Avalonia
- Native gates: windows-uia
- Field benchmark: incomplete
- Promotion blockers:
  - native gate still requires the interactive Windows VM
  - independent affected-versus-fixed field benchmark is incomplete

## Windows WinUI 3

- Maturity: Preview
- Scope: Native x86_64 Windows UI Automation on WinUI 3
- Native gates: windows-uia
- Field benchmark: incomplete
- Promotion blockers:
  - native gate still requires the interactive Windows VM
  - independent affected-versus-fixed field benchmark is incomplete

## Windows WPF

- Maturity: Preview
- Scope: Native x86_64 Windows UI Automation on WPF
- Native gates: windows-uia
- Field benchmark: incomplete
- Promotion blockers:
  - native gate still requires the interactive Windows VM
  - independent affected-versus-fixed field benchmark is incomplete

A Stable target requires exact-commit native evidence, two independent
affected-versus-fixed applications, three clean affected reproductions,
three reached-observation fixed controls, exact identity preservation,
verified minimization, and neighboring legal behavior.
