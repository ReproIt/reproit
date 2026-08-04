//! Capsule key management and authenticated encryption.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{bail, Result};
use std::path::Path;

pub(super) fn hex_sha256(bytes: &[u8]) -> String {
    crate::domain::hash::sha256_hex(bytes)
}

/// The capsule key, from `REPROIT_CAPSULE_KEY` when set, else the local file.
///
/// The local key is random and per-machine, which is correct for a candidate
/// capsule nobody else will ever open, and wrong for a guard a team shares: the
/// ciphertext travels but the key does not, so the guard cannot replay on
/// another machine or in CI. `REPROIT_CAPSULE_KEY` (64 hex characters) is the
/// team-held key that makes a shared capsule store replayable. It is read, not
/// written: reproit never persists a key it was handed.
pub(super) fn capsule_key(root: &Path) -> Result<[u8; 32]> {
    // The environment read lives in `retention`, the capsule module's one
    // environment-tuned file, so this stays pure by the domain rule.
    if let Some(key) = super::retention::key_override()? {
        return Ok(key);
    }
    let path = crate::runtime::project_layout::capsule_key_path(root);
    if let Ok(bytes) = std::fs::read(&path) {
        return bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("{} is not a 32-byte capsule key", path.display()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key)
        .map_err(|error| anyhow::anyhow!("generating capsule key: {error}"))?;
    write_private(&path, &key)?;
    Ok(key)
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write as _;
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

pub(super) fn encrypt(root: &Path, plaintext: &[u8]) -> Result<Vec<u8>> {
    encrypt_with_key(&capsule_key(root)?, plaintext)
}

pub(super) fn encrypt_with_key(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|error| anyhow::anyhow!("capsule cipher: {error}"))?;
    let mut nonce = [0_u8; 12];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("generating capsule nonce: {error}"))?;
    let cipher_nonce = Nonce::try_from(nonce.as_slice())
        .map_err(|error| anyhow::anyhow!("capsule nonce: {error}"))?;
    let ciphertext = cipher
        .encrypt(&cipher_nonce, plaintext)
        .map_err(|error| anyhow::anyhow!("encrypting capsule: {error}"))?;
    let mut output = b"RPC1".to_vec();
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

pub(super) fn decrypt(root: &Path, bytes: &[u8]) -> Result<Vec<u8>> {
    decrypt_with_key(&capsule_key(root)?, bytes)
}

pub(super) fn decrypt_with_key(key: &[u8; 32], bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 16 || &bytes[..4] != b"RPC1" {
        bail!("invalid encrypted capsule header");
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|error| anyhow::anyhow!("capsule cipher: {error}"))?;
    let nonce = Nonce::try_from(&bytes[4..16])
        .map_err(|error| anyhow::anyhow!("capsule nonce: {error}"))?;
    cipher
        .decrypt(&nonce, &bytes[16..])
        .map_err(|_| anyhow::anyhow!("capsule authentication failed (wrong key or corrupt data)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::scoped_env::ScopedEnv;

    /// A disposable project root. The crate has no tempfile dev-dependency and
    /// does not need one here: these cases only ever touch the key file.
    fn scratch(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("reproit-capsule-key-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch root");
        root
    }

    fn env(value: &str) -> ScopedEnv {
        ScopedEnv::set(vec![("REPROIT_CAPSULE_KEY".to_string(), value.to_string())])
    }

    /// `REPROIT_CAPSULE_KEY` is process-global, so every case that touches it
    /// lives in ONE test: cargo runs test functions in parallel and two of
    /// these racing would set the key out from under each other.
    #[test]
    fn a_supplied_capsule_key_is_shared_verbatim_refused_when_malformed_and_never_stored() {
        let _environment = super::super::capsule_environment_lock();
        // Used verbatim, and never written: reproit does not own a key it was
        // handed, so it must not leave a copy behind for the next run.
        let dir = scratch("supplied");
        {
            let _guard = env(&"11".repeat(32));
            assert_eq!(capsule_key(dir.as_path()).expect("env key"), [0x11_u8; 32]);
            assert!(
                !crate::runtime::project_layout::capsule_key_path(dir.as_path()).exists(),
                "a supplied key must not be persisted"
            );
        }

        // The portability claim as a test: ciphertext sealed on one machine
        // opens on another that holds the same key and no local key file.
        {
            let sender = scratch("sender");
            let receiver = scratch("receiver");
            let _guard = env(&"ab".repeat(32));
            let sealed = encrypt(sender.as_path(), b"the capsule").expect("encrypt");
            assert_eq!(
                decrypt(receiver.as_path(), &sealed).expect("decrypt elsewhere"),
                b"the capsule"
            );
        }

        // Malformed is refused, never ignored. Falling back to the local key
        // would encrypt under a key the operator did not name, leaving a store
        // that is half openable and gives no sign of it.
        {
            let bad_dir = scratch("malformed");
            for bad in ["not-hex", "abcd", &"zz".repeat(32)] {
                let _guard = env(bad);
                assert!(
                    capsule_key(bad_dir.as_path()).is_err(),
                    "expected {bad:?} to be refused"
                );
            }
        }

        // Unset keeps the existing behavior: a stable per-project local key.
        {
            let local = scratch("local");
            let _guard = ScopedEnv::cleared(&["REPROIT_CAPSULE_KEY"]);
            let first = capsule_key(local.as_path()).expect("local key");
            assert_eq!(
                first,
                capsule_key(local.as_path()).expect("local key again"),
                "the local key must be stable across reads"
            );
        }
    }
}
