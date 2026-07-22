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
    /// regranting permissions, and recording from the host. Backends that
    /// return false have a runner that brings its own target (a browser, an
    /// Appium session, a desktop app, a PTY), so reproit only assigns it a
    /// logical label. Keyed on the backend, not a "not-Flutter" guess, so a
    /// new backend that needs device provisioning opts in here rather than
    /// by editing the orchestrator.
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

    /// Whether this backend's runner implements the multi-actor conductor
    /// client (`GET /claim` + `GET /next` + `POST /done`, see
    /// modes/barrier.rs), i.e. can play one actor of an authored multi-user
    /// scenario. Keyed on the backend, not the platform id, so every
    /// platform riding a supporting backend gets scenarios at once (web,
    /// electron and tauri all ride WebCdp). `run_scenario` gates on this so
    /// a scenario on an unsupported backend fails with a clear message
    /// instead of booting idle devices.
    ///
    /// Every backend speaks it today. The conductor serializes actions
    /// GLOBALLY (one ACT outstanding at a time), so no backend needs
    /// concurrent-input isolation: a desktop actor drives its OWN app instance
    /// and brings its own window forward before its one action; an Appium
    /// actor is its own device session; an instrumented app polls between
    /// frames. Where each client lives:
    ///   flutter          assets/scaffolds/flutter/integration_test/reproit_explorer/runner.dart
    ///   web/electron/tauri  runners/web/runner.mjs, runners/electron.mjs,
    ///                    runners/tauri.mjs
    ///   appium           runners/rn/runner.mjs (each actor = its own session)
    ///   desktop ax/uia/atspi  runners/macos-ax.swift, and the in-process Rust
    ///                    runners backends/uia/mod.rs (`reproit __uia`) +
    ///                    backends/atspi/mod.rs (`reproit __atspi`) (per-actor
    /// app                    instance, bound by pid; window re-activated
    /// per action)   instrumented     the scenario core in
    /// reproit_imgui.h/reproit_clay.h                    (a dependency-free
    /// blocking HTTP client over POSIX/                    Winsock sockets,
    /// polled from FrameEnd)   tui              the actor loop in
    /// backends/tui/mod.rs The wire contract is pinned per runner source by
    /// tests/barrier_scenario.rs.
    pub fn speaks_barrier(self) -> bool {
        true
    }
}

/// A registered platform identifier (the `app.platform` string in
/// reproit.yaml).
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
        "Native Android (Jetpack Compose / Views) via Appium UiAutomator2; Compose nodes surface \
         by text/content-desc, and testTagsAsResourceId=true exposes testTag locators.",
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
        "Qt Widgets read through the host OS accessibility API (native gate: Linux AT-SPI).",
    ),
    (
        "gtk",
        "GTK read through the host OS accessibility API (native gate: Linux AT-SPI).",
    ),
    (
        "avalonia",
        "Avalonia UI read through the host OS accessibility API (native gate: Windows UIA).",
    ),
    (
        "wxwidgets",
        "wxWidgets read through the host OS accessibility API (native gate: Linux AT-SPI).",
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
/// (`/claim` + `/next` + `/done`). Pure routing: the capability lives on the
/// BACKEND (see `Backend::speaks_barrier`), so a platform supports scenarios
/// exactly when its backend's runner does, and a new platform mapped onto a
/// supporting backend inherits scenarios with no edit here.
pub fn speaks_barrier(id: &str) -> bool {
    resolve(id)
        .map(|p| p.backend.speaks_barrier())
        .unwrap_or(false)
}

/// One-line list of the platform ids whose backend speaks the multi-actor
/// conductor protocol, for the `run_scenario` gate's error text.
pub fn barrier_ids() -> String {
    all()
        .iter()
        .filter(|p| p.backend.speaks_barrier())
        .map(|p| p.id)
        .collect::<Vec<_>>()
        .join(", ")
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
    use std::path::Path;

    fn workflow_invokes_gate(repo: &Path, workflow_text: &str, gate_id: &str) -> bool {
        let invocation = format!("gate.py {gate_id}");
        if workflow_text.contains(&invocation) {
            return true;
        }

        workflow_text
            .split_whitespace()
            .map(|token| {
                token.trim_matches(|character: char| {
                    matches!(character, '\'' | '"' | '(' | ')' | '[' | ']' | ',' | ':')
                })
            })
            .filter(|token| token.starts_with(".github/scripts/") && token.ends_with(".sh"))
            .any(|script| {
                std::fs::read_to_string(repo.join(script))
                    .is_ok_and(|script_text| script_text.contains(&invocation))
            })
    }

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

    #[test]
    fn barrier_support_is_keyed_on_the_backend() {
        // Every registered platform speaks the conductor protocol: the WebCdp
        // trio agree by construction (the old per-platform list supported web
        // but not electron/tauri, the exact bug the registry keying removed),
        // and every other backend now ships a conductor client too (pinned to
        // each runner source by tests/barrier_scenario.rs).
        for p in all() {
            assert!(
                speaks_barrier(p.id),
                "{} rides a barrier-speaking backend",
                p.id
            );
            // Per-platform answers agree with the backend capability, always.
            assert_eq!(speaks_barrier(p.id), p.backend.speaks_barrier(), "{}", p.id);
        }
        assert!(!speaks_barrier("cobol-tui"), "unknown ids never speak it");
    }

    #[test]
    fn barrier_ids_lists_every_registered_platform() {
        let ids = barrier_ids();
        for p in all() {
            assert!(ids.contains(p.id), "{} missing from {ids}", p.id);
        }
    }

    #[test]
    fn every_registered_platform_has_a_runtime_evidence_gate() {
        let manifest = include_str!("../../../../validation/backends/evidence.json");
        let json: serde_json::Value =
            serde_json::from_str(manifest).expect("backend evidence manifest is valid JSON");
        assert_eq!(
            json.get("schema").and_then(serde_json::Value::as_u64),
            Some(2)
        );
        let platforms = json
            .get("platforms")
            .and_then(|value| value.as_object())
            .expect("evidence manifest has a platforms object");
        let gates = json
            .get("gates")
            .and_then(|value| value.as_object())
            .expect("evidence manifest has a gates object");

        let registered: std::collections::BTreeSet<&str> =
            all().into_iter().map(|platform| platform.id).collect();
        let evidenced: std::collections::BTreeSet<&str> =
            platforms.keys().map(String::as_str).collect();
        assert_eq!(
            evidenced, registered,
            "evidence ids must exactly match registry ids"
        );

        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let result_schema = json
            .get("resultSchema")
            .and_then(serde_json::Value::as_str)
            .expect("evidence manifest names its result schema");
        assert!(
            repo.join(result_schema).is_file(),
            "missing {result_schema}"
        );

        for (platform, platform_gates) in platforms {
            let platform_gates = platform_gates
                .as_array()
                .unwrap_or_else(|| panic!("{platform} evidence must be an array"));
            assert!(
                !platform_gates.is_empty(),
                "{platform} must name at least one gate"
            );
            for gate_id in platform_gates {
                let gate_id = gate_id
                    .as_str()
                    .unwrap_or_else(|| panic!("{platform} gate must be an id string"));
                let gate = gates
                    .get(gate_id)
                    .unwrap_or_else(|| panic!("{platform} references unknown gate {gate_id}"));
                let gate_platforms = gate["platforms"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{gate_id} platforms must be an array"));
                assert!(
                    gate_platforms.iter().any(|value| value == platform),
                    "{gate_id} does not claim {platform}"
                );
            }
        }

        for (gate_id, gate) in gates {
            let command = gate["command"]
                .as_array()
                .unwrap_or_else(|| panic!("{gate_id} command must be an array"));
            assert!(!command.is_empty(), "{gate_id} command cannot be empty");
            if let Some(script) = command.get(1).and_then(serde_json::Value::as_str) {
                assert!(
                    repo.join(script).is_file(),
                    "{gate_id} command missing: {script}"
                );
            }
            let timeout = gate["timeoutSeconds"]
                .as_u64()
                .unwrap_or_else(|| panic!("{gate_id} timeout must be an integer"));
            assert!(
                (1..=7_200).contains(&timeout),
                "{gate_id} timeout is unbounded"
            );
            for field in [
                "backend",
                "fixture",
                "targetOs",
                "resetStrategy",
                "cleanupStrategy",
            ] {
                assert!(
                    gate[field]
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty()),
                    "{gate_id} must declare {field}"
                );
            }
            let automation = gate["automation"]
                .as_object()
                .unwrap_or_else(|| panic!("{gate_id} automation must be an object"));
            let mode = automation["mode"]
                .as_str()
                .unwrap_or_else(|| panic!("{gate_id} automation mode must be a string"));
            if mode == "manual-required" {
                assert!(
                    automation["blocker"]
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty()),
                    "{gate_id} manual gate must record its blocker"
                );
                continue;
            }
            let workflow = automation["workflow"]
                .as_str()
                .unwrap_or_else(|| panic!("{gate_id} must name its workflow"));
            let job = automation["job"]
                .as_str()
                .unwrap_or_else(|| panic!("{gate_id} must name its workflow job"));
            let workflow_text = std::fs::read_to_string(repo.join(workflow))
                .unwrap_or_else(|error| panic!("read {workflow}: {error}"));
            assert!(
                workflow_text.contains(&format!("  {job}:\n")),
                "{gate_id} workflow does not define job {job}"
            );
            assert!(
                workflow_invokes_gate(&repo, &workflow_text, gate_id),
                "{gate_id} workflow or referenced script never invokes its gate"
            );
        }
    }
}
