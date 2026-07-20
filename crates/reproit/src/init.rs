//! `reproit init`: scaffold a repo for reproit in one command. Detects
//! Flutter vs web, writes reproit.yaml, vendors the explorer + driver
//! (Flutter) and fills the app entry point automatically when it can.
//! Scaffolds are embedded in the binary so init works standalone.

use anyhow::{bail, Result};
use regex::Regex;
use std::path::Path;

const EXPLORER: &str = include_str!("../scaffolds/flutter/integration_test/journey_explore.dart");
const EXPLORER_HEADLESS: &str = include_str!("../scaffolds/flutter/test/fuzz_headless_test.dart");
const EXPLORER_LIBRARY: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer.dart");
const EXPLORER_CONFIG: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/config.dart");
const EXPLORER_SIGNATURE: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/signature.dart");
const EXPLORER_SEMANTICS: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/semantics.dart");
const EXPLORER_GROUND_TRUTH: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/ground_truth.dart");
const EXPLORER_HYGIENE_ORACLES: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/hygiene_oracles.dart");
const EXPLORER_INVARIANTS: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/invariants.dart");
const EXPLORER_ENVIRONMENT_ORACLES: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/environment_oracles.dart");
const EXPLORER_SIMULATOR_WATCHDOG: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/simulator_watchdog.dart");
const EXPLORER_RUNTIME: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/runtime.dart");
const EXPLORER_RUNNER: &str =
    include_str!("../scaffolds/flutter/integration_test/reproit_explorer/runner.dart");
const HELPERS: &str = include_str!("../scaffolds/flutter/integration_test/journey_helpers.dart");

/// Shared files imported by both generated explorer entry points. Paths are
/// relative to `journeys.dir`, which defaults to `integration_test`.
const EXPLORER_SHARED_FILES: &[(&str, &str)] = &[
    ("reproit_explorer.dart", EXPLORER_LIBRARY),
    ("reproit_explorer/config.dart", EXPLORER_CONFIG),
    ("reproit_explorer/signature.dart", EXPLORER_SIGNATURE),
    ("reproit_explorer/semantics.dart", EXPLORER_SEMANTICS),
    ("reproit_explorer/ground_truth.dart", EXPLORER_GROUND_TRUTH),
    (
        "reproit_explorer/hygiene_oracles.dart",
        EXPLORER_HYGIENE_ORACLES,
    ),
    ("reproit_explorer/invariants.dart", EXPLORER_INVARIANTS),
    (
        "reproit_explorer/environment_oracles.dart",
        EXPLORER_ENVIRONMENT_ORACLES,
    ),
    (
        "reproit_explorer/simulator_watchdog.dart",
        EXPLORER_SIMULATOR_WATCHDOG,
    ),
    ("reproit_explorer/runtime.dart", EXPLORER_RUNTIME),
    ("reproit_explorer/runner.dart", EXPLORER_RUNNER),
];

const INTEGRATION_DRIVER: &str =
    "import 'package:integration_test/integration_test_driver.dart';\n\n\
     Future<void> main() => integrationDriver();\n";

/// The import comment block both explorer entries carry (sim + headless).
const IMPORT_NEEDLE: &str =
    "// APP-SPECIFIC: import your app's root widget.\n// import 'package:your_app/app.dart';";
/// The pump line both explorer entries carry inside their `pumpApp` callback.
const PUMP_NEEDLE: &str = "      // await t.pumpWidget(const YourApp());";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Platform {
    Flutter,
    Web,
    Rn,
    Android,
    Backend,
}

pub fn init(dir: &Path, platform: Option<&str>, force: bool) -> Result<()> {
    let platform = match platform {
        Some("flutter") => Platform::Flutter,
        Some("web") => Platform::Web,
        Some("rn") | Some("react-native") => Platform::Rn,
        Some("android") => Platform::Android,
        Some("backend") => Platform::Backend,
        Some(other) => {
            bail!("unknown platform {other:?} (expected flutter, web, rn, android, or backend)")
        }
        None => detect(dir).ok_or_else(|| {
            anyhow::anyhow!(
                "could not detect a supported UI project or backend schema; pass --platform \
                 explicitly"
            )
        })?,
    };
    let cfg_path = dir.join("reproit.yaml");
    if cfg_path.exists() && !force {
        bail!("reproit.yaml already exists (use --force to overwrite)");
    }

    match platform {
        Platform::Flutter => init_flutter(dir, force)?,
        Platform::Web => init_web(dir, force)?,
        Platform::Rn => write(&cfg_path, RN_CONFIG, force)?,
        Platform::Android => write(&cfg_path, ANDROID_CONFIG, force)?,
        Platform::Backend => init_backend(dir, &cfg_path, force)?,
    }
    ensure_gitignore(dir)?;
    println!("\n  reproit initialized.");
    match platform {
        Platform::Flutter => {
            println!("  1. confirm the app entry in integration_test/journey_explore.dart");
            println!("  2. reproit doctor");
            println!("  3. reproit scan   # visible issues");
            println!("     reproit fuzz   # deeper interaction bugs");
        }
        Platform::Web => {
            println!("  1. edit reproit.yaml: set app.url to your dev/staging URL");
            println!("  2. reproit doctor");
            println!("  3. reproit scan   # visible issues");
            println!("     reproit fuzz   # deeper interaction bugs");
            println!("     (the web runner auto-provisions on first run; needs Node 18+)");
        }
        Platform::Rn => {
            println!(
                "  1. edit reproit.yaml: set app.appiumCaps (app path, deviceName, platformName)"
            );
            println!("  2. (cd <reproit>/runners/rn && npm install); start an Appium server");
            println!("  3. reproit fuzz");
        }
        Platform::Android => {
            println!(
                "  1. edit reproit.yaml: set app.appiumCaps (apk path, appPackage, appActivity)"
            );
            println!(
                "  2. (cd <reproit>/runners/rn && npm install); appium driver install uiautomator2"
            );
            println!("  3. boot an AVD (emulator -avd <name>); start an Appium server");
            println!("  4. reproit fuzz");
        }
        Platform::Backend => {
            println!("  1. start a disposable local or staging service");
            println!("  2. reproit scan   # read-only contract checks");
            println!("     reproit fuzz   # stateful interaction bugs");
        }
    }
    Ok(())
}

fn detect(dir: &Path) -> Option<Platform> {
    if detect_backend_schema(dir).is_some() {
        Some(Platform::Backend)
    } else if dir.join("pubspec.yaml").exists() {
        Some(Platform::Flutter)
    } else if dir.join("package.json").exists() {
        let pkg = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
        if pkg.contains("\"react-native\"") {
            Some(Platform::Rn)
        } else {
            Some(Platform::Web)
        }
    } else if dir.join("index.html").exists() {
        Some(Platform::Web)
    } else {
        None
    }
}

fn init_backend(dir: &Path, config: &Path, force: bool) -> Result<()> {
    let schema = detect_backend_schema(dir).ok_or_else(|| {
        anyhow::anyhow!(
            "could not find an OpenAPI, GraphQL introspection, or protobuf descriptor schema"
        )
    })?;
    let relative = schema
        .strip_prefix(dir)
        .unwrap_or(&schema)
        .to_string_lossy();
    let relative = serde_json::to_string(relative.as_ref())?;
    let content = format!(
        "# Reproit backend config. The schema owns structural contracts.\nbackend:\n  enabled: \
         true\n  schemas:\n    - {relative}\n"
    );
    write(config, &content, force)
}

fn detect_backend_schema(dir: &Path) -> Option<std::path::PathBuf> {
    const NAMES: &[&str] = &[
        "openapi.yaml",
        "openapi.yml",
        "openapi.json",
        "swagger.yaml",
        "swagger.yml",
        "swagger.json",
        "schema.graphql.json",
        "graphql-schema.json",
        "schema.graphql",
        "schema.gql",
        "descriptor.json",
        "protobuf-descriptor.json",
    ];
    NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
        .or_else(|| {
            let mut protos = std::fs::read_dir(dir)
                .ok()?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("proto"))
                .collect::<Vec<_>>();
            protos.sort();
            protos.into_iter().next()
        })
}

fn write(path: &Path, content: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        println!("  skip  {} (exists)", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    println!("  write {}", path.display());
    Ok(())
}

/// The repro suite (`.reproit/repros/`) is committed on purpose: it's the
/// regression guard. Run evidence, recordings, scratch files, logs, and the
/// secrets vault are local-only and must never land in git. Paths are relative
/// to `.reproit/`, so `repros/` and `map/` stay trackable while local evidence
/// stays ignored.
const REPROIT_GITIGNORE: &str = "\
# The repro suite (repros/) and learned map (map/) are reviewable project state.
# Raw runs, recordings, scratch files, and secrets are local-only.
/runs/
/recordings/
/captures/
/tmp/
/capsules/
*.vault
*.key
*.log
";

/// Write `.reproit/.gitignore` so a `git add .` can't include run artifacts,
/// recorded media, or the secrets vault. Idempotent and non-destructive: if the
/// file already exists (a user may have customized it) we leave it untouched.
fn ensure_gitignore(dir: &Path) -> Result<()> {
    let path = crate::layout::reproit_dir(dir).join(".gitignore");
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, REPROIT_GITIGNORE)?;
    println!("  write {}", path.display());
    Ok(())
}

fn init_flutter(dir: &Path, force: bool) -> Result<()> {
    let (import, pump) = detect_app_entry(dir);
    // Fill the two APP-SPECIFIC lines in BOTH the sim explorer (flutter drive)
    // and the headless explorer (flutter test, the default fuzz/check tier).
    let fill = |tmpl: &str| {
        tmpl.replace(IMPORT_NEEDLE, &import)
            .replace(PUMP_NEEDLE, &pump)
    };
    write(
        &dir.join("integration_test/journey_explore.dart"),
        &fill(EXPLORER),
        force,
    )?;
    // Vendor the headless explorer under test/: integration_test depends on
    // dart:ui on a device, while this entry is compiled by `flutter test`.
    write(
        &dir.join("test/fuzz_headless_test.dart"),
        &fill(EXPLORER_HEADLESS),
        force,
    )?;
    for (relative, content) in EXPLORER_SHARED_FILES {
        write(&dir.join("integration_test").join(relative), content, force)?;
    }
    write(
        &dir.join("integration_test/journey_helpers.dart"),
        HELPERS,
        force,
    )?;
    write(
        &dir.join("test_driver/integration_driver.dart"),
        INTEGRATION_DRIVER,
        force,
    )?;
    ensure_integration_test_dep(dir)?;
    write(&dir.join("reproit.yaml"), &flutter_config(dir), force)?;
    Ok(())
}

/// Self-heal the FlutterDrive sim tier: vendor reproit's own explorer (the same
/// journey_explore.dart + helpers + driver that `reproit init` lays down, with
/// the app entry inferred from the project) into a project that only had the
/// headless explorer, so `--record-video` / `--sim` just works instead
/// of erroring on a file reproit knows how to create. Only writes what's
/// missing, so a configured explorer, shared module, helper, or driver is never
/// clobbered.
pub fn vendor_sim_explorer(
    project_dir: &Path,
    journeys_dir: &Path,
    driver_rel: &str,
) -> Result<()> {
    let (import, pump) = detect_app_entry(project_dir);
    let explorer = EXPLORER
        .replace(IMPORT_NEEDLE, &import)
        .replace(PUMP_NEEDLE, &pump);
    std::fs::create_dir_all(journeys_dir)?;
    vendor_missing(
        &journeys_dir.join("journey_explore.dart"),
        &explorer,
        " (reproit's sim explorer)",
    )?;
    for (relative, content) in EXPLORER_SHARED_FILES {
        vendor_missing(&journeys_dir.join(relative), content, "")?;
    }
    vendor_missing(&journeys_dir.join("journey_helpers.dart"), HELPERS, "")?;
    let driver_path = project_dir.join(driver_rel);
    vendor_missing(&driver_path, INTEGRATION_DRIVER, "")?;
    // The flutter-drive tier imports package:integration_test, so it must be a
    // dev dependency or the sim build fails (the headless tier needs only
    // flutter_test, which Flutter projects have by default).
    ensure_integration_test_dep(project_dir)?;
    Ok(())
}

/// Write one on-demand scaffold dependency only when the project does not
/// already own it. This is deliberately stricter than `init --force`: runtime
/// self-healing must never replace user customization.
fn vendor_missing(path: &Path, content: &str, suffix: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    eprintln!("  vendored {}{suffix}", path.display());
    Ok(())
}

/// Ensure the `integration_test` dev dependency the flutter-drive (sim) tier
/// needs is in pubspec.yaml; add it under dev_dependencies if absent. Shared by
/// `init` and the on-demand sim self-heal so neither leaves the sim tier
/// un-buildable. The next `flutter` build runs pub get and resolves it.
pub fn ensure_integration_test_dep(project_dir: &Path) -> Result<()> {
    let pubspec = project_dir.join("pubspec.yaml");
    let Ok(content) = std::fs::read_to_string(&pubspec) else {
        return Ok(());
    };
    if content.contains("integration_test") {
        return Ok(());
    }
    if let Some(idx) = content.find("\ndev_dependencies:") {
        let at = idx + "\ndev_dependencies:".len();
        let patched = format!(
            "{}\n  integration_test:\n    sdk: flutter{}",
            &content[..at],
            &content[at..]
        );
        std::fs::write(&pubspec, patched)?;
        eprintln!(
            "  added integration_test dev dependency to {}",
            pubspec.display()
        );
    }
    Ok(())
}

fn init_web(dir: &Path, force: bool) -> Result<()> {
    write(&dir.join("reproit.yaml"), &web_config(None, None)?, force)?;
    Ok(())
}

/// Persist the zero-config URL workflow so later bare `scan` and `fuzz` calls
/// target the same web application.
pub fn init_web_url(dir: &Path, url: &str, runner: &Path, force: bool) -> Result<()> {
    let config = dir.join("reproit.yaml");
    if config.exists() && !force {
        bail!("reproit.yaml already exists (use --force to overwrite)");
    }
    write(&config, &web_config(Some(url), Some(runner))?, force)?;
    ensure_gitignore(dir)?;
    println!("\n  reproit initialized for {url}.");
    println!("  1. reproit doctor");
    println!("  2. reproit scan   # visible issues");
    println!("     reproit fuzz   # deeper interaction bugs");
    Ok(())
}

fn web_config(url: Option<&str>, runner: Option<&Path>) -> Result<String> {
    let url = serde_json::to_string(url.unwrap_or("http://localhost:3000"))?;
    let runner = serde_json::to_string(
        &runner
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "../reproit/runners/web".into()),
    )?;
    Ok(WEB_CONFIG
        .replace("{{URL}}", &url)
        .replace("{{WEB_RUNNER_DIR}}", &runner))
}

/// Best-effort: find the package name (pubspec) and the runApp widget in
/// lib/main.dart, so the explorer's two app-specific lines fill themselves.
fn detect_app_entry(dir: &Path) -> (String, String) {
    let pkg = std::fs::read_to_string(dir.join("pubspec.yaml"))
        .ok()
        .and_then(|s| {
            Regex::new(r"(?m)^name:\s*(\S+)")
                .unwrap()
                .captures(&s)
                .map(|c| c[1].to_string())
        });
    let widget = std::fs::read_to_string(dir.join("lib/main.dart"))
        .ok()
        .and_then(|s| {
            Regex::new(r"runApp\(\s*(?:const\s+)?(\w+)\s*\(")
                .unwrap()
                .captures(&s)
                .map(|c| c[1].to_string())
        });
    match (pkg, widget) {
        (Some(pkg), Some(w)) => (
            format!("import 'package:{pkg}/main.dart';"),
            format!("      await t.pumpWidget(const {w}());"),
        ),
        _ => (
            "// TODO: import your app's root widget, e.g.\n// import 'package:your_app/main.dart';"
                .to_string(),
            "      // TODO: pump your app's root widget, e.g.\n      // await \
             t.pumpWidget(const MyApp());"
                .to_string(),
        ),
    }
}

fn flutter_config(dir: &Path) -> String {
    let bundle = std::fs::read_to_string(dir.join("pubspec.yaml"))
        .ok()
        .and_then(|s| {
            Regex::new(r"(?m)^name:\s*(\S+)")
                .unwrap()
                .captures(&s)
                .map(|c| c[1].to_string())
        })
        .unwrap_or_else(|| "com.example.app".to_string());
    FLUTTER_CONFIG.replace("{{BUNDLE}}", &bundle)
}

const FLUTTER_CONFIG: &str = r#"# reproit config (flutter). See the example in the reproit repo for
# the full set of options (reset steps, hooks, visual baselines, llm).
app:
  platform: flutter
  projectDir: .
  bundleId: {{BUNDLE}}
  defines: {}

devices:
  deviceType: iPhone 16 Plus
  namePrefix: App           # reproit only touches App-A, App-B, ... simulators
  determinism:
    statusBarTime: "9:41"
    disableKeyboardIntro: true
  permissions: []

# State reset runs before every gate run. Add your dev/staging reset
# endpoints here so each run starts from a known state.
reset:
  steps: []

journeys:
  dir: integration_test
  driver: test_driver/integration_driver.dart
  readyMarker: claimed role
  doneMarkers:
    - All tests passed
    - Some tests failed
  deviceDoneMarker: "JOURNEY DONE"
  actionPrefix: "JOURNEY"
  timeoutSec: 600

evidence:
  outDir: .reproit/runs
  video: true
  composite: true
  screenshotMarker: "SHOOT:"

gate:
  runs: 5

# Which LLM powers authoring / analyze / fix. codex-cli (ChatGPT
# subscription) is the default; see the example config for all providers.
llm:
  provider: codex-cli
"#;

const WEB_CONFIG: &str = r#"# reproit config (web). Drives a browser with Playwright; the
# whole map/fuzz/soak/a11y/evidence pipeline is shared with the mobile path.
app:
  platform: web
  # Path to reproit's web runner directory (run npm install there once).
  webRunnerDir: {{WEB_RUNNER_DIR}}
  url: {{URL}}      # your dev/staging URL
  defines: {}

devices:
  namePrefix: web

reset:
  steps: []

journeys:
  dir: integration_test
  driver: web
  readyMarker: claimed role
  doneMarkers:
    - All tests passed
    - Some tests failed
  deviceDoneMarker: "JOURNEY DONE"
  actionPrefix: "JOURNEY"
  timeoutSec: 300

evidence:
  outDir: .reproit/runs
  video: false
  composite: false
  screenshotMarker: "SHOOT:"

gate:
  runs: 3

llm:
  provider: codex-cli
"#;

const RN_CONFIG: &str = r#"# reproit config (react-native). Drives a React Native app over an Appium
# session; the whole map/fuzz/soak/a11y/evidence pipeline is shared.
app:
  platform: react-native
  rnRunnerDir: ../reproit/runners/rn   # run npm install there once
  appiumUrl: http://127.0.0.1:4723
  appiumCaps:
    platformName: iOS                # or Android
    "appium:deviceName": iPhone 16 Plus
    "appium:automationName": XCUITest   # UiAutomator2 for Android
    "appium:app": /path/to/YourApp.app  # .app for sim, .apk for Android
  defines: {}

devices:
  namePrefix: rn

reset:
  steps: []

journeys:
  dir: integration_test
  driver: rn
  readyMarker: claimed role
  doneMarkers:
    - All tests passed
    - Some tests failed
  deviceDoneMarker: "JOURNEY DONE"
  actionPrefix: "JOURNEY"
  timeoutSec: 300

evidence:
  outDir: .reproit/runs
  video: false
  composite: false
  screenshotMarker: "SHOOT:"

gate:
  runs: 3

llm:
  provider: codex-cli
"#;

const ANDROID_CONFIG: &str = r#"# reproit config (android). Drives a native Android app
# (Jetpack Compose or
# Android Views) over an Appium UiAutomator2 session. Shares the exact same
# runner and marker protocol as react-native and swift-ios; only the caps differ.
#
# Jetpack Compose note: UiAutomator2 sees Compose nodes by their text and
# content-desc, which is what reproit's explorer keys off, so it works without
# any test ids. Set testTagsAsResourceId=true in the app to additionally expose
# Modifier.testTag values as resource-id locators. Adding contentDescription to
# icon-only buttons both improves accessibility and gives reproit better labels.
app:
  platform: android
  rnRunnerDir: ../reproit/runners/rn   # run npm install there once
  appiumUrl: http://127.0.0.1:4723
  appiumCaps:
    platformName: Android
    "appium:automationName": UiAutomator2
    "appium:app": /path/to/YourApp.apk        # or use appPackage + appActivity
    # "appium:appPackage": com.example.app
    # "appium:appActivity": .MainActivity
    "appium:deviceName": emulator-5554
  defines: {}

devices:
  namePrefix: android

reset:
  steps: []

journeys:
  dir: integration_test
  driver: android
  readyMarker: claimed role
  doneMarkers:
    - All tests passed
    - Some tests failed
  deviceDoneMarker: "JOURNEY DONE"
  actionPrefix: "JOURNEY"
  timeoutSec: 300

evidence:
  outDir: .reproit/runs
  video: false
  composite: false
  screenshotMarker: "SHOOT:"

gate:
  runs: 3

llm:
  provider: codex-cli
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_project(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "reproit-init-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn flutter_project(name: &str) -> std::path::PathBuf {
        let project = temporary_project(name);
        std::fs::create_dir_all(project.join("lib")).unwrap();
        std::fs::write(
            project.join("pubspec.yaml"),
            "name: demo_app\n\ndev_dependencies:\n  flutter_test:\n    sdk: flutter\n",
        )
        .unwrap();
        std::fs::write(
            project.join("lib/main.dart"),
            "void main() => runApp(const DemoApp());\n",
        )
        .unwrap();
        project
    }

    fn generated_flutter_files() -> Vec<(String, &'static str)> {
        let mut files = vec![
            ("integration_test/journey_explore.dart".into(), EXPLORER),
            ("test/fuzz_headless_test.dart".into(), EXPLORER_HEADLESS),
            ("integration_test/journey_helpers.dart".into(), HELPERS),
            (
                "test_driver/integration_driver.dart".into(),
                INTEGRATION_DRIVER,
            ),
        ];
        files.extend(
            EXPLORER_SHARED_FILES
                .iter()
                .map(|(path, content)| (format!("integration_test/{path}"), *content)),
        );
        files
    }

    #[test]
    fn the_app_specific_needles_exist_in_both_explorer_templates() {
        // If a template changes its wording, init's literal replace silently
        // no-ops; assert the needles still match so this can never rot again.
        assert!(
            EXPLORER.contains(IMPORT_NEEDLE),
            "explorer.dart import needle drifted"
        );
        assert!(
            EXPLORER.contains(PUMP_NEEDLE),
            "sim explorer pump needle drifted"
        );
        assert!(
            EXPLORER_HEADLESS.contains(IMPORT_NEEDLE),
            "headless explorer import needle drifted"
        );
        assert!(
            EXPLORER_HEADLESS.contains(PUMP_NEEDLE),
            "headless explorer pump needle drifted"
        );
    }

    #[test]
    fn backend_schema_wins_over_framework_files_and_needs_no_ui_config() {
        let project = temporary_project("backend");
        std::fs::write(project.join("package.json"), "{}").unwrap();
        std::fs::write(project.join("openapi.yaml"), "openapi: 3.1.0\npaths: {}\n").unwrap();
        assert_eq!(detect(&project), Some(Platform::Backend));
        init(&project, None, false).unwrap();
        let config = std::fs::read_to_string(project.join("reproit.yaml")).unwrap();
        assert!(config.contains("backend:\n  enabled: true"));
        assert!(config.contains("openapi.yaml"));
        assert!(!config.contains("app:"));
        std::fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn web_url_init_persists_the_exact_target_for_bare_commands() {
        let project = temporary_project("web-url");
        init_web_url(
            &project,
            "https://app.example.com/path?preview=one&mode=two",
            Path::new("/tmp/reproit web runner"),
            false,
        )
        .unwrap();
        let config = std::fs::read_to_string(project.join("reproit.yaml")).unwrap();
        assert!(config.contains("url: \"https://app.example.com/path?preview=one&mode=two\""));
        assert!(config.contains("webRunnerDir: \"/tmp/reproit web runner\""));
        assert!(project.join(".reproit/.gitignore").is_file());
        std::fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn generated_reproit_gitignore_keeps_project_state_reviewable() {
        assert!(REPROIT_GITIGNORE.contains("/runs/"));
        assert!(REPROIT_GITIGNORE.contains("/recordings/"));
        assert!(REPROIT_GITIGNORE.contains("/captures/"));
        assert!(REPROIT_GITIGNORE.contains("/tmp/"));
        assert!(REPROIT_GITIGNORE.contains("*.vault"));
        assert!(REPROIT_GITIGNORE.contains("*.log"));
        assert!(
            !REPROIT_GITIGNORE.contains("/map/"),
            "learned map should stay reviewable"
        );
        assert!(
            !REPROIT_GITIGNORE.contains("/repros/"),
            "saved repro suite should stay reviewable"
        );
    }

    #[test]
    fn init_flutter_writes_the_complete_scaffold_and_fills_both_entries() {
        let project = flutter_project("complete-flutter-scaffold");
        init_flutter(&project, false).unwrap();

        for (relative, _) in generated_flutter_files() {
            assert!(project.join(&relative).is_file(), "missing {relative}");
        }

        let sim =
            std::fs::read_to_string(project.join("integration_test/journey_explore.dart")).unwrap();
        // The pump line was actually filled, not left as the commented stub.
        assert!(
            sim.contains("await t.pumpWidget(const DemoApp());"),
            "sim explorer pump line was not filled"
        );
        assert!(
            !sim.contains(PUMP_NEEDLE),
            "the commented pump stub should be gone after fill"
        );
        assert!(sim.contains("import 'package:demo_app/main.dart';"));

        // The headless explorer is vendored (the default fuzz/check tier needs
        // it) and is filled the same way.
        let headless =
            std::fs::read_to_string(project.join("test/fuzz_headless_test.dart")).unwrap();
        assert!(
            headless.contains("await t.pumpWidget(const DemoApp());"),
            "headless explorer was not vendored/filled"
        );
        assert!(headless.contains("import 'package:demo_app/main.dart';"));

        std::fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn init_flutter_without_force_preserves_owned_scaffold_files() {
        let project = flutter_project("preserve-flutter-scaffold");
        for (relative, _) in generated_flutter_files() {
            let path = project.join(&relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("custom {relative}\n")).unwrap();
        }

        init_flutter(&project, false).unwrap();

        for (relative, _) in generated_flutter_files() {
            let actual = std::fs::read_to_string(project.join(&relative)).unwrap();
            assert_eq!(actual, format!("custom {relative}\n"));
        }
        std::fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn init_flutter_force_refreshes_scaffold_but_preserves_reproit_gitignore() {
        let project = flutter_project("force-flutter-scaffold");
        for (relative, _) in generated_flutter_files() {
            let path = project.join(&relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "custom\n").unwrap();
        }
        std::fs::create_dir_all(project.join(".reproit")).unwrap();
        std::fs::write(project.join(".reproit/.gitignore"), "custom ignore\n").unwrap();

        init(&project, Some("flutter"), true).unwrap();

        for (relative, content) in generated_flutter_files() {
            let actual = std::fs::read_to_string(project.join(&relative)).unwrap();
            if relative.ends_with("journey_explore.dart")
                || relative.ends_with("fuzz_headless_test.dart")
            {
                assert!(actual.contains("await t.pumpWidget(const DemoApp());"));
            } else {
                assert_eq!(actual, content, "force did not refresh {relative}");
            }
        }
        assert_eq!(
            std::fs::read_to_string(project.join(".reproit/.gitignore")).unwrap(),
            "custom ignore\n"
        );
        std::fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn sim_self_heal_adds_missing_dependencies_without_overwriting_custom_files() {
        let project = flutter_project("self-heal-flutter-scaffold");
        let journeys = project.join("integration_test");
        std::fs::create_dir_all(journeys.join("reproit_explorer")).unwrap();
        std::fs::create_dir_all(project.join("test_driver")).unwrap();
        let preserved = [
            "journey_explore.dart",
            "journey_helpers.dart",
            "reproit_explorer.dart",
            "reproit_explorer/config.dart",
        ];
        for relative in preserved {
            std::fs::write(journeys.join(relative), format!("custom {relative}\n")).unwrap();
        }
        std::fs::write(
            project.join("test_driver/integration_driver.dart"),
            "custom driver\n",
        )
        .unwrap();

        vendor_sim_explorer(&project, &journeys, "test_driver/integration_driver.dart").unwrap();

        for relative in preserved {
            let actual = std::fs::read_to_string(journeys.join(relative)).unwrap();
            assert_eq!(actual, format!("custom {relative}\n"));
        }
        assert_eq!(
            std::fs::read_to_string(project.join("test_driver/integration_driver.dart")).unwrap(),
            "custom driver\n"
        );
        for relative in [
            "reproit_explorer/signature.dart",
            "reproit_explorer/semantics.dart",
            "reproit_explorer/ground_truth.dart",
            "reproit_explorer/hygiene_oracles.dart",
            "reproit_explorer/invariants.dart",
            "reproit_explorer/environment_oracles.dart",
            "reproit_explorer/simulator_watchdog.dart",
            "reproit_explorer/runtime.dart",
            "reproit_explorer/runner.dart",
        ] {
            assert!(journeys.join(relative).is_file(), "missing {relative}");
        }
        std::fs::remove_dir_all(project).unwrap();
    }
}
