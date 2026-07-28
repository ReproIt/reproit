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
remains explicitly manual because it requires a native interactive runner; its blocker is recorded
in the manifest instead of being presented as hosted CI coverage.

The manifest records the architecture used by the scheduled gate. When a local target differs,
pass `--architecture` so the evidence records what was actually exercised. Repeat the option when
a single gate covers multiple architectures.

```sh
python3 validation/backends/gate.py compose-android --architecture arm64
```

Registered runtime gates:

- `flutter-drive`: Flutter app on an iOS simulator.
- `web-cdp`: Chromium, Firefox, WebKit, Electron, and Tauri fixtures.
- `appium`: React Native Android, Compose Android, and SwiftUI iOS fixtures.
- `desktop-ax`: SwiftUI macOS fixture.
- `desktop-uia`: WPF, Avalonia, and WinUI 3 fixtures.
- `desktop-atspi`: GTK, Qt Widgets, Qt Quick/QML, and wxWidgets fixtures.
- `tui-pty`: real curses app in a PTY.
- `backend-contract`: current-server scan, fuzz, replay, and proof.

The exact command for every entry is canonical in `evidence.json`; documentation
and release validation do not maintain a second command registry.

The Appium commands require a running server with XCUITest or UiAutomator2 as appropriate.
`run-react-native-android.sh` accepts `REPROIT_ANDROID_UDID`; it pins React Native 0.76.9 and builds
a bundled release APK so Metro is not part of the result. Run the Windows command directly in a
native interactive Windows session. A noninteractive service session is not valid UI Automation
evidence.

Linux desktop and Tauri gates build inside pinned containers. macOS, iOS,
Flutter, Android, and Windows gates use their native host tools. The backend
contract gate launches an owned loopback service and exercises the production
CLI against it. No gate treats a mocked marker stream as backend operability
evidence.
