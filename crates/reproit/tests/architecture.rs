//! Cheap architecture ratchets for boundaries that Rust does not encode itself.

use std::path::PathBuf;

fn source(relative: &str) -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(manifest.join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

#[test]
fn process_entry_point_stays_thin() {
    let main = source("src/main.rs");
    let code_lines = main
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("//!") && !line.starts_with("//")
        })
        .count();
    assert!(
        code_lines <= 10,
        "src/main.rs grew to {code_lines} code lines; put application logic in the library"
    );
    assert!(
        main.contains("reproit::startup()"),
        "src/main.rs must delegate to the bounded-stack startup path"
    );
    assert!(
        !main.contains("tokio::main"),
        "src/main.rs must not poll the CLI future on the platform entry stack"
    );
}

#[test]
fn crate_root_stays_declarative() {
    let root = source("src/lib.rs");
    let code_lines = root
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("//!") && !line.starts_with("//")
        })
        .count();
    assert!(
        code_lines <= 80,
        "src/lib.rs grew to {code_lines} code lines; put behavior in a named module"
    );
}

#[test]
fn crate_root_does_not_restore_compatibility_aliases() {
    let root = source("src/lib.rs");
    for namespace in [
        "backends", "commands", "crosscut", "infra", "model", "modes",
    ] {
        let alias = format!("pub(crate) use {namespace}::");
        assert!(
            !root.contains(&alias),
            "src/lib.rs reintroduced the `{namespace}` compatibility aliases; use the owning \
             namespace at call sites"
        );
    }
    assert!(
        !root.contains("pub mod cli;"),
        "the internal CLI parser and context must not become public API"
    );
}

#[test]
fn legacy_source_namespaces_do_not_return() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/backends",
        "src/cli",
        "src/commands",
        "src/crosscut",
        "src/infra",
        "src/model",
        "src/modes",
        "scaffolds",
    ] {
        assert!(
            !manifest.join(relative).exists(),
            "legacy namespace {relative} returned; use the owning architectural layer"
        );
    }
}

#[test]
fn inner_layers_do_not_depend_on_outer_layers() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (layer, forbidden) in [
        ("domain", &["crate::interface", "crate::workflows"][..]),
        ("adapters", &["crate::interface", "crate::workflows"][..]),
        ("interface", &["crate::workflows"][..]),
    ] {
        let mut pending = vec![manifest.join("src").join(layer)];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("read layer directory") {
                let path = entry.expect("read layer entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).expect("read layer source");
                let production = if path.file_name().is_some_and(|name| name == "tests.rs") {
                    ""
                } else {
                    body.split("#[cfg(test)]").next().unwrap_or(&body)
                };
                for dependency in forbidden {
                    assert!(
                        !production.contains(dependency),
                        "{} depends outward through {dependency}",
                        path.display()
                    );
                }
            }
        }
    }
}

#[test]
fn source_tree_uses_real_module_hierarchy() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![src];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let body = std::fs::read_to_string(&path).expect("read Rust source");
                assert!(
                    !body.contains("#[path ="),
                    "{} bypasses the module hierarchy with #[path]",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn production_code_uses_canonical_artifact_layout() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![src];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                // A conventional `tests.rs` module is compiled only through its parent's
                // `#[cfg(test)] mod tests;` declaration, so none of its contents are production.
                if path.file_name().is_some_and(|name| name == "tests.rs") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).expect("read Rust source");
                let production = body.split("#[cfg(test)]").next().unwrap_or(&body);
                for forbidden in [".reproit/findings", ".reproit/tools"] {
                    assert!(
                        !production.contains(forbidden),
                        "{} hard-codes {forbidden}; use runtime/project_layout.rs",
                        path.display()
                    );
                }
            }
        }
    }
}

#[test]
fn source_files_stay_reviewable() {
    const MAX_LINES: usize = 4_000;
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![src];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let body = std::fs::read_to_string(&path).expect("read Rust source");
                let lines = body.lines().count();
                assert!(
                    lines <= MAX_LINES,
                    "{} has {lines} lines; split responsibilities before exceeding {MAX_LINES}",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn responsibility_heavy_modules_stay_split() {
    const MAX_LINES: usize = 1_200;
    for relative in [
        "src/domain/capsule/mod.rs",
        "src/adapters/config/mod.rs",
        "src/interface/mcp/mod.rs",
        "src/adapters/project_scaffold/mod.rs",
    ] {
        let body = source(relative);
        let lines = body.lines().count();
        assert!(
            lines <= MAX_LINES,
            "{relative} has {lines} lines; move the next responsibility into a named submodule"
        );
    }
    let commands = source("src/workflows/mod.rs");
    assert!(
        commands.lines().count() <= 1_000,
        "src/workflows/mod.rs must stay below 1,000 lines; move command workflows into named modules"
    );
    let tui = source("src/adapters/tui/mod.rs");
    let tui_runtime = tui.split("#[cfg(test)]\nmod tests").next().unwrap_or(&tui);
    assert!(
        tui_runtime.lines().count() <= 1_000,
        "src/adapters/tui/mod.rs runtime must stay below 1,000 lines; keep adapters in named modules"
    );
    for relative in [
        "src/domain/capsule/crypto.rs",
        "src/domain/capsule/matching.rs",
        "src/domain/capsule/redaction.rs",
        "src/adapters/config/loader.rs",
        "src/interface/mcp/dispatch.rs",
        "src/workflows/backend_target.rs",
        "src/workflows/check.rs",
        "src/workflows/fuzz_command.rs",
        "src/workflows/proof.rs",
        "src/workflows/create_command.rs",
        "src/workflows/scan_command.rs",
        "src/adapters/tui/capture.rs",
        "src/adapters/tui/fuzz_config.rs",
        "src/adapters/tui/interaction.rs",
        "src/adapters/tui/scenario.rs",
        "src/adapters/tui/session.rs",
    ] {
        assert!(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(relative)
                .is_file(),
            "missing responsibility module {relative}"
        );
    }
}

#[test]
fn flutter_explorer_scaffold_stays_modular() {
    const MAX_ENTRY_LINES: usize = 40;
    const MAX_MODULE_LINES: usize = 1_000;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/scaffolds/flutter");

    for entry in [
        root.join("integration_test/journey_explore.dart"),
        root.join("test/fuzz_headless_test.dart"),
    ] {
        let body = std::fs::read_to_string(&entry).expect("read Flutter explorer entry");
        let lines = body.lines().count();
        assert!(
            lines <= MAX_ENTRY_LINES,
            "{} has {lines} lines; keep application wiring in the entry and behavior in modules",
            entry.display()
        );
        assert!(
            !body.contains("class FuzzCfg") && !body.contains("Snapshot snapshot"),
            "{} duplicates explorer behavior instead of importing the shared library",
            entry.display()
        );
    }

    let modules = root.join("integration_test/reproit_explorer");
    for entry in std::fs::read_dir(modules).expect("read Flutter explorer modules") {
        let path = entry.expect("read Flutter explorer module entry").path();
        if path.extension().is_none_or(|extension| extension != "dart") {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read Flutter explorer module");
        let lines = body.lines().count();
        assert!(
            lines <= MAX_MODULE_LINES,
            "{} has {lines} lines; split the module before exceeding {MAX_MODULE_LINES}",
            path.display()
        );
    }
}
