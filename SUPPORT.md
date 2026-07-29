# Support

## Community support

Use GitHub Issues for reproducible product defects and GitHub Discussions for
usage questions. Include `reproit --version`, the target platform, the relevant
configuration with secrets removed, and the smallest reproducer.

Security reports follow [SECURITY.md](SECURITY.md), never a public issue.

## Compatibility

The supported platform and evidence tiers are listed in
[docs/compatibility.md](docs/compatibility.md). A platform outside that matrix
may work, but it is not a 1.0 release commitment until native evidence is part
of the release gate.

The claim below is generated from `validation/support-manifest.json`. Editing
this section by hand cannot promote a target.

<!-- generated:support-claim -->

Stable (8): Jetpack Compose Android, Electron Linux, Flutter iOS, Terminal UI, Web Chromium, Web Firefox, Web WebKit, Windows WPF.

Preview (13): Backend contracts, Flutter Android, Linux GTK, Linux Qt Quick/QML, Linux Qt Widgets, Linux wxWidgets, macOS Accessibility, React Native Android, React Native iOS, SwiftUI iOS, Tauri Linux, Windows Avalonia, Windows WinUI 3.

Production-to-local qualified: Web Chromium, Web Firefox, Web WebKit.

Stable is an atomic compatibility claim. It does not by itself claim
that every production occurrence on that target reproduces locally;
that is the separate production-to-local qualification above.

<!-- /generated:support-claim -->
