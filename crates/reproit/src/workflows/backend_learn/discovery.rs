//! Bounded backend service discovery.
//!
//! Source extraction and service discovery walk different domains. A service's
//! `tests/` directory is not part of that service's shipping source, but a
//! repository named `tests/` can itself contain deployable services. Keeping
//! the policies separate prevents the file-content skip list from hiding whole
//! applications.

use crate::adapters::project_scaffold::backend_detect::{
    detect_backend_framework, is_dotnet_aggregator,
};
use std::path::{Path, PathBuf};

/// Bound the scan for sibling services.
const MAX_SERVICE_SCAN: usize = 64;
/// Bound traversal independently from source-file depth.
const MAX_SERVICE_DEPTH: usize = 3;
/// Build and dependency directories that cannot be first-party service roots.
const SKIP_SERVICE_DIRS: [&str; 6] = ["node_modules", "target", "vendor", "dist", "build", ".git"];
const MAX_AGGREGATOR_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Which subtree implements the service this config describes.
pub enum SourceRoot {
    Scan(PathBuf),
    /// Several services under one root and no `backend.source` to pick one.
    Ambiguous(Vec<String>),
}

pub fn source_root(project_root: &Path, declared: Option<&str>) -> SourceRoot {
    if let Some(declared) = declared {
        return SourceRoot::Scan(project_root.join(declared));
    }
    let siblings = sibling_services(project_root);
    let root_is_service = is_service_root(project_root);
    match (siblings.len(), root_is_service) {
        (0, _) => SourceRoot::Scan(project_root.to_path_buf()),
        (1, false) => SourceRoot::Scan(project_root.join(&siblings[0])),
        _ => SourceRoot::Ambiguous(siblings),
    }
}

/// Descendant directories that independently detect as their own backend.
///
/// A detected service is a leaf: its own subdirectories are source, not more
/// services. Workspace-only manifests are not detected as services and remain
/// traversable.
fn sibling_services(project_root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    descend(project_root, project_root, 0, &mut found);
    found.sort();
    found
}

fn descend(root: &Path, dir: &Path, depth: usize, found: &mut Vec<String>) {
    if depth > MAX_SERVICE_DEPTH || found.len() >= MAX_SERVICE_SCAN {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        !name.starts_with('.') && !SKIP_SERVICE_DIRS.contains(&name)
                    })
        })
        .collect();
    paths.sort();
    for path in paths {
        if found.len() >= MAX_SERVICE_SCAN {
            return;
        }
        if is_service_root(&path) {
            if let Ok(relative) = path.strip_prefix(root) {
                found.push(relative.display().to_string());
            }
            continue;
        }
        descend(root, &path, depth + 1, found);
    }
}

pub(super) fn is_service_root(path: &Path) -> bool {
    detect_backend_framework(path).is_some()
        && !is_workspace_aggregator(path)
        && !is_dotnet_aggregator(path)
}

/// Cargo workspace-only manifests group services but do not serve routes.
///
/// Framework detection intentionally follows workspace members so a workspace
/// can be recognized. Discovery needs the narrower question: whether this
/// directory itself is a service leaf.
fn is_workspace_aggregator(path: &Path) -> bool {
    let manifest = path.join("Cargo.toml");
    let readable = std::fs::metadata(&manifest).ok().is_some_and(|metadata| {
        metadata.is_file() && metadata.len() <= MAX_AGGREGATOR_MANIFEST_BYTES
    });
    if !readable {
        return false;
    }
    let Ok(contents) = std::fs::read_to_string(manifest) else {
        return false;
    };
    let mut workspace = false;
    let mut package = false;
    for line in contents.lines() {
        match line.trim() {
            "[workspace]" => workspace = true,
            "[package]" => package = true,
            _ => {}
        }
    }
    workspace && !package
}
