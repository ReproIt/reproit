//! Capsule key lifecycle and store retention.
//!
//! This is the capsule module's ENVIRONMENT-TUNED surface, and the only file
//! under `domain/` exempted from the no-`std::env` rule (see
//! `tests/architecture.rs`). Retention reads its bounds from the environment
//! and ages capsules by wall clock, and the shared key is supplied the same
//! way, so concentrating those reads here keeps the rest of the capsule domain
//! pure rather than scattering the exemption across it.

use super::{capsule_key, decrypt_with_key, encrypt_with_key, write_private};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;

/// The operator-supplied capsule key, from `REPROIT_CAPSULE_KEY`.
///
/// The local key is random and per-machine, which is right for a candidate
/// capsule nobody else opens and wrong for a guard a team shares: the
/// ciphertext travels, the key does not. This is the team-held key that makes
/// a shared capsule store replayable on another machine or in CI. A
/// set-but-malformed value is an error, never a silent fall back to the local
/// key: encrypting under a key the operator did not name leaves a store that
/// is half openable and gives no sign of it.
pub(super) fn key_override() -> Result<Option<[u8; 32]>> {
    let Ok(value) = std::env::var("REPROIT_CAPSULE_KEY") else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let bytes = decode_key(value)
        .ok_or_else(|| anyhow::anyhow!("REPROIT_CAPSULE_KEY must be 64 hexadecimal characters"))?;
    Ok(Some(bytes))
}

/// Retention bounds for ABANDONED candidate capsules: how many to keep and how
/// old one may get. Referenced capsules are pinned regardless, so these bounds
/// never reach a capsule a finding or a kept repro points at.
pub(super) fn bounds() -> (usize, std::time::Duration) {
    let max_count = std::env::var("REPROIT_CAPSULE_MAX_UNREFERENCED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let max_days: u64 = std::env::var("REPROIT_CAPSULE_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(30);
    (max_count, std::time::Duration::from_secs(max_days * 86_400))
}

fn decode_key(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut key = [0_u8; 32];
    for (slot, pair) in key.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

pub(super) fn maybe_rotate_key(root: &Path) -> Result<()> {
    // An operator-supplied key is the operator's to rotate. Rotating here would
    // re-encrypt the store under a fresh LOCAL key, so every other machine
    // holding the shared key would stop being able to open it.
    if std::env::var("REPROIT_CAPSULE_KEY").is_ok_and(|value| !value.trim().is_empty()) {
        return Ok(());
    }
    let path = crate::runtime::project_layout::capsule_key_path(root);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Ok(());
    };
    let days: u64 = std::env::var("REPROIT_CAPSULE_KEY_ROTATION_DAYS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(90);
    if days == 0
        || metadata
            .modified()?
            .elapsed()
            .is_ok_and(|age| age > std::time::Duration::from_secs(days * 86_400))
    {
        rotate_key(root)?;
    }
    Ok(())
}

fn referenced_capsules(root: &Path) -> BTreeSet<String> {
    let mut referenced = BTreeSet::new();
    for parent in [
        crate::runtime::project_layout::findings_dir(root),
        crate::runtime::project_layout::repros_dir(root),
    ] {
        let Ok(entries) = std::fs::read_dir(parent) else {
            continue;
        };
        for entry in entries.flatten() {
            let link = entry.path().join("capsule-id");
            if let Ok(id) = std::fs::read_to_string(link) {
                let id = id.trim();
                if !id.is_empty() {
                    referenced.insert(id.to_string());
                }
            }
        }
    }
    referenced
}

/// Remove only unreferenced encrypted capsules. Findings and kept repros pin
/// their capsule forever; count/age bounds apply solely to abandoned
/// candidates.
pub fn prune_unreferenced(
    root: &Path,
    keep_id: Option<&str>,
    max_count: usize,
    max_age: std::time::Duration,
) -> Result<usize> {
    let capsules = crate::runtime::project_layout::capsules_dir(root);
    let Ok(entries) = std::fs::read_dir(&capsules) else {
        return Ok(0);
    };
    let referenced = referenced_capsules(root);
    let now = std::time::SystemTime::now();
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if keep_id == Some(id.as_str()) || referenced.contains(&id) {
            continue;
        }
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((modified, entry.path()));
    }
    candidates.sort_by_key(|(modified, path)| (*modified, path.clone()));
    let excess = candidates.len().saturating_sub(max_count);
    let mut removed = 0;
    for (index, (modified, path)) in candidates.into_iter().enumerate() {
        let expired = now.duration_since(modified).is_ok_and(|age| age > max_age);
        if index < excess || expired {
            std::fs::remove_dir_all(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Re-encrypt every retained capsule with a fresh random key. Staging finishes
/// before any live artifact changes; backups allow rollback if the key swap
/// fails, so rotation never intentionally leaves a mixed-key store.
pub fn rotate_key(root: &Path) -> Result<usize> {
    let old_key = capsule_key(root)?;
    let capsules_dir = crate::runtime::project_layout::capsules_dir(root);
    let mut staged = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&capsules_dir) {
        for entry in entries {
            let entry = entry?;
            let path = entry.path().join("capsule.enc");
            if !path.is_file() {
                continue;
            }
            let plaintext = decrypt_with_key(&old_key, &std::fs::read(&path)?)?;
            staged.push((path, plaintext));
        }
    }
    let mut new_key = [0u8; 32];
    getrandom::fill(&mut new_key).map_err(|e| anyhow::anyhow!("generating capsule key: {e}"))?;
    for (path, plaintext) in &staged {
        std::fs::write(
            path.with_extension("enc.rotate"),
            encrypt_with_key(&new_key, plaintext)?,
        )?;
    }
    let key_path = crate::runtime::project_layout::capsule_key_path(root);
    let key_new = key_path.with_extension("key.rotate");
    write_private(&key_new, &new_key)?;
    for (path, _) in &staged {
        std::fs::rename(path, path.with_extension("enc.previous"))?;
        std::fs::rename(path.with_extension("enc.rotate"), path)?;
    }
    let key_previous = key_path.with_extension("key.previous");
    let swap = (|| -> Result<()> {
        std::fs::rename(&key_path, &key_previous)?;
        std::fs::rename(&key_new, &key_path)?;
        Ok(())
    })();
    if let Err(error) = swap {
        for (path, _) in &staged {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::rename(path.with_extension("enc.previous"), path);
        }
        let _ = std::fs::rename(&key_previous, &key_path);
        let _ = std::fs::remove_file(&key_new);
        return Err(error);
    }
    for (path, _) in &staged {
        let _ = std::fs::remove_file(path.with_extension("enc.previous"));
    }
    let _ = std::fs::remove_file(key_previous);
    Ok(staged.len())
}
