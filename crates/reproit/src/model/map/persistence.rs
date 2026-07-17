//! Map file paths, snapshots, and JSON persistence.

use super::Visits;
use crate::config::Config;
use crate::layout;
use crate::model::appmap::AppMap;
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) fn appmap_path(root: &Path) -> PathBuf {
    layout::appmap_path(root)
}

pub(super) fn provenance_path(root: &Path) -> PathBuf {
    appmap_path(root).with_file_name("provenance.json")
}

pub(crate) struct MapSnapshot(Vec<(PathBuf, Option<Vec<u8>>)>);

pub(crate) fn begin_full_rebuild(root: &Path) -> Result<MapSnapshot> {
    let paths = [appmap_path(root), visits_path(root), provenance_path(root)];
    let mut saved = Vec::new();
    for path in paths {
        if path.is_file() {
            saved.push((path.clone(), Some(std::fs::read(&path)?)));
            std::fs::remove_file(path)?;
        } else {
            saved.push((path, None));
        }
    }
    Ok(MapSnapshot(saved))
}

pub(crate) fn restore_map(snapshot: MapSnapshot) -> Result<()> {
    for (path, bytes) in snapshot.0 {
        match bytes {
            Some(bytes) => {
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(path, bytes)?;
            }
            None if path.exists() => std::fs::remove_file(path)?,
            None => {}
        }
    }
    Ok(())
}

fn visits_path(root: &Path) -> PathBuf {
    layout::visits_path(root)
}

pub(crate) fn load_map(root: &Path, cfg: &Config) -> AppMap {
    std::fs::read_to_string(appmap_path(root))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| AppMap {
            app: cfg.app.bundle_id.clone(),
            version: 1,
            states: BTreeMap::new(),
            transitions: Vec::new(),
            invariants: Vec::new(),
            interrupts: Vec::new(),
        })
}

pub(super) fn save_map(root: &Path, map: &AppMap) -> Result<()> {
    let out = appmap_path(root);
    std::fs::create_dir_all(out.parent().unwrap())?;
    std::fs::write(&out, serde_json::to_string_pretty(map)?)?;
    Ok(())
}

pub(crate) fn load_visits(root: &Path) -> Visits {
    std::fs::read_to_string(visits_path(root))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(super) fn save_visits(root: &Path, v: &Visits) -> Result<()> {
    let out = visits_path(root);
    std::fs::create_dir_all(out.parent().unwrap())?;
    std::fs::write(out, serde_json::to_string_pretty(v)?)?;
    Ok(())
}
