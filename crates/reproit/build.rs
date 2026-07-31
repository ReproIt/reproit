//! Stamp an exact release version or an unmistakable source-build version.
//!
//! Release builds create the manifest's `vX.Y.Z` tag in their isolated checkout
//! before compiling, so they report the plain manifest version. Untagged source
//! builds include the current commit and dirty state. This prevents an old
//! installed release and a fresh source build from both reporting the same
//! version while a new release tag is still being prepared.

use std::process::Command;

fn main() {
    let manifest = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let release_tag = format!("v{manifest}");
    let exact_tags = run(&["git", "tag", "--points-at", "HEAD"]);
    let is_release = exact_tags
        .as_deref()
        .is_some_and(|tags| tags.lines().any(|tag| tag == release_tag));
    let version = if is_release {
        manifest
    } else if let Some(commit) = run(&["git", "rev-parse", "--short=12", "HEAD"]) {
        let dirty = run(&["git", "status", "--porcelain"]).is_some_and(|status| !status.is_empty());
        format!(
            "{}-dev+g{}{}",
            manifest,
            commit,
            if dirty { ".dirty" } else { "" }
        )
    } else {
        manifest
    };

    println!("cargo:rustc-env=REPROIT_VERSION={version}");

    generate_oracles();
    // Re-stamp when HEAD moves, the index changes, or a tag is cut.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rerun-if-changed=../../.git/refs/tags");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");
}

/// Run a command, returning trimmed stdout, or None on failure / empty output.
fn run(args: &[&str]) -> Option<String> {
    let out = Command::new(args[0]).args(&args[1..]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Generate the `Oracle` enum and its `ORACLES` metadata table from
/// `oracle-registry.json`.
///
/// The registry was already the cross-repo contract and the cloud already
/// derived its severity ranking from it, but on the CLI side the dependency
/// ran backwards: the JSON mirrored a hand written enum, and five drift tests
/// existed only to notice when the two disagreed. Generating the mechanical
/// half makes that divergence impossible to express, so adding an oracle is an
/// edit to the registry plus whatever behavior the new category needs.
///
/// Only data is generated. Evaluation logic, the parse arms that carry
/// structure, and `classify`'s precedence rules stay hand written in
/// `src/domain/oracle.rs`, because they are behavior rather than a table.
fn generate_oracles() {
    use std::fmt::Write as _;

    let registry = std::path::Path::new("oracle-registry.json");
    let raw = std::fs::read_to_string(registry).expect("oracle-registry.json is readable");
    let doc: serde_json::Value =
        serde_json::from_str(&raw).expect("oracle-registry.json is valid JSON");

    let ids: Vec<&str> = doc["oracles"]
        .as_array()
        .expect("`oracles` is an array")
        .iter()
        .map(|value| value.as_str().expect("each oracle id is a string"))
        .collect();
    let classification = doc["classification"]
        .as_object()
        .expect("`classification` is an object");
    let stable: std::collections::BTreeSet<&str> = doc["stable_defaults"]
        .as_array()
        .expect("`stable_defaults` is an array")
        .iter()
        .map(|value| value.as_str().expect("each stable id is a string"))
        .collect();

    let mut variants = String::new();
    let mut rows = String::new();
    for id in &ids {
        let entry = classification
            .get(*id)
            .unwrap_or_else(|| panic!("oracle `{id}` has no classification entry"));
        let variant = entry
            .get("variant")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| pascal_case(id));

        if let Some(prose) = entry.get("doc").and_then(serde_json::Value::as_str) {
            for line in wrap(prose, 74) {
                writeln!(variants, "    /// {line}").unwrap();
            }
        }
        writeln!(variants, "    {variant},").unwrap();

        let list = |key: &str| -> String {
            entry
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(|value| format!("{:?}", value.as_str().unwrap_or_default()))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        };
        writeln!(
            rows,
            "    OracleMeta {{ oracle: Oracle::{variant}, id: {id:?}, \
             invariants: &[{}], kinds: &[{}], stable: {} }},",
            list("invariants"),
            list("kinds"),
            stable.contains(id)
        )
        .unwrap();
    }

    let generated = format!(
        "// GENERATED by build.rs from oracle-registry.json. Do not edit.\n\
         // Add an oracle by editing that registry, not this file.\n\
         #[derive(Clone, Copy, PartialEq, Eq, Debug)]\n\
         pub enum Oracle {{\n{variants}}}\n\n\
         /// The oracle metadata table, in registry order.\n\
         pub const ORACLES: &[OracleMeta] = &[\n{rows}];\n"
    );

    let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR is set"))
        .join("oracle_generated.rs");
    std::fs::write(&out, generated).expect("write generated oracle table");
    println!("cargo:rerun-if-changed=oracle-registry.json");
}

/// `backend-server-error` becomes `BackendServerError`. Ids whose Rust name is
/// not this mechanical transform carry an explicit `variant` in the registry.
fn pascal_case(id: &str) -> String {
    id.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Wrap prose onto doc comment lines so the generated file stays readable.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}
