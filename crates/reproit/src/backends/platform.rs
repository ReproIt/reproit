//! Platform registry: every UI stack reproit supports maps onto one of a small
//! set of *backends*, keyed by HOW the UI tree is read and driven. Adding a
//! framework is choosing its backend, not writing a new engine: ~80% of reproit
//! (map, fuzz, soak, evidence, fix) runs on the framework-agnostic marker
//! protocol, so a backend only has to launch the app and emit those markers.
//!
//! This is why "support everything" is tractable. Qt, GTK, WinUI, Avalonia,
//! wxWidgets and native AppKit are NOT six integrations: on a given OS they all
//! publish to the same accessibility bus (AX / UI Automation / AT-SPI), so they
//! share ONE backend. Electron and Tauri are web engines, so they share the web
//! backend. Only immediate-mode GUIs (Dear ImGui, Clay), which have no retained
//! widget tree and no a11y, need a different shape (an in-app hook).

/// How a platform's UI tree is introspected and driven. The runner differs per
/// backend; everything downstream of the markers is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// `flutter drive` over the Dart VM service + semantics tree.
    FlutterDrive,
    /// A web engine driven over CDP/WebDriver (Playwright). Covers real
    /// browsers AND embedded webviews, since the DOM accessibility tree is the
    /// state tree: Electron is Chromium, Tauri is the system webview.
    WebCdp,
    /// A device/emulator driven over Appium; the a11y source is the tree.
    /// Covers React Native and native iOS/Android (XCUITest / UiAutomator2).
    Appium,
    /// Native desktop read through the macOS accessibility API (AXUIElement).
    DesktopAx,
    /// Native desktop read through Windows UI Automation (UIA).
    DesktopUia,
    /// Native desktop read through Linux AT-SPI2.
    DesktopAtspi,
    /// Immediate-mode GUIs (Dear ImGui, Clay) have no retained widget tree and
    /// no OS a11y, so they cannot be introspected from outside. The app links a
    /// tiny reproit hook that walks the frame's widget/ID stack and prints the
    /// marker protocol. The only backend that needs app cooperation.
    Instrumented,
    /// Terminal UIs (any CLI/TUI: vim, lazygit, k9s, Claude Code) driven in a
    /// pseudo-terminal. The "screen" is the VT cell grid parsed from the app's
    /// ANSI output; actions are keystrokes. Fully headless, no input hijack,
    /// the most deterministic backend of all.
    Tui,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::FlutterDrive => "flutter-drive",
            Backend::WebCdp => "web-cdp",
            Backend::Appium => "appium",
            Backend::DesktopAx => "desktop-ax (macOS)",
            Backend::DesktopUia => "desktop-uia (Windows)",
            Backend::DesktopAtspi => "desktop-atspi (Linux)",
            Backend::Instrumented => "instrumented (in-app hook)",
            Backend::Tui => "tui-pty (terminal)",
        }
    }

    /// Whether reproit itself provisions and manages the target device for this
    /// backend: booting named `<prefix>-X` simulators, pinning determinism,
    /// regranting permissions, and recording from the host. Backends that return
    /// false have a runner that brings its own target (a browser, an Appium
    /// session, a desktop app, a PTY), so reproit only assigns it a logical
    /// label. Keyed on the backend, not a "not-Flutter" guess, so a new backend
    /// that needs device provisioning opts in here rather than by editing the
    /// orchestrator.
    pub fn provisions_device(self) -> bool {
        matches!(self, Backend::FlutterDrive)
    }

    /// The host OS a backend must run on, if it is OS-bound. The desktop
    /// accessibility APIs are OS-specific (AX = macOS, UIA = Windows, AT-SPI =
    /// Linux), and flutter needs a Mac. None means OS-agnostic.
    pub fn required_os(self) -> Option<&'static str> {
        match self {
            Backend::DesktopAx | Backend::FlutterDrive => Some("macos"),
            Backend::DesktopUia => Some("windows"),
            Backend::DesktopAtspi => Some("linux"),
            _ => None,
        }
    }
}

/// A registered platform identifier (the `app.platform` string in reproit.yaml).
#[derive(Debug, Clone, Copy)]
pub struct Platform {
    pub id: &'static str,
    pub backend: Backend,
    /// Human description of the toolkit and what running it needs.
    pub note: &'static str,
}

/// The desktop backend for the current host OS. The toolkit (Qt/GTK/WinUI/
/// Avalonia/wxWidgets) is irrelevant: each publishes to whatever accessibility
/// API its host OS provides, so the backend is chosen by OS, not toolkit.
fn desktop_backend() -> Backend {
    match std::env::consts::OS {
        "macos" => Backend::DesktopAx,
        "windows" => Backend::DesktopUia,
        _ => Backend::DesktopAtspi,
    }
}

/// Static platforms whose backend does not depend on the host OS.
const STATIC_PLATFORMS: &[(Backend, &str, &str)] = &[
    // id is the 2nd field for the static table; see resolve() below.
    // (backend, id, note)
    (
        Backend::FlutterDrive,
        "flutter",
        "Flutter on the iOS simulator via flutter drive + Dart VM service.",
    ),
    (
        Backend::WebCdp,
        "web",
        "Any web app driven by Playwright (Chromium); DOM a11y tree is the state.",
    ),
    (
        Backend::Appium,
        "react-native",
        "React Native over an Appium session; a11y source is the tree.",
    ),
    (
        Backend::WebCdp,
        "electron",
        "Electron desktop app: Chromium under the hood, driven over CDP.",
    ),
    (
        Backend::WebCdp,
        "tauri",
        "Tauri desktop app: system webview driven over WebDriver (tauri-driver).",
    ),
    (
        Backend::Appium,
        "swift-ios",
        "Native iOS (UIKit/SwiftUI) via XCUITest through Appium.",
    ),
    (
        Backend::Appium,
        "android",
        "Native Android (Jetpack Compose / Views) via Appium UiAutomator2; \
         Compose nodes surface by text/content-desc, and testTagsAsResourceId=true \
         exposes testTag locators.",
    ),
    (
        Backend::DesktopAx,
        "swift-macos",
        "Native macOS (AppKit/SwiftUI) read through the AXUIElement API.",
    ),
    (
        Backend::DesktopUia,
        "winui",
        "WinUI / WPF read through Windows UI Automation.",
    ),
    (
        Backend::Instrumented,
        "imgui",
        "Dear ImGui: immediate-mode, no a11y; needs the in-app reproit hook.",
    ),
    (
        Backend::Instrumented,
        "clay",
        "Clay: immediate-mode layout lib; needs the in-app reproit hook.",
    ),
    (
        Backend::Tui,
        "tui",
        "Any terminal UI / CLI (vim, lazygit, k9s, Claude Code) driven in a PTY.",
    ),
];

/// Cross-platform desktop toolkits: same `id`, backend resolved from host OS.
const DESKTOP_TOOLKITS: &[(&str, &str)] = &[
    (
        "qt",
        "Qt (Widgets/QML) read through the host OS accessibility API.",
    ),
    (
        "gtk",
        "GTK read through the host OS accessibility API (AT-SPI on Linux).",
    ),
    (
        "avalonia",
        "Avalonia UI read through the host OS accessibility API.",
    ),
    (
        "wxwidgets",
        "wxWidgets read through the host OS accessibility API.",
    ),
];

/// Resolve a platform identifier to its backend and note. Returns None
/// for unknown ids (config load turns that into a helpful error).
pub fn resolve(id: &str) -> Option<Platform> {
    for &(backend, pid, note) in STATIC_PLATFORMS {
        if pid == id {
            return Some(Platform {
                id: pid,
                backend,
                note,
            });
        }
    }
    for &(pid, note) in DESKTOP_TOOLKITS {
        if pid == id {
            return Some(Platform {
                id: pid,
                backend: desktop_backend(),
                note,
            });
        }
    }
    None
}

/// Backend for a platform id, if known. Convenience for callers that only
/// need routing, not the full record.
#[allow(dead_code)]
pub fn backend(id: &str) -> Option<Backend> {
    resolve(id).map(|p| p.backend)
}

/// Whether this platform's runner implements the multi-actor conductor client
/// (`/claim` + `/next` + `/done`), i.e. can drive an authored multi-user
/// scenario. Keyed on platform id, not backend, because the WebCdp backend
/// covers both the Playwright runner (which speaks it) and the Electron/Tauri
/// runners (which do not yet). `run_scenario` gates on this so a scenario on an
/// unsupported backend fails with a clear message instead of booting idle
/// devices that just sit there.
pub fn speaks_barrier(id: &str) -> bool {
    matches!(id, "web" | "flutter")
}

/// The full support matrix, for `reproit platforms` and error messages.
pub fn all() -> Vec<Platform> {
    let mut out: Vec<Platform> = STATIC_PLATFORMS
        .iter()
        .map(|&(backend, id, note)| Platform { id, backend, note })
        .collect();
    for &(id, note) in DESKTOP_TOOLKITS {
        out.push(Platform {
            id,
            backend: desktop_backend(),
            note,
        });
    }
    out
}

/// One-line, copy-pasteable summary of valid platform ids for error text.
pub fn known_ids() -> String {
    let mut ids: Vec<&str> = STATIC_PLATFORMS.iter().map(|t| t.1).collect();
    ids.extend(DESKTOP_TOOLKITS.iter().map(|t| t.0));
    ids.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_platforms_are_resolvable() {
        for id in ["flutter", "web", "react-native", "android"] {
            let p = resolve(id).expect("known");
            assert_eq!(p.id, id);
        }
    }

    #[test]
    fn webviews_share_the_web_backend() {
        for id in ["web", "electron", "tauri"] {
            assert_eq!(backend(id), Some(Backend::WebCdp), "{id}");
        }
    }

    #[test]
    fn immediate_mode_is_instrumented() {
        assert_eq!(backend("imgui"), Some(Backend::Instrumented));
        assert_eq!(backend("clay"), Some(Backend::Instrumented));
    }

    #[test]
    fn desktop_toolkits_resolve_to_one_os_backend() {
        // Qt and GTK collapse to the SAME backend on a given host: the point
        // of the taxonomy. Whatever this OS is, both agree.
        assert_eq!(backend("qt"), backend("gtk"));
        assert_eq!(backend("qt"), backend("avalonia"));
        assert_eq!(backend("qt"), backend("wxwidgets"));
    }

    #[test]
    fn native_mobile_shares_the_appium_backend() {
        // React Native, native iOS (XCUITest) and native Android (UiAutomator2)
        // are all Appium sessions: one backend, three a11y sources.
        for id in ["react-native", "swift-ios", "android"] {
            assert_eq!(backend(id), Some(Backend::Appium), "{id}");
        }
    }

    #[test]
    fn android_is_registered_and_appium_routed() {
        let p = resolve("android").expect("android is a known platform");
        assert_eq!(p.backend, Backend::Appium);
        // Appium is OS-agnostic: Android drives from any host with the SDK.
        assert_eq!(p.backend.required_os(), None);
        assert!(p.note.contains("UiAutomator2"), "note documents the driver");
    }

    #[test]
    fn unknown_platform_is_none() {
        assert!(resolve("cobol-tui").is_none());
    }
}
