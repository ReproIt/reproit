//! Typed environment requirements and strict corpus enumeration.
//!
//! A guard is required where its environment holds and honestly not
//! applicable elsewhere; suite runs that gate CI enumerate the committed
//! store fail-closed so a malformed guard is an error, never a silent skip.

use super::{repros_dir, Meta};
use anyhow::{Context, Result};
use std::path::Path;

/// A host operating system a guard's replay mechanism can run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostOs {
    Linux,
    Macos,
    Windows,
}

impl HostOs {
    pub fn as_str(self) -> &'static str {
        match self {
            HostOs::Linux => "linux",
            HostOs::Macos => "macos",
            HostOs::Windows => "windows",
        }
    }

    /// The host this binary was built for, or None outside the guard
    /// vocabulary. Compile-time, so domain code stays deterministic.
    pub fn current() -> Option<HostOs> {
        if cfg!(target_os = "linux") {
            Some(HostOs::Linux)
        } else if cfg!(target_os = "macos") {
            Some(HostOs::Macos)
        } else if cfg!(target_os = "windows") {
            Some(HostOs::Windows)
        } else {
            None
        }
    }
}

/// A guard's typed environment requirement. A guard whose requirement does
/// not hold on this host is NOT APPLICABLE here: a suite replay must report
/// it loudly and must never count it as a pass. The vocabulary is a closed
/// enum so a misspelled requirement fails meta parsing instead of silently
/// gating nothing.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Requires {
    /// Hosts the replay mechanism runs on (e.g. an LD_PRELOAD shim is
    /// linux-only). Empty means any host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub os: Vec<HostOs>,
}

impl Requires {
    /// Does this requirement hold on the current host? An unrecognized host
    /// never satisfies an os requirement (fail closed).
    pub fn satisfied_here(&self) -> bool {
        self.os.is_empty() || HostOs::current().is_some_and(|host| self.os.contains(&host))
    }

    /// Human description of the requirement, for not-applicable reporting.
    pub fn describe(&self) -> String {
        let os = self
            .os
            .iter()
            .map(|os| os.as_str())
            .collect::<Vec<_>>()
            .join("|");
        format!("os {os}")
    }
}

/// Runaway backstop for the committed store, far above any real corpus.
const MAX_CORPUS_GUARDS: usize = 1000;

/// Strict, fail-closed enumeration of the committed repro store, for suite
/// runs that gate CI. Unlike `list`, malformation is an error, never a skip:
/// a store directory that is not content-addressed, a missing or unparseable
/// meta.json, or a meta that does not identify its directory would otherwise
/// become a guard that silently stopped guarding. A missing store is an empty
/// corpus, not an error, so projects without keeps still check cleanly.
pub fn load_corpus(root: &Path) -> Result<Vec<Meta>> {
    let store = repros_dir(root);
    let Ok(entries) = std::fs::read_dir(&store) else {
        return Ok(Vec::new());
    };
    let mut directories = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("cannot enumerate {}", store.display()))?
            .path();
        if path.is_dir() {
            directories.push(path);
        }
    }
    if directories.len() > MAX_CORPUS_GUARDS {
        anyhow::bail!("guard corpus exceeds the {MAX_CORPUS_GUARDS} guard bound");
    }
    let mut metas = Vec::new();
    for directory in &directories {
        let name = directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if name.len() != 12 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            anyhow::bail!(
                "{} is not a content-addressed guard directory",
                directory.display()
            );
        }
        let meta_path = directory.join("meta.json");
        let text = std::fs::read_to_string(&meta_path)
            .with_context(|| format!("guard {name} is missing meta.json"))?;
        let meta: Meta = serde_json::from_str(&text)
            .with_context(|| format!("guard {name} has malformed meta.json"))?;
        if meta.id != name {
            anyhow::bail!("guard {name}'s meta.json does not identify its directory");
        }
        metas.push(meta);
    }
    metas.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(metas)
}
