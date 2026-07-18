//! Auth: an encrypted credential vault and live TOTP, so journeys can log in
//! without secrets ever touching the repo or the config. The runner is
//! framework-agnostic, so secrets are delivered the same way to every backend:
//! resolved at run time and injected as `REPROIT_SECRET_<ACCOUNT>_<FIELD>` env
//! vars, with TOTP codes computed fresh for the current 30s window.
//!
//! At rest the vault is AES-256-GCM. The key comes from `REPROIT_VAULT_KEY`
//! (a passphrase) when set, otherwise from a machine-local keyfile created
//! 0600 under the user config dir. A random per-vault salt is stored in the
//! file header, so the same passphrase yields a distinct key per vault.
//!
//! Key derivation is recorded in the header so we can evolve it. The on-disk
//! format is:
//!
//!   RMV2: MAGIC | kdf_id(1) | salt(16) | nonce(12) | ciphertext
//!       `kdf_id` records the derivation used:
//!         KDF_SHA256 (0): SHA256(salt || material). Used for machine-keyfile
//!             material (already 32 random bytes, a slow KDF adds nothing).
//!         KDF_ARGON2ID (1): Argon2id(material, salt) with the params below.
//!             Used for REPROIT_VAULT_KEY passphrases (a human secret, so a
//!             single SHA256 would be brute-forceable).
//!
//! On open we dispatch on `kdf_id`, so a vault always decrypts with the same
//! derivation it was written with.

use crate::config::AuthCfg;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC_V2: &[u8; 4] = b"RMV2";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

/// KDF identifiers recorded in the RMV2 header (1 byte).
const KDF_SHA256: u8 = 0; // SHA256(salt || material); for random keyfile material.
const KDF_ARGON2ID: u8 = 1; // Argon2id; for human passphrase material.

/// Argon2id parameters for passphrase-derived keys. Recorded here (not in the
/// header) because they are fixed for the RMV2/KDF_ARGON2ID scheme: any future
/// change gets a new kdf_id so old vaults keep decrypting with the old cost.
/// 19 MiB / 2 passes / 1 lane is the OWASP-recommended Argon2id baseline.
const ARGON2_MEM_KIB: u32 = 19 * 1024;
const ARGON2_ITERS: u32 = 2;
const ARGON2_LANES: u32 = 1;

/// Which source the key material came from, so save() can pick the matching
/// KDF.
enum Material {
    /// REPROIT_VAULT_KEY passphrase (low-entropy human secret -> Argon2id).
    Passphrase(Vec<u8>),
    /// Machine keyfile (32 random bytes -> fast SHA256).
    Keyfile(Vec<u8>),
}

/// The decrypted secret store: opaque name -> secret value.
pub struct Vault {
    path: PathBuf,
    map: BTreeMap<String, String>,
}

impl Vault {
    /// Open the vault at `path`, or an empty one if it does not exist yet.
    pub fn open(path: &Path) -> Result<Vault> {
        if !path.exists() {
            return Ok(Vault {
                path: path.to_path_buf(),
                map: BTreeMap::new(),
            });
        }
        let raw =
            std::fs::read(path).with_context(|| format!("reading vault {}", path.display()))?;
        // Dispatch on the recorded kdf_id, so a vault always decrypts with the
        // exact derivation it was written with.
        let (kdf_id, salt, nonce, ct) = if raw.len() >= 4 && &raw[..4] == MAGIC_V2 {
            if raw.len() < 4 + 1 + SALT_LEN + NONCE_LEN {
                bail!("{} is not a reproit vault (truncated RMV2)", path.display());
            }
            let kdf_id = raw[4];
            let salt = &raw[5..5 + SALT_LEN];
            let nonce = &raw[5 + SALT_LEN..5 + SALT_LEN + NONCE_LEN];
            let ct = &raw[5 + SALT_LEN + NONCE_LEN..];
            (kdf_id, salt, nonce, ct)
        } else {
            bail!("{} is not a reproit vault (bad header)", path.display());
        };
        let key = derive_key(kdf_id, salt)?;
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|error| anyhow::anyhow!("vault cipher: {error}"))?;
        let nonce =
            Nonce::try_from(nonce).map_err(|error| anyhow::anyhow!("vault nonce: {error}"))?;
        let pt = cipher.decrypt(&nonce, ct).map_err(|_| {
            anyhow::anyhow!("vault decrypt failed: wrong REPROIT_VAULT_KEY or keyfile")
        })?;
        let map: BTreeMap<String, String> =
            serde_json::from_slice(&pt).context("vault is corrupt (json)")?;
        Ok(Vault {
            path: path.to_path_buf(),
            map,
        })
    }

    /// Encrypt and write the vault, creating parent dirs. 0600 on unix.
    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).expect("OS RNG");
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).expect("OS RNG");
        // Always write the current RMV2 format. The kdf_id follows the material
        // source: Argon2id for a passphrase, fast SHA256 for the random keyfile.
        let material = key_material()?;
        let kdf_id = match &material {
            Material::Passphrase(_) => KDF_ARGON2ID,
            Material::Keyfile(_) => KDF_SHA256,
        };
        let key = derive_key_from(kdf_id, &salt, &material)?;
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|error| anyhow::anyhow!("vault cipher: {error}"))?;
        let cipher_nonce = Nonce::try_from(nonce.as_slice())
            .map_err(|error| anyhow::anyhow!("vault nonce: {error}"))?;
        let pt = serde_json::to_vec(&self.map)?;
        let ct = cipher
            .encrypt(&cipher_nonce, pt.as_ref())
            .map_err(|_| anyhow::anyhow!("vault encrypt failed"))?;
        let mut out = Vec::with_capacity(MAGIC_V2.len() + 1 + SALT_LEN + NONCE_LEN + ct.len());
        out.extend_from_slice(MAGIC_V2);
        out.push(kdf_id);
        out.extend_from_slice(&salt);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        write_private(&self.path, &out)
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.map.insert(key.to_string(), value.to_string());
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|s| s.as_str())
    }
    pub fn remove(&mut self, key: &str) -> bool {
        self.map.remove(key).is_some()
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.map.keys()
    }
}

/// Resolve the raw key material: the passphrase env var (low entropy, human)
/// or the machine keyfile (32 random bytes). The variant drives which KDF runs.
fn key_material() -> Result<Material> {
    match std::env::var("REPROIT_VAULT_KEY") {
        Ok(p) if !p.is_empty() => Ok(Material::Passphrase(p.into_bytes())),
        _ => Ok(Material::Keyfile(machine_keyfile()?)),
    }
}

/// Derive the 32-byte AES key for `kdf_id`, resolving the material from the
/// current environment. Used on OPEN, where the on-disk header dictates which
/// KDF to run regardless of which source the material happens to come from.
fn derive_key(kdf_id: u8, salt: &[u8]) -> Result<[u8; 32]> {
    derive_key_from(kdf_id, salt, &key_material()?)
}

/// Derive the 32-byte AES key for `kdf_id` from explicit `material`.
fn derive_key_from(kdf_id: u8, salt: &[u8], material: &Material) -> Result<[u8; 32]> {
    let bytes = match material {
        Material::Passphrase(b) | Material::Keyfile(b) => b.as_slice(),
    };
    match kdf_id {
        KDF_SHA256 => Ok(derive_sha256(salt, bytes)),
        KDF_ARGON2ID => derive_argon2id(salt, bytes),
        other => bail!("vault uses an unknown key-derivation id {other}; upgrade reproit"),
    }
}

/// Fast derivation for high-entropy (random keyfile) material:
/// SHA256(salt||material).
fn derive_sha256(salt: &[u8], material: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(salt);
    h.update(material);
    h.finalize().into()
}

/// Memory-hard derivation for low-entropy passphrase material: Argon2id with
/// the recorded params, keyed by the per-vault salt.
fn derive_argon2id(salt: &[u8], material: &[u8]) -> Result<[u8; 32]> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(ARGON2_MEM_KIB, ARGON2_ITERS, ARGON2_LANES, Some(32))
        .map_err(|e| anyhow::anyhow!("invalid argon2 params: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(material, salt, &mut key)
        .map_err(|e| anyhow::anyhow!("argon2 key derivation failed: {e}"))?;
    Ok(key)
}

/// Read (or create) the 32-byte machine keyfile used when no passphrase is set.
fn machine_keyfile() -> Result<Vec<u8>> {
    let path = keyfile_path();
    if let Ok(b) = std::fs::read(&path) {
        if b.len() == 32 {
            return Ok(b);
        }
    }
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("OS RNG");
    write_private(&path, &b)?;
    Ok(b.to_vec())
}

fn keyfile_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        });
    base.join("reproit").join("vault.key")
}

/// Write a file with 0600 perms on unix (best effort elsewhere).
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let mut f =
        std::fs::File::create(path).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = f.metadata()?.permissions();
        perm.set_mode(0o600);
        f.set_permissions(perm).ok();
    }
    f.write_all(bytes)?;
    Ok(())
}

// ---- TOTP (RFC 6238, HMAC-SHA1, 30s step, 6 digits) ----------------------

/// Current 6-digit TOTP code for a base32 secret, or None if it won't decode.
pub fn totp_now(secret_base32: &str) -> Option<String> {
    let key = base32_decode(secret_base32)?;
    let step = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() / 30;
    Some(format!("{:06}", hotp(&key, step) % 1_000_000))
}

fn hotp(key: &[u8], counter: u64) -> u32 {
    use hmac::digest::KeyInit;
    use hmac::{Hmac, Mac};
    use sha1::Sha1;
    let mut mac =
        <Hmac<Sha1> as KeyInit>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let off = (digest[19] & 0x0f) as usize;
    ((u32::from(digest[off]) & 0x7f) << 24)
        | (u32::from(digest[off + 1]) << 16)
        | (u32::from(digest[off + 2]) << 8)
        | u32::from(digest[off + 3])
}

/// RFC 4648 base32 decode (upper/lower, padding and spaces ignored).
fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut nbits = 0u32;
    let mut out = Vec::new();
    for c in s.chars() {
        if c == '=' || c.is_whitespace() {
            continue;
        }
        let v = match c.to_ascii_uppercase() {
            'A'..='Z' => c.to_ascii_uppercase() as u32 - 'A' as u32,
            '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => return None,
        };
        bits = (bits << 5) | v;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    Some(out)
}

// ---- run-time injection --------------------------------------------------

/// Resolve every configured account into env vars for the runner. For each
/// account `alice` with refs into the vault, emits any of:
///   REPROIT_SECRET_ALICE_USERNAME, REPROIT_SECRET_ALICE_EMAIL,
///   REPROIT_SECRET_ALICE_PHONE, REPROIT_SECRET_ALICE_PASSWORD,
///   REPROIT_SECRET_ALICE_TOTP   (a fresh 6-digit code, not the seed),
///   REPROIT_SECRET_ALICE_OTP    (a fixed/manual code from the vault),
///   REPROIT_SECRET_ALICE_STORAGE (a JSON session blob for the login bypass)
/// Missing refs are skipped silently so partial accounts still work. Returns an
/// empty vec when no auth is configured (the common case), so callers can
/// always extend their env with the result.
pub fn secret_env(auth: &AuthCfg, root: &Path) -> Result<Vec<(String, String)>> {
    if auth.accounts.is_empty() {
        return Ok(Vec::new());
    }
    let vault_path = auth
        .vault
        .as_ref()
        .map(|path| root.join(path))
        .unwrap_or_else(|| crate::layout::secrets_vault_path(root));
    let vault = Vault::open(&vault_path)?;
    let mut out = Vec::new();
    for acct in &auth.accounts {
        let prefix = format!("REPROIT_SECRET_{}", env_ident(&acct.name));
        if let Some(u) = &acct.username {
            out.push((format!("{prefix}_USERNAME"), u.clone()));
        }
        if let Some(r) = &acct.username_ref {
            if let Some(v) = vault.get(r) {
                out.push((format!("{prefix}_USERNAME"), v.to_string()));
            }
        }
        if let Some(r) = &acct.email_ref {
            if let Some(v) = vault.get(r) {
                out.push((format!("{prefix}_EMAIL"), v.to_string()));
            }
        }
        if let Some(r) = &acct.phone_ref {
            if let Some(v) = vault.get(r) {
                out.push((format!("{prefix}_PHONE"), v.to_string()));
            }
        }
        if let Some(r) = &acct.password_ref {
            if let Some(v) = vault.get(r) {
                out.push((format!("{prefix}_PASSWORD"), v.to_string()));
            }
        }
        if let Some(r) = &acct.totp_ref {
            if let Some(seed) = vault.get(r) {
                if let Some(code) = totp_now(seed) {
                    out.push((format!("{prefix}_TOTP"), code));
                }
            }
        }
        if let Some(r) = &acct.otp_ref {
            if let Some(v) = vault.get(r) {
                out.push((format!("{prefix}_OTP"), v.to_string()));
            }
        }
        if let Some(r) = &acct.storage_ref {
            if let Some(v) = vault.get(r) {
                out.push((format!("{prefix}_STORAGE"), v.to_string()));
            }
        }
    }
    Ok(out)
}

/// Resolve `${REPROIT_SECRET_*}` placeholders in an action string to their
/// vault values, host-side, so a runner types the real value without ever
/// touching the vault, env, or any framework-specific secret transport.
/// `secrets` is the `secret_env` output. This is what makes the vault
/// framework-agnostic: the only secret-aware code is here in the host, not
/// duplicated per runner language.
pub fn resolve_placeholders(action: &str, secrets: &[(String, String)]) -> String {
    if !action.contains("${") {
        return action.to_string();
    }
    let mut out = action.to_string();
    for (key, value) in secrets {
        out = out.replace(&format!("${{{key}}}"), value);
    }
    out
}

/// The inverse of `resolve_placeholders`: replace any secret VALUE with its
/// `${KEY}` placeholder. Applied as the host captures a runner's log so a
/// resolved secret never persists in `drive-*.log` / evidence, on any
/// framework.
pub fn redact(line: &str, secrets: &[(String, String)]) -> String {
    let mut out = line.to_string();
    for (key, value) in secrets {
        // Skip empties (a blank value would replace everything) and very short
        // values (too collision-prone to redact safely).
        if value.len() >= 3 {
            out = out.replace(value.as_str(), &format!("${{{key}}}"));
        }
    }
    out
}

/// Uppercase, non-alphanumeric -> underscore, for an env var fragment.
fn env_ident(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_matches_rfc6238_known_vector() {
        // RFC 6238 SHA1 test seed "12345678901234567890" -> base32. At T=59s
        // (counter 1) the published code is 287082.
        let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"; // ascii "1234567890" x2
        let key = base32_decode(secret).unwrap();
        assert_eq!(format!("{:06}", hotp(&key, 1) % 1_000_000), "287082");
    }

    // REPROIT_VAULT_KEY is process-global, so the env-touching vault tests must
    // not run concurrently. Serialize them through this mutex.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn unique_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "reproit-vault-test-{}-{}-{:?}",
            std::process::id(),
            tag,
            std::thread::current().id()
        ))
    }

    #[test]
    fn vault_roundtrips_under_a_passphrase() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("REPROIT_VAULT_KEY", "test-passphrase-xyz");
        let dir = unique_dir("pass");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("secrets.vault");
        let mut v = Vault::open(&path).unwrap();
        v.set("alice.password", "hunter2");
        v.save().unwrap();
        // New writes are the Argon2id RMV2 format (magic + kdf_id == 1).
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(&raw[..4], MAGIC_V2);
        assert_eq!(raw[4], KDF_ARGON2ID);
        let v2 = Vault::open(&path).unwrap();
        assert_eq!(v2.get("alice.password"), Some("hunter2"));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("REPROIT_VAULT_KEY");
    }

    #[test]
    fn vault_roundtrips_under_the_machine_keyfile() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("REPROIT_VAULT_KEY");
        let dir = unique_dir("keyfile");
        let _ = std::fs::remove_dir_all(&dir);
        // Point the keyfile at this test's dir so we don't clobber the user's.
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let path = dir.join("secrets.vault");
        let mut v = Vault::open(&path).unwrap();
        v.set("k", "v");
        v.save().unwrap();
        // Keyfile material is already random, so the fast SHA256 KDF is recorded.
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(&raw[..4], MAGIC_V2);
        assert_eq!(raw[4], KDF_SHA256);
        let v2 = Vault::open(&path).unwrap();
        assert_eq!(v2.get("k"), Some("v"));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn env_ident_sanitizes() {
        assert_eq!(env_ident("alice-01"), "ALICE_01");
    }

    #[test]
    fn resolve_then_redact_round_trips() {
        let secrets = vec![
            (
                "REPROIT_SECRET_ALICE_PASSWORD".to_string(),
                "hunter2pw".to_string(),
            ),
            (
                "REPROIT_SECRET_ALICE_USERNAME".to_string(),
                "alice@dev".to_string(),
            ),
        ];
        let action = "type:key:pass=${REPROIT_SECRET_ALICE_PASSWORD}";
        let resolved = resolve_placeholders(action, &secrets);
        assert_eq!(resolved, "type:key:pass=hunter2pw");
        // The captured log of that resolved action must not leak the value.
        let logged = format!("FUZZ:ACT a {resolved}");
        assert_eq!(
            redact(&logged, &secrets),
            "FUZZ:ACT a type:key:pass=${REPROIT_SECRET_ALICE_PASSWORD}"
        );
    }

    #[test]
    fn resolve_is_noop_without_placeholders_and_redact_skips_short_values() {
        let secrets = vec![("REPROIT_SECRET_X_CODE".to_string(), "42".to_string())];
        assert_eq!(resolve_placeholders("tap:key:go", &secrets), "tap:key:go");
        // A 2-char value is too collision-prone to redact (would scrub "42"
        // everywhere).
        assert_eq!(redact("seed 42 items", &secrets), "seed 42 items");
    }
}
