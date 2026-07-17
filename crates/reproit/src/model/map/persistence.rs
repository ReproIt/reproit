//! Map file paths, snapshots, and JSON persistence.

use super::Visits;
use crate::config::Config;
use crate::layout;
use crate::model::appmap::AppMap;
use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingSnapshot {
    map: AppMap,
    visits: Visits,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingSnapshotRef<'a> {
    map: &'a AppMap,
    visits: &'a Visits,
}

pub(crate) fn appmap_path(root: &Path) -> PathBuf {
    layout::appmap_path(root)
}

pub(super) fn provenance_path(root: &Path) -> PathBuf {
    appmap_path(root).with_file_name("provenance.json")
}

fn visits_path(root: &Path) -> PathBuf {
    layout::visits_path(root)
}

fn pending_snapshot_path(root: &Path) -> PathBuf {
    appmap_path(root).with_file_name("pending-snapshot.json")
}

pub(super) fn with_map_lock<T>(root: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let path = appmap_path(root).with_file_name("map.lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening map lock {}", path.display()))?;
    FileExt::lock_exclusive(&lock)
        .with_context(|| format!("locking map snapshot {}", path.display()))?;
    let result = recover_pending_unlocked(root).and_then(|()| operation());
    let unlock = FileExt::unlock(&lock)
        .with_context(|| format!("unlocking map snapshot {}", path.display()));
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

pub(crate) fn load_map(root: &Path, cfg: &Config) -> Result<AppMap> {
    with_map_lock(root, || load_map_unlocked(root, cfg))
}

pub(crate) fn load_snapshot(root: &Path, cfg: &Config) -> Result<(AppMap, Visits)> {
    with_map_lock(root, || {
        let map = load_map_unlocked(root, cfg)?;
        let visits = load_visits_unlocked(root, map.revision)?;
        Ok((map, visits))
    })
}

pub(crate) fn load_existing_map(root: &Path) -> Result<Option<AppMap>> {
    with_map_lock(root, || load_existing_map_unlocked(root))
}

pub(crate) fn load_existing_snapshot(root: &Path) -> Result<Option<(AppMap, Visits)>> {
    with_map_lock(root, || {
        let Some(map) = load_existing_map_unlocked(root)? else {
            return Ok(None);
        };
        let visits = load_visits_unlocked(root, map.revision)?;
        Ok(Some((map, visits)))
    })
}

pub(super) fn load_map_unlocked(root: &Path, cfg: &Config) -> Result<AppMap> {
    if let Some(map) = load_existing_map_unlocked(root)? {
        return Ok(map);
    }
    Ok(AppMap::empty(cfg.app.bundle_id.clone()))
}

pub(super) fn load_existing_map_unlocked(root: &Path) -> Result<Option<AppMap>> {
    let path = appmap_path(root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let mut map: AppMap = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "parsing {}; refusing to replace a corrupt map",
            path.display()
        )
    })?;
    if map.schema_version == 1 {
        map.schema_version = crate::model::appmap::APP_MAP_SCHEMA_VERSION;
    }
    validate_map(&map, &path)?;
    Ok(Some(map))
}

pub(super) fn save_map(root: &Path, map: &AppMap) -> Result<()> {
    let out = appmap_path(root);
    atomic_write_json(&out, map)
}

#[cfg(test)]
pub(crate) fn load_visits(root: &Path, map_revision: u64) -> Result<Visits> {
    with_map_lock(root, || load_visits_unlocked(root, map_revision))
}

pub(super) fn load_visits_unlocked(root: &Path, map_revision: u64) -> Result<Visits> {
    let path = visits_path(root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(Visits {
                map_revision,
                ..Visits::default()
            });
        }
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let mut visits: Visits = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "parsing {}; refusing to discard corrupt visits",
            path.display()
        )
    })?;
    if visits.map_revision == 0 {
        visits.map_revision = map_revision;
    } else if visits.map_revision != map_revision {
        anyhow::bail!(
            "{} belongs to map revision {}, but appmap.json is revision {}; refusing a partial \
             snapshot",
            path.display(),
            visits.map_revision,
            map_revision
        );
    }
    Ok(visits)
}

pub(super) fn save_visits(root: &Path, v: &Visits) -> Result<()> {
    let out = visits_path(root);
    atomic_write_json(&out, v)
}

pub(super) fn save_snapshot(root: &Path, map: &AppMap, visits: &mut Visits) -> Result<()> {
    visits.map_revision = map.revision;
    let pending = pending_snapshot_path(root);
    atomic_write_json(&pending, &PendingSnapshotRef { map, visits })?;
    // The recovery journal makes this two-file commit roll-forward safe. A
    // crash after either write is completed from `pending-snapshot.json` on the
    // next locked read instead of exposing a permanently mixed generation.
    save_visits(root, visits)?;
    save_map(root, map)?;
    std::fs::remove_file(&pending)?;
    sync_parent(&pending)
}

fn recover_pending_unlocked(root: &Path) -> Result<()> {
    let pending = pending_snapshot_path(root);
    let bytes = match std::fs::read(&pending) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", pending.display())),
    };
    let snapshot: PendingSnapshot = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing recovery journal {}", pending.display()))?;
    validate_map(&snapshot.map, &pending)?;
    if snapshot.visits.map_revision != snapshot.map.revision {
        anyhow::bail!(
            "{} contains mismatched map and visit revisions",
            pending.display()
        );
    }
    save_visits(root, &snapshot.visits)?;
    save_map(root, &snapshot.map)?;
    std::fs::remove_file(&pending)?;
    sync_parent(&pending)
}

fn validate_map(map: &AppMap, path: &Path) -> Result<()> {
    if map.schema_version != crate::model::appmap::APP_MAP_SCHEMA_VERSION {
        anyhow::bail!(
            "{} uses unsupported app-map schema {} (this build supports {})",
            path.display(),
            map.schema_version,
            crate::model::appmap::APP_MAP_SCHEMA_VERSION
        );
    }
    for transition in &map.transitions {
        if !map.states.contains_key(&transition.from) || !map.states.contains_key(&transition.to) {
            anyhow::bail!(
                "{} contains transition {} -> {} with a missing state",
                path.display(),
                transition.from,
                transition.to
            );
        }
    }
    Ok(())
}

pub(super) fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("map");
    let temp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let result = (|| -> Result<()> {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, value)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        replace_file(&temp, path)?;
        sync_parent(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.with_context(|| format!("atomically writing {}", path.display()))
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    std::fs::rename(source, destination)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "reproit-{name}-{}-{}",
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn pending_snapshot_rolls_forward_after_a_torn_commit() {
        let root = test_root("map-recovery");
        let old_map = AppMap::empty("app".to_string());
        let mut old_visits = Visits::default();
        with_map_lock(&root, || save_snapshot(&root, &old_map, &mut old_visits)).unwrap();

        let mut new_map = AppMap::empty("app".to_string());
        new_map.revision = old_map.revision + 1;
        let new_visits = Visits {
            map_revision: new_map.revision,
            ..Visits::default()
        };
        atomic_write_json(
            &pending_snapshot_path(&root),
            &PendingSnapshotRef {
                map: &new_map,
                visits: &new_visits,
            },
        )
        .unwrap();
        save_visits(&root, &new_visits).unwrap();

        let (map, visits) = load_existing_snapshot(&root).unwrap().unwrap();
        assert_eq!(map.revision, new_map.revision);
        assert_eq!(visits.map_revision, new_map.revision);
        assert!(!pending_snapshot_path(&root).exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn map_lock_serializes_revision_updates() {
        let root = test_root("map-lock");
        let map = AppMap::empty("app".to_string());
        let initial_revision = map.revision;
        let mut visits = Visits::default();
        with_map_lock(&root, || save_snapshot(&root, &map, &mut visits)).unwrap();

        let roots = (0..4).map(|_| root.clone()).collect::<Vec<_>>();
        let threads = roots.into_iter().map(|root| {
            std::thread::spawn(move || {
                with_map_lock(&root, || {
                    let mut map = load_existing_map_unlocked(&root)?.unwrap();
                    let mut visits = load_visits_unlocked(&root, map.revision)?;
                    map.mark_changed();
                    save_snapshot(&root, &map, &mut visits)
                })
                .unwrap();
            })
        });
        for thread in threads {
            thread.join().unwrap();
        }

        let (map, visits) = load_existing_snapshot(&root).unwrap().unwrap();
        assert_eq!(map.revision, initial_revision + 4);
        assert_eq!(visits.map_revision, map.revision);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn atomic_json_write_replaces_an_existing_file() {
        let root = test_root("atomic-replace");
        let path = root.join("value.json");
        atomic_write_json(&path, &serde_json::json!({"value": 1})).unwrap();
        atomic_write_json(&path, &serde_json::json!({"value": 2})).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["value"], 2);
        std::fs::remove_dir_all(root).ok();
    }
}
