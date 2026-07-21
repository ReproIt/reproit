//! Reproducibility metadata for the exact local toolchain selected by PATH.

use crate::adapters::config::Config;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

const MAX_LOCK_FILES: usize = 64;
const MAX_LOCK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WALK_DEPTH: usize = 5;
const VERSION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainIdentity {
    pub resolved_executables: BTreeMap<String, String>,
    pub versions: BTreeMap<String, String>,
    pub environment_paths: BTreeMap<String, String>,
    pub dependency_locks: BTreeMap<String, String>,
}

fn compact_output(result: crate::runtime::process::RunResult) -> Option<String> {
    if !result.ok() {
        return None;
    }
    let output = if result.stdout.trim().is_empty() {
        result.stderr
    } else {
        result.stdout
    };
    let compact = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(12)
        .collect::<Vec<_>>()
        .join(" | ");
    (!compact.is_empty()).then_some(compact)
}

async fn record_version(
    identity: &mut ToolchainIdentity,
    name: &str,
    executable: &Path,
    arguments: &[&str],
) {
    identity
        .resolved_executables
        .insert(name.to_string(), executable.to_string_lossy().into_owned());
    let executable = executable.to_string_lossy();
    let result =
        crate::runtime::process::run_timeout(executable.as_ref(), arguments, VERSION_TIMEOUT).await;
    if let Some(version) = compact_output(result) {
        identity.versions.insert(name.to_string(), version);
    }
}

fn lock_name(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(
            "pubspec.lock"
                | "Podfile.lock"
                | "Package.resolved"
                | "gradle.lockfile"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
        )
    )
}

fn ignored_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | ".reproit" | "build" | "node_modules" | "target")
    )
}

fn collect_locks(
    root: &Path,
    directory: &Path,
    depth: usize,
    locks: &mut BTreeMap<String, String>,
) -> Result<()> {
    if depth > MAX_WALK_DEPTH || locks.len() >= MAX_LOCK_FILES || ignored_directory(directory) {
        return Ok(());
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(());
    };
    let mut entries = entries.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if locks.len() >= MAX_LOCK_FILES {
            break;
        }
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_locks(root, &path, depth + 1, locks)?;
        } else if metadata.is_file() && metadata.len() <= MAX_LOCK_BYTES && lock_name(&path) {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            locks.insert(
                relative.to_string_lossy().into_owned(),
                crate::domain::hash::sha256_hex(&std::fs::read(&path)?),
            );
        }
    }
    Ok(())
}

pub async fn collect(root: &Path, cfg: &Config) -> ToolchainIdentity {
    let mut identity = ToolchainIdentity::default();
    for variable in [
        "ANDROID_HOME",
        "ANDROID_SDK_ROOT",
        "DEVELOPER_DIR",
        "FLUTTER_ROOT",
        "JAVA_HOME",
        "PATH",
    ] {
        if let Some(value) = std::env::var_os(variable) {
            identity
                .environment_paths
                .insert(variable.into(), value.to_string_lossy().into_owned());
        }
    }

    if let Some(flutter) = crate::runtime::process::executable_path("flutter") {
        record_version(
            &mut identity,
            "flutter",
            &flutter,
            &["--version", "--machine"],
        )
        .await;
        if let Some(bin) = flutter.parent() {
            let bundled_dart = bin.join(if cfg!(windows) { "dart.exe" } else { "dart" });
            if bundled_dart.is_file() {
                record_version(&mut identity, "dart", &bundled_dart, &["--version"]).await;
            }
        }
    }
    if !identity.resolved_executables.contains_key("dart") {
        if let Some(dart) = crate::runtime::process::executable_path("dart") {
            record_version(&mut identity, "dart", &dart, &["--version"]).await;
        }
    }
    if let Some(xcodebuild) = crate::runtime::process::executable_path("xcodebuild") {
        record_version(&mut identity, "xcode", &xcodebuild, &["-version"]).await;
    }
    if let Some(xcrun) = crate::runtime::process::executable_path("xcrun") {
        record_version(
            &mut identity,
            "simulator_runtimes",
            &xcrun,
            &["simctl", "list", "runtimes", "available"],
        )
        .await;
    }
    if let Some(adb) = crate::runtime::process::executable_path("adb") {
        record_version(&mut identity, "adb", &adb, &["version"]).await;
    }

    let project = root.join(&cfg.app.project_dir);
    let _ = collect_locks(root, &project, 0, &mut identity.dependency_locks);
    identity
}

pub async fn write(run_dir: &Path, root: &Path, cfg: &Config) -> Result<ToolchainIdentity> {
    let identity = collect(root, cfg).await;
    std::fs::write(
        run_dir.join("toolchain.json"),
        serde_json::to_vec_pretty(&identity)?,
    )
    .context("writing toolchain identity")?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_collection_is_bounded_and_ignores_build_outputs() {
        let root = std::env::temp_dir().join(format!(
            "reproit-toolchain-locks-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("ios")).unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("pubspec.lock"), "packages: {}").unwrap();
        std::fs::write(root.join("ios/Podfile.lock"), "PODS: []").unwrap();
        std::fs::write(root.join("build/package-lock.json"), "{}").unwrap();
        let mut locks = BTreeMap::new();
        collect_locks(&root, &root, 0, &mut locks).unwrap();

        assert_eq!(locks.len(), 2);
        assert!(locks.contains_key("pubspec.lock"));
        assert!(locks.contains_key("ios/Podfile.lock"));
        let _ = std::fs::remove_dir_all(root);
    }
}
