# Backend runtime validation

These are operability gates, not compile checks. A backend passes only when its production runner
launches or attaches to a native fixture, reads a non-empty UI tree, performs a real action,
observes a different structural state, emits an `EXPLORE:EDGE`, and finishes with `JOURNEY DONE`
plus `All tests passed`.

`evidence.json` maps every registered `app.platform` id to a bounded gate command, fixture, target
OS and architecture, reset and cleanup strategy, execution tier, and automation owner. The
platform-registry unit test rejects missing commands, workflows, jobs, result schemas, and
unrepresented platform ids. Sharing a backend does not by itself count as native toolkit evidence:
React Native, Compose, SwiftUI, Tauri, Electron, Avalonia, and the other named stacks each have
their own fixture.

Run one gate through the evidence recorder:

```sh
python3 validation/backends/gate.py web-chromium
```

The recorder applies the gate's timeout, bounds captured output to 16 MiB, checks required runtime
markers, and writes a log plus a `result.schema.json`-compatible result under
`target/reproit-validation/`. Set `REPROIT_GATE_OUTPUT_DIR` to place CI artifacts elsewhere. The
weekly and manually dispatched matrix lives in `.github/workflows/native-gates.yml`. Windows UIA
remains explicitly manual because it requires the private interactive VM SSH chain; its blocker is
recorded in the manifest instead of being presented as hosted CI coverage.

| Backend         | Native runtime evidence                      | Command                                             |
| --------------- | -------------------------------------------- | --------------------------------------------------- |
| `flutter-drive` | Flutter app on an iOS simulator              | `validation/backends/run-flutter-drive.sh`          |
| `web-cdp`       | Chromium DOM                                 | `validation/backends/run-web-cdp.sh`                |
| `web-cdp`       | Firefox and WebKit DOM through Playwright    | `validation/backends/run-web-engines.sh`            |
| `web-cdp`       | Electron/Chromium                            | `validation/backends/run-electron.sh`               |
| `web-cdp`       | Tauri v2/WebKitGTK through `tauri-driver`    | `validation/backends/run-tauri.sh`                  |
| `appium`        | React Native Android release app             | `validation/backends/run-react-native-android.sh`   |
| `appium`        | Jetpack Compose Android app                  | `examples/compose-fixture/compose-appium-smoke.sh`  |
| `appium`        | SwiftUI iOS app                              | `.github/scripts/appium-ios-swiftui-smoke.sh`       |
| `desktop-ax`    | SwiftUI macOS app                            | `validation/backends/run-macos-ax.sh`               |
| `desktop-uia`   | WPF, Avalonia, and WinUI 3 apps              | `validation/backends/run-windows-desktop-remote.sh` |
| `desktop-atspi` | GTK multi-actor app                          | `.github/scripts/atspi-scenario-e2e.sh`             |
| `desktop-atspi` | Qt Widgets, Qt Quick/QML, and wxWidgets apps | `examples/qt-fixture/qt-atspi-e2e.sh`               |
| `instrumented`  | Dear ImGui and Clay native fixtures          | `validation/backends/run-instrumented.sh`           |
| `tui-pty`       | Real curses app in a PTY                     | `validation/backends/run-tui.sh`                    |

The Appium commands require a running server with XCUITest or UiAutomator2 as appropriate.
`run-react-native-android.sh` accepts `REPROIT_ANDROID_UDID`; it pins React Native 0.76.9 and builds
a bundled release APK so Metro is not part of the result. The Windows command requires an OpenSSH
host alias supplied through `REPROIT_WINDOWS_HOST` and runs the GUI gate as an interactive scheduled
task. A noninteractive service session is not valid UI Automation evidence.

Linux desktop and Tauri gates build inside pinned containers. macOS, iOS, Flutter, Android, and
Windows gates use their native host tools. No gate treats a mocked marker stream as backend
operability evidence.
