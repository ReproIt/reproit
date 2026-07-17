//! Source/config fingerprinting and map freshness provenance.

use super::persistence::{atomic_write_json, load_existing_snapshot, provenance_path};
use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MapProvenance {
    pub schema: u32,
    #[serde(default)]
    pub map_revision: u64,
    pub source_fingerprint: String,
    #[serde(default)]
    pub source_file_count: usize,
    pub config_fingerprint: String,
    pub reproit_version: String,
    pub generated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(default)]
    pub git_dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MapFreshness {
    Missing,
    Current,
    Stale(Vec<&'static str>),
}

fn ignored_dir(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | ".github"
                | ".reproit"
                | ".dart_tool"
                | ".gradle"
                | ".idea"
                | ".next"
                | ".nuxt"
                | ".svelte-kit"
                | ".venv"
                | "build"
                | "coverage"
                | "dist"
                | "node_modules"
                | "Pods"
                | "target"
                | "vendor"
        )
    )
}

fn relevant_source(path: &Path) -> bool {
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    if matches!(
        name,
        "Cargo.lock"
            | "Cargo.toml"
            | "Gemfile.lock"
            | "Package.resolved"
            | "Podfile"
            | "Podfile.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "pubspec.lock"
            | "yarn.lock"
    ) {
        return true;
    }
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some(
            "c" | "cc"
                | "cpp"
                | "cs"
                | "csproj"
                | "css"
                | "dart"
                | "go"
                | "gradle"
                | "h"
                | "hpp"
                | "html"
                | "java"
                | "js"
                | "json"
                | "jsx"
                | "kt"
                | "kts"
                | "m"
                | "mm"
                | "php"
                | "pbxproj"
                | "plist"
                | "properties"
                | "py"
                | "qml"
                | "rb"
                | "rs"
                | "scss"
                | "sln"
                | "swift"
                | "toml"
                | "ts"
                | "tsx"
                | "vue"
                | "vcxproj"
                | "fsproj"
                | "xaml"
                | "xml"
                | "yaml"
                | "yml"
        )
    )
}

fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            if !ignored_dir(&entry.file_name()) {
                collect_sources(&entry.path(), out)?;
            }
        } else if ty.is_file() && relevant_source(&entry.path()) {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn hash_files(root: &Path, files: &mut [PathBuf]) -> Result<String> {
    files.sort_by(|a, b| {
        a.strip_prefix(root)
            .unwrap_or(a)
            .cmp(b.strip_prefix(root).unwrap_or(b))
    });
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    for path in files {
        let rel = path.strip_prefix(root).unwrap_or(path);
        let file = std::fs::File::open(&*path)?;
        let file_len = file.metadata()?.len();
        let rel = rel.to_string_lossy();
        hash.update((rel.len() as u64).to_le_bytes());
        hash.update(rel.as_bytes());
        hash.update(file_len.to_le_bytes());
        let mut reader = BufReader::new(file);
        let mut read_len = 0_u64;
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            read_len += count as u64;
            hash.update(&buffer[..count]);
        }
        anyhow::ensure!(
            read_len == file_len,
            "{} changed while its source fingerprint was being computed",
            path.display()
        );
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn project_fingerprints(root: &Path) -> Result<(String, String, usize)> {
    let mut source = Vec::new();
    collect_sources(root, &mut source)?;
    source.retain(|p| p != &root.join("reproit.yaml") && p != &root.join(".reproit/reproit.yaml"));
    let source_file_count = source.len();
    let source_fingerprint = hash_files(root, &mut source)?;
    let mut configs: Vec<PathBuf> = [
        root.join("reproit.yaml"),
        root.join(".reproit/reproit.yaml"),
    ]
    .into_iter()
    .filter(|p| p.is_file())
    .collect();
    let config_fingerprint = hash_files(root, &mut configs)?;
    Ok((source_fingerprint, config_fingerprint, source_file_count))
}

fn git_metadata(root: &Path) -> (Option<String>, bool) {
    let commit = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());
    let dirty = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ".",
            ":(exclude).reproit",
        ])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| !o.stdout.is_empty());
    (commit, dirty)
}

pub(crate) fn map_freshness(root: &Path) -> Result<MapFreshness> {
    let Some((map, _visits)) = load_existing_snapshot(root)? else {
        return Ok(MapFreshness::Missing);
    };
    let map_revision = map.revision;
    let old: MapProvenance = match std::fs::read_to_string(provenance_path(root))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => return Ok(MapFreshness::Stale(vec!["missing provenance"])),
    };
    let (source, config, source_file_count) = project_fingerprints(root)?;
    let mut reasons = Vec::new();
    if old.source_fingerprint != source {
        reasons.push("application source changed");
    }
    if old.config_fingerprint != config {
        reasons.push("reproit configuration changed");
    }
    if old.reproit_version != crate::VERSION {
        reasons.push("reproit version changed");
    }
    if old.map_revision != map_revision {
        reasons.push("map snapshot is incomplete");
    }
    if old.source_file_count == 0 || source_file_count == 0 {
        reasons.push("runtime build has no local source identity");
    }
    Ok(if reasons.is_empty() {
        MapFreshness::Current
    } else {
        MapFreshness::Stale(reasons)
    })
}

pub(crate) fn stamp_map(root: &Path, map_revision: u64) -> Result<MapProvenance> {
    let (source_fingerprint, config_fingerprint, source_file_count) = project_fingerprints(root)?;
    let (git_commit, git_dirty) = git_metadata(root);
    let provenance = MapProvenance {
        schema: 1,
        map_revision,
        source_fingerprint,
        source_file_count,
        config_fingerprint,
        reproit_version: crate::VERSION.to_string(),
        generated_at: Utc::now().to_rfc3339(),
        git_commit,
        git_dirty,
    };
    let path = provenance_path(root);
    atomic_write_json(&path, &provenance)?;
    Ok(provenance)
}
