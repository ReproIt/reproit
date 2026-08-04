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

Reproit has 19 qualified atomic platform targets:
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
- SwiftUI iOS
- Terminal UI
- Web Chromium
- Web Firefox
- Web WebKit
- Windows Avalonia
- Windows WinUI 3
- Windows WPF

Preview targets with incomplete independent evidence:
- React Native iOS
- Tauri

Every declared target has gates for its native fixtures.
Qualified targets have complete independent behavior evidence.
Only qualified targets are part of the 1.0 support claim.
Preview targets keep their 1.x configuration and wire compatibility.
The generated status shows each preview evidence gap.

<!-- /generated:support-claim -->
