//! Capsule key management and authenticated encryption.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{bail, Result};
use std::path::Path;

pub(super) fn hex_sha256(bytes: &[u8]) -> String {
    crate::infra::sha256_hex(bytes)
}

pub(super) fn capsule_key(root: &Path) -> Result<[u8; 32]> {
    let path = crate::layout::capsule_key_path(root);
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
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
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
    cipher
        .decrypt(Nonce::from_slice(&bytes[4..16]), &bytes[16..])
        .map_err(|_| anyhow::anyhow!("capsule authentication failed (wrong key or corrupt data)"))
}
