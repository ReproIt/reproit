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
    /// Linux), and flutter-ios-sim needs a Mac. None means OS-agnostic.
    pub fn required_os(self) -> Option<&'static str> {
        match self {
            Backend::DesktopAx | Backend::FlutterDrive => Some("macos"),
            Backend::DesktopUia => Some("windows"),
            Backend::DesktopAtspi => Some("linux"),
            _ => None,
        }
    }
}

/// Implementation maturity. The registry is honest: `Planned` platforms parse
/// and report, but refuse to run with a message saying what's needed, rather
/// than faking a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Built and validated end to end.
    Live,
    /// Wired but not yet validated against a real target app.
    Beta,
    /// Registered and routed to a backend; runner not built yet. Kept for new
    /// backends added ahead of their runner.
    #[allow(dead_code)]
    Planned,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Live => "live",
            Status::Beta => "beta",
            Status::Planned => "planned",
        }
    }
    /// Whether `reproit run` will attempt execution. Planned platforms error
    /// early with guidance instead.
    pub fn executable(self) -> bool {
        matches!(self, Status::Live | Status::Beta)
    }
}

/// A registered platform identifier (the `app.platform` string in reproit.yaml).
#[derive(Debug, Clone, Copy)]
pub struct Platform {
    pub id: &'static str,
    pub backend: Backend,
    pub status: Status,
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
const STATIC_PLATFORMS: &[(Backend, Status, &str, &str)] = &[
    // id is the 3rd field for the static table; see resolve() below.
    // (backend, status, id, note)
    (
        Backend::FlutterDrive,
        Status::Live,
        "flutter-ios-sim",
        "Flutter on the iOS simulator via flutter drive + Dart VM service.",
    ),
    (
        Backend::WebCdp,
        Status::Live,
        "web-playwright",
        "Any web app driven by Playwright (Chromium); DOM a11y tree is the state.",
    ),
    (
        Backend::Appium,
        Status::Live,
        "rn-appium",
        "React Native over an Appium session; a11y source is the tree.",
    ),
    (
        Backend::WebCdp,
        Status::Live,
        "electron",
        "Electron desktop app: Chromium under the hood, driven over CDP.",
    ),
    (
        Backend::WebCdp,
        Status::Live,
        "tauri",
        "Tauri desktop app: system webview driven over WebDriver (tauri-driver).",
    ),
    (
        Backend::Appium,
        Status::Live,
        "swift-ios",
        "Native iOS (UIKit/SwiftUI) via XCUITest through Appium.",
    ),
    (
        Backend::Appium,
        Status::Live,
        "android",
        "Native Android (Jetpack Compose / Views) via Appium UiAutomator2; \
         Compose nodes surface by text/content-desc, and testTagsAsResourceId=true \
         exposes testTag locators.",
    ),
    (
        Backend::DesktopAx,
        Status::Live,
        "swift-macos",
        "Native macOS (AppKit/SwiftUI) read through the AXUIElement API.",
    ),
    (
        Backend::DesktopUia,
        Status::Live,
        "winui",
        "WinUI / WPF read through Windows UI Automation.",
    ),
    (
        Backend::Instrumented,
        Status::Live,
        "imgui",
        "Dear ImGui: immediate-mode, no a11y; needs the in-app reproit hook.",
    ),
    (
        Backend::Instrumented,
        Status::Live,
        "clay",
        "Clay: immediate-mode layout lib; needs the in-app reproit hook.",
    ),
    (
        Backend::Tui,
        Status::Live,
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

/// Resolve a platform identifier to its backend, status and note. Returns None
/// for unknown ids (config load turns that into a helpful error).
pub fn resolve(id: &str) -> Option<Platform> {
    for &(backend, status, pid, note) in STATIC_PLATFORMS {
        if pid == id {
            return Some(Platform {
                id: pid,
                backend,
                status,
                note,
            });
        }
    }
    for &(pid, note) in DESKTOP_TOOLKITS {
        if pid == id {
            return Some(Platform {
                id: pid,
                backend: desktop_backend(),
                status: Status::Beta,
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
    matches!(id, "web-playwright" | "flutter-ios-sim")
}

/// The full support matrix, for `reproit platforms` and error messages.
pub fn all() -> Vec<Platform> {
    let mut out: Vec<Platform> = STATIC_PLATFORMS
        .iter()
        .map(|&(backend, status, id, note)| Platform {
            id,
            backend,
            status,
            note,
        })
        .collect();
    for &(id, note) in DESKTOP_TOOLKITS {
        out.push(Platform {
            id,
            backend: desktop_backend(),
            status: Status::Beta,
            note,
        });
    }
    out
}

/// One-line, copy-pasteable summary of valid platform ids for error text.
pub fn known_ids() -> String {
    let mut ids: Vec<&str> = STATIC_PLATFORMS.iter().map(|t| t.2).collect();
    ids.extend(DESKTOP_TOOLKITS.iter().map(|t| t.0));
    ids.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validated_platforms_are_executable() {
        for id in ["flutter-ios-sim", "web-playwright", "rn-appium", "android"] {
            let p = resolve(id).expect("known");
            assert!(p.status.executable(), "{id} should be executable");
        }
    }

    #[test]
    fn webviews_share_the_web_backend() {
        for id in ["web-playwright", "electron", "tauri"] {
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
        for id in ["rn-appium", "swift-ios", "android"] {
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
