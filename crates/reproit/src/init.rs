//! `reproit init`: scaffold a repo for reproit in one command. Detects
//! Flutter vs web, writes reproit.yaml, vendors the explorer + driver
//! (Flutter) and fills the app entry point automatically when it can.
//! Templates are embedded in the binary so init works standalone.

use anyhow::{bail, Result};
use regex::Regex;
use std::path::Path;

const EXPLORER: &str = include_str!("../../../templates/explorer.dart");
const EXPLORER_HEADLESS: &str = include_str!("../../../templates/explorer_headless.dart");
const HELPERS: &str = include_str!("../../../templates/journey_helpers.dart");

/// The import comment block both explorer templates carry (sim + headless).
const IMPORT_NEEDLE: &str =
    "// APP-SPECIFIC: import your app's root widget.\n// import 'package:your_app/app.dart';";
/// The pump line both explorer templates carry inside `pumpApp(WidgetTester t)`.
/// The widget tester is bound to `t` here (NOT `tester`), so the filled line
/// must call `t.pumpWidget`.
const PUMP_NEEDLE: &str = "    // await t.pumpWidget(const YourApp());";

#[derive(Clone, Copy, PartialEq)]
pub enum Platform {
    Flutter,
    Web,
    Rn,
    Android,
}

pub fn init(dir: &Path, platform: Option<&str>, force: bool) -> Result<()> {
    let platform = match platform {
        Some("flutter") => Platform::Flutter,
        Some("web") => Platform::Web,
        Some("rn") | Some("react-native") => Platform::Rn,
        Some("android") => Platform::Android,
        Some(other) => {
            bail!("unknown platform {other:?} (expected flutter, web, rn, or android)")
        }
        None => detect(dir).ok_or_else(|| {
            anyhow::anyhow!(
                "could not detect platform (no pubspec.yaml or index.html); pass --platform flutter|web"
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
    }
    ensure_gitignore(dir)?;
    println!("\n  reproit initialized. Next:");
    match platform {
        Platform::Flutter => {
            println!("  1. confirm the app entry in integration_test/journey_explore.dart");
            println!("  2. reproit doctor   then   reproit map");
        }
        Platform::Web => {
            println!("  1. edit reproit.yaml: set app.url to your dev/staging URL");
            println!(
                "  2. (cd <reproit>/runners/web && npm install && npx playwright install chromium)"
            );
            println!("  3. reproit doctor   then   reproit map");
        }
        Platform::Rn => {
            println!(
                "  1. edit reproit.yaml: set app.appiumCaps (app path, deviceName, platformName)"
            );
            println!("  2. (cd <reproit>/runners/rn && npm install); start an Appium server");
            println!("  3. reproit map");
        }
        Platform::Android => {
            println!(
                "  1. edit reproit.yaml: set app.appiumCaps (apk path, appPackage, appActivity)"
            );
            println!(
                "  2. (cd <reproit>/runners/rn && npm install); appium driver install uiautomator2"
            );
            println!("  3. boot an AVD (emulator -avd <name>); start an Appium server");
            println!("  4. reproit map");
        }
    }
    Ok(())
}

fn detect(dir: &Path) -> Option<Platform> {
    if dir.join("pubspec.yaml").exists() {
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
/// regression guard. Everything else under `.reproit/` is local-only, ephemeral
/// run output, recorded videos/screenshots, logs, and the secrets vault, and
/// must never land in git. Paths are relative to `.reproit/`, so `repros/` stays
/// tracked while the rest is ignored.
const REPROIT_GITIGNORE: &str = "\
# The repro suite (repros/) belongs in git: it's your regression guard.
# Everything else here is local-only and must not be committed.
/runs/
/media/
*.vault
*.log
";

/// Write `.reproit/.gitignore` so a `git add .` can't sweep up run artifacts,
/// recorded media, or the secrets vault. Idempotent and non-destructive: if the
/// file already exists (a user may have customized it) we leave it untouched.
fn ensure_gitignore(dir: &Path) -> Result<()> {
    let path = dir.join(".reproit").join(".gitignore");
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
    // Vendor the headless explorer too: the default fuzz/check tier runs
    // `flutter test fuzz_headless_test.dart`, so it must exist or those
    // commands cannot run. The orchestrator resolves this exact filename.
    write(
        &dir.join("integration_test/fuzz_headless_test.dart"),
        &fill(EXPLORER_HEADLESS),
        force,
    )?;
    write(
        &dir.join("integration_test/journey_helpers.dart"),
        HELPERS,
        force,
    )?;
    write(
        &dir.join("test_driver/integration_driver.dart"),
        "import 'package:integration_test/integration_test_driver.dart';\n\nFuture<void> main() => integrationDriver();\n",
        force,
    )?;
    ensure_integration_test_dep(dir)?;
    write(&dir.join("reproit.yaml"), &flutter_config(dir), force)?;
    Ok(())
}

/// Self-heal the FlutterDrive sim tier: vendor reproit's own explorer (the same
/// journey_explore.dart + helpers + driver that `reproit init` lays down, with
/// the app entry inferred from the project) into a project that only had the
/// headless explorer, so `reproit check --record` / `--sim` just works instead
/// of erroring on a file reproit knows how to create. Only writes what's missing,
/// so a configured explorer/driver is never clobbered.
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
    let explorer_path = journeys_dir.join("journey_explore.dart");
    std::fs::write(&explorer_path, explorer)?;
    eprintln!(
        "  vendored {} (reproit's sim explorer)",
        explorer_path.display()
    );
    let helpers_path = journeys_dir.join("journey_helpers.dart");
    if !helpers_path.exists() {
        std::fs::write(&helpers_path, HELPERS)?;
        eprintln!("  vendored {}", helpers_path.display());
    }
    let driver_path = project_dir.join(driver_rel);
    if !driver_path.exists() {
        if let Some(p) = driver_path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(
            &driver_path,
            "import 'package:integration_test/integration_test_driver.dart';\n\nFuture<void> main() => integrationDriver();\n",
        )?;
        eprintln!("  vendored {}", driver_path.display());
    }
    // The flutter-drive tier imports package:integration_test, so it must be a
    // dev dependency or the sim build fails (the headless tier needs only
    // flutter_test, which Flutter projects have by default).
    ensure_integration_test_dep(project_dir)?;
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
    write(&dir.join("reproit.yaml"), WEB_CONFIG, force)?;
    Ok(())
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
            // The widget tester is bound to `t` inside `pumpApp(WidgetTester t)`.
            format!("    await t.pumpWidget(const {w}());"),
        ),
        _ => (
            "// TODO: import your app's root widget, e.g.\n// import 'package:your_app/main.dart';"
                .to_string(),
            "    // TODO: pump your app's root widget, e.g.\n    // await t.pumpWidget(const MyApp());"
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

const FLUTTER_CONFIG: &str = r#"# reproit config (flutter-ios-sim). See the example in the reproit repo for
# the full set of options (reset steps, hooks, visual baselines, llm).
app:
  platform: flutter-ios-sim
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

const WEB_CONFIG: &str = r#"# reproit config (web-playwright). Drives a browser with Playwright; the
# whole map/fuzz/soak/a11y/evidence pipeline is shared with the mobile path.
app:
  platform: web-playwright
  # Path to reproit's web runner directory (run npm install there once).
  webRunnerDir: ../reproit/runners/web
  url: http://localhost:3000      # your dev/staging URL
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
  timeoutSec: 180

evidence:
  outDir: .reproit/runs
  video: true
  composite: false
  screenshotMarker: "SHOOT:"

gate:
  runs: 3

llm:
  provider: codex-cli
"#;

const RN_CONFIG: &str = r#"# reproit config (rn-appium). Drives a React Native app over an Appium
# session; the whole map/fuzz/soak/a11y/evidence pipeline is shared.
app:
  platform: rn-appium
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

const ANDROID_CONFIG: &str = r#"# reproit config (android). Drives a native Android app (Jetpack Compose or
# Android Views) over an Appium UiAutomator2 session. Shares the exact same
# runner and marker protocol as rn-appium and swift-ios; only the caps differ.
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
            "explorer.dart pump needle drifted (template uses `t`, not `tester`)"
        );
        assert!(
            EXPLORER_HEADLESS.contains(IMPORT_NEEDLE),
            "explorer_headless.dart import needle drifted"
        );
        assert!(
            EXPLORER_HEADLESS.contains(PUMP_NEEDLE),
            "explorer_headless.dart pump needle drifted"
        );
    }

    #[test]
    fn init_flutter_fills_the_pump_line_and_vendors_the_headless_explorer() {
        let dir = std::env::temp_dir().join(format!("reproit-init-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("pubspec.yaml"), "name: demo_app\n").unwrap();
        std::fs::write(
            dir.join("lib/main.dart"),
            "void main() => runApp(const DemoApp());\n",
        )
        .unwrap();

        init_flutter(&dir, true).unwrap();

        let sim =
            std::fs::read_to_string(dir.join("integration_test/journey_explore.dart")).unwrap();
        // The pump line was actually filled (not left as the commented stub),
        // and uses `t` (the WidgetTester bound inside pumpApp), not `tester`.
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
            std::fs::read_to_string(dir.join("integration_test/fuzz_headless_test.dart")).unwrap();
        assert!(
            headless.contains("await t.pumpWidget(const DemoApp());"),
            "headless explorer was not vendored/filled"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
