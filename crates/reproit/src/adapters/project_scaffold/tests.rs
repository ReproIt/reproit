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
    assert!(EXPLORER.contains(IMPORT_NEEDLE));
    assert!(EXPLORER.contains(PUMP_NEEDLE));
    assert!(EXPLORER_HEADLESS.contains(IMPORT_NEEDLE));
    assert!(EXPLORER_HEADLESS.contains(PUMP_NEEDLE));
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
    assert!(!REPROIT_GITIGNORE.contains("/map/"));
    assert!(!REPROIT_GITIGNORE.contains("/repros/"));
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
    assert!(sim.contains("await t.pumpWidget(const DemoApp());"));
    assert!(!sim.contains(PUMP_NEEDLE));
    assert!(sim.contains("import 'package:demo_app/main.dart';"));

    let headless = std::fs::read_to_string(project.join("test/fuzz_headless_test.dart")).unwrap();
    assert!(headless.contains("await t.pumpWidget(const DemoApp());"));
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
        "reproit_explorer/settling.dart",
        "reproit_explorer/navigation.dart",
        "reproit_explorer/action_execution.dart",
        "reproit_explorer/oracle_collection.dart",
        "reproit_explorer/runner.dart",
    ] {
        assert!(journeys.join(relative).is_file(), "missing {relative}");
    }
    std::fs::remove_dir_all(project).unwrap();
}
