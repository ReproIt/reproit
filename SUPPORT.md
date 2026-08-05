# Support

## Community support

Use GitHub Issues for reproducible product defects and GitHub Discussions for
usage questions. Include `reproit --version`, the target platform, the relevant
configuration with secrets removed, and the smallest reproducer.

Security reports follow [SECURITY.md](SECURITY.md), never a public issue.

## Compatibility

The supported platform targets, their gates, and their bounds are listed in
[docs/compatibility.md](docs/compatibility.md). A platform outside that matrix
may work, but it is not a release commitment until native evidence is part of
the release gate.

The claim below is generated from `validation/support-manifest.json`. Editing
this section by hand cannot add a target.

<!-- generated:support-claim -->

Reproit supports 21 atomic platform targets:
- Backend contracts
- Jetpack Compose Android
- Electron
- Flutter Android
- Flutter iOS
- Linux GTK
- Linux Qt Quick/QML
- Linux Qt Widgets
- Linux wxWidgets
- macOS Accessibility
- React Native Android
- React Native iOS
- SwiftUI iOS
- Tauri
- Terminal UI
- Web Chromium
- Web Firefox
- Web WebKit
- Windows Avalonia
- Windows WinUI 3
- Windows WPF

Every declared target has gates for its native fixtures.
The 1.0 support claim covers every declared target.
The generated status records each target's retained evidence.

<!-- /generated:support-claim -->
