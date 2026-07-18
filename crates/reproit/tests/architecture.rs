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
    for namespace in ["backends", "model", "modes"] {
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
                let body = std::fs::read_to_string(&path).expect("read Rust source");
                let production = body.split("#[cfg(test)]").next().unwrap_or(&body);
                for forbidden in [".reproit/findings", ".reproit/tools"] {
                    assert!(
                        !production.contains(forbidden),
                        "{} hard-codes {forbidden}; use layout.rs",
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
    for relative in ["src/capsule.rs", "src/config/mod.rs", "src/mcp.rs"] {
        let body = source(relative);
        let lines = body.lines().count();
        assert!(
            lines <= MAX_LINES,
            "{relative} has {lines} lines; move the next responsibility into a named submodule"
        );
    }
    for relative in [
        "src/capsule/crypto.rs",
        "src/capsule/matching.rs",
        "src/capsule/redaction.rs",
        "src/config/loader.rs",
        "src/mcp/dispatch.rs",
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
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scaffolds/flutter");

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
