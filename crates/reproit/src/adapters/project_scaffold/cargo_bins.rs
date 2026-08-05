//! Cargo bin-target enumeration for boot recipe inference: which binaries a
//! Rust workspace could serve from, read from manifests only. Split from
//! `backend_detect` to keep that file reviewable; it shares the same
//! bounded, evidence-only discipline.

use super::backend_detect::{cargo_framework, manifest, workspace_members};
use std::path::Path;

/// Bin targets of the packages that declare a server framework, for boot
/// recipe inference. When the framework dependency lives only in library
/// crates that the binaries wrap, every workspace bin is the candidate set.
/// Sorted and deduplicated; the caller decides what one-vs-many means.
pub(crate) fn cargo_server_bins(dir: &Path) -> Vec<String> {
    let Some(root) = manifest(dir, "Cargo.toml") else {
        return Vec::new();
    };
    let mut package_dirs = vec![dir.to_path_buf()];
    package_dirs.extend(workspace_members(&root, dir));
    let mut with_framework = Vec::new();
    let mut all = Vec::new();
    for package_dir in &package_dirs {
        let Some(cargo) = manifest(package_dir, "Cargo.toml") else {
            continue;
        };
        let bins = package_bins(package_dir, &cargo);
        if cargo_framework(&cargo).is_some() {
            with_framework.extend(bins.iter().cloned());
        }
        all.extend(bins);
    }
    let mut bins = if with_framework.is_empty() {
        all
    } else {
        with_framework
    };
    bins.sort();
    bins.dedup();
    bins
}

/// The bin targets one package manifest declares: explicit `[[bin]]` names,
/// the `src/main.rs` default named after the package, and `src/bin/*`
/// autodiscovery. Cargo's full precedence is richer; the union is the honest
/// candidate list for a chooser that verifies before trusting.
fn package_bins(package_dir: &Path, cargo: &str) -> Vec<String> {
    let mut bins = section_names(cargo, "[[bin]]");
    if package_dir.join("src/main.rs").is_file() {
        bins.extend(section_names(cargo, "[package]"));
    }
    if let Ok(entries) = std::fs::read_dir(package_dir.join("src/bin")) {
        for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
            let stem = path.file_stem().and_then(|stem| stem.to_str());
            let is_bin = path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                || (path.is_dir() && path.join("main.rs").is_file());
            if is_bin {
                bins.extend(stem.map(str::to_string));
            }
        }
    }
    bins
}

/// `name = "..."` values inside sections with the given header line.
fn section_names(cargo: &str, header: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in cargo.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == header;
            continue;
        }
        if !inside {
            continue;
        }
        let Some(rest) = line.strip_prefix("name") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']);
        if !value.is_empty() {
            names.push(value.to_string());
            inside = false;
        }
    }
    names
}
