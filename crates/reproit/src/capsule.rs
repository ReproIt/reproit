//! Versioned causal capsules: the framework-neutral input artifact behind
//! `pull`, `run`, fuzz confirmation, and guards.
//!
//! Runners only capture facts. This module owns normalization, privacy,
//! completeness, matching, identity, and durable layout so every platform has
//! one trust contract.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const CAPSULE_VERSION: u32 = 1;

pub struct PlaintextGuard {
    path: std::path::PathBuf,
}

impl PlaintextGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PlaintextGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    Captured,
    Unsupported,
    Unavailable,
    Redacted,
}

fn capability_rank(status: &CaptureStatus) -> u8 {
    match status {
        CaptureStatus::Captured => 3,
        CaptureStatus::Redacted => 2,
        CaptureStatus::Unavailable => 1,
        CaptureStatus::Unsupported => 0,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capability {
    pub status: CaptureStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Action {
    pub index: u32,
    pub actor: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_sig: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_sig: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Exchange {
    pub id: String,
    pub actor: String,
    #[serde(rename = "actionIndex", alias = "action_index")]
    pub action_index: u32,
    pub ordinal: u32,
    pub protocol: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    #[serde(rename = "requestHeaders", alias = "request_headers")]
    pub request_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "requestBody", alias = "request_body")]
    pub request_body: Option<Value>,
    pub status: u16,
    #[serde(default)]
    #[serde(rename = "responseHeaders", alias = "response_headers")]
    pub response_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "responseBody", alias = "response_body")]
    pub response_body: Option<Value>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingIdentity {
    pub oracle: String,
    pub invariant: String,
    pub kind: String,
    #[serde(default)]
    pub message: String,
    pub frame: String,
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<String>,
}

impl FindingIdentity {
    /// The path-independent identity of the defect itself. Unlike `fnd_...`,
    /// which identifies one discovered and minimized case, this deliberately
    /// excludes the seed, action path, build, machine, and evidence. Production
    /// occurrences carrying the same structured identity therefore join the
    /// prelaunch finding without fuzzy message matching.
    pub fn bug_id(&self) -> String {
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"reproit-structural-bug-v1\n");
        for part in [
            self.oracle.as_str(),
            self.invariant.as_str(),
            self.kind.as_str(),
            self.message.as_str(),
            self.frame.as_str(),
            self.trigger.as_str(),
            self.boundary.as_deref().unwrap_or(""),
        ] {
            hasher.update(part.trim().as_bytes());
            hasher.update(b"\n");
        }
        let digest = hasher.finalize();
        let suffix: String = digest[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("bug_{suffix}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capsule {
    pub version: u32,
    pub id: String,
    pub app: String,
    #[serde(default)]
    pub builds: BTreeMap<String, String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, Capability>,
    pub actions: Vec<Action>,
    #[serde(default)]
    pub exchanges: Vec<Exchange>,
    /// Structural backend observations are evidence and oracle input. A
    /// hermetic web capsule retains their redacted trace-bound envelope in the
    /// matching HTTP response, then routes it through the same validator during
    /// replay. It never applies the recorded mutation to a real datastore.
    #[serde(default)]
    pub backend_events: Vec<crate::model::backend::BackendEvent>,
    pub finding: FindingIdentity,
    #[serde(default)]
    pub redactions: Vec<String>,
}

impl Capsule {
    fn is_backend_finding(&self) -> bool {
        self.finding.oracle == "backend-contract" || self.finding.invariant.starts_with("backend:")
    }

    pub fn new(app: impl Into<String>, finding: FindingIdentity) -> Self {
        Self {
            version: CAPSULE_VERSION,
            id: String::new(),
            app: app.into(),
            builds: BTreeMap::new(),
            environment: BTreeMap::new(),
            capabilities: BTreeMap::new(),
            actions: Vec::new(),
            exchanges: Vec::new(),
            backend_events: Vec::new(),
            finding,
            redactions: Vec::new(),
        }
    }

    pub fn finalize_id(&mut self) -> Result<String> {
        self.version = CAPSULE_VERSION;
        // Action order is the executable schedule. In multi-actor capsules each
        // actor has its own 1-based index, so sorting by index would interleave
        // actors incorrectly and destroy the conductor order.
        self.exchanges
            .sort_by_key(|e| (e.action_index, e.ordinal, e.id.clone()));
        self.id.clear();
        let bytes = serde_json::to_vec(self)?;
        self.id = format!("cap_{}", &hex_sha256(&bytes)[..16]);
        Ok(self.id.clone())
    }

    /// Required capabilities are derived from actual causal inputs. An absent
    /// transport can never silently degrade into a confirmed reproduction.
    pub fn missing_required_capabilities(&self) -> Vec<String> {
        // External-input observation is required even when this particular run
        // emitted zero exchanges. Otherwise an unsupported adapter and a truly
        // network-free path are indistinguishable, allowing a live-backend replay
        // to masquerade as a hermetic reproduction.
        let mut required = BTreeSet::from(["ui_actions", "http"]);
        if self.is_backend_finding() {
            required.insert("backend_effects");
        }
        for exchange in self.exchanges.iter().filter(|e| e.required) {
            required.insert(match exchange.protocol.as_str() {
                "ws" | "wss" => "websocket",
                "sse" => "sse",
                _ => "http",
            });
        }
        required
            .into_iter()
            .filter(|name| {
                self.capabilities
                    .get(*name)
                    .is_none_or(|c| c.status != CaptureStatus::Captured)
            })
            .map(str::to_string)
            .collect()
    }

    pub fn confirmable(&self) -> bool {
        let bootstrap_backend_finding =
            self.is_backend_finding() && !self.backend_events.is_empty();
        self.version == CAPSULE_VERSION
            && (!self.actions.is_empty() || bootstrap_backend_finding)
            && self.missing_required_capabilities().is_empty()
    }

    pub fn missing_required_replay_capabilities(&self) -> Vec<String> {
        // Replay interception is required even for a traffic-free capture. It is
        // what proves a newly introduced/unexpected request will become a
        // CAPSULE:MISS instead of reaching live infrastructure.
        let mut required = BTreeSet::from(["http_replay"]);
        if self.is_backend_finding() {
            required.insert("backend_effects_replay");
        }
        for exchange in self.exchanges.iter().filter(|e| e.required) {
            required.insert(match exchange.protocol.as_str() {
                "ws" | "wss" => "websocket_replay",
                "sse" => "sse_replay",
                _ => "http_replay",
            });
        }
        required
            .into_iter()
            .filter(|name| {
                self.capabilities
                    .get(*name)
                    .is_none_or(|c| c.status != CaptureStatus::Captured)
            })
            .map(str::to_string)
            .collect()
    }

    pub fn persist(&mut self, root: &Path) -> Result<std::path::PathBuf> {
        let id = self.finalize_id()?;
        maybe_rotate_key(root)?;
        let dir = crate::layout::capsule_dir(root, &id);
        std::fs::create_dir_all(&dir)?;
        let plaintext = serde_json::to_vec_pretty(self)?;
        let encrypted = encrypt(root, &plaintext)?;
        let tmp = dir.join("capsule.enc.tmp");
        std::fs::write(&tmp, encrypted)?;
        std::fs::rename(&tmp, dir.join("capsule.enc"))?;
        let _ = std::fs::remove_file(dir.join("capsule.json"));
        let max_count = std::env::var("REPROIT_CAPSULE_MAX_UNREFERENCED")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(200);
        let max_days = std::env::var("REPROIT_CAPSULE_RETENTION_DAYS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(30);
        let _ = prune_unreferenced(
            root,
            Some(&id),
            max_count,
            std::time::Duration::from_secs(max_days * 86_400),
        );
        Ok(dir)
    }

    pub fn load(root: &Path, id: &str) -> Result<Self> {
        let dir = crate::layout::capsule_dir(root, id);
        let encrypted_path = dir.join("capsule.enc");
        let legacy_path = dir.join("capsule.json");
        let raw = if encrypted_path.is_file() {
            decrypt(root, &std::fs::read(&encrypted_path)?)?
        } else {
            std::fs::read(&legacy_path)
                .with_context(|| format!("reading {}", legacy_path.display()))?
        };
        let capsule: Capsule = serde_json::from_slice(&raw)?;
        if capsule.version != CAPSULE_VERSION {
            bail!(
                "capsule `{id}` uses schema {}, but this reproit supports {}",
                capsule.version,
                CAPSULE_VERSION
            );
        }
        Ok(capsule)
    }

    /// Decrypt into ignored, mode-0600 scratch storage for a runner process.
    /// Callers delete this immediately after the run.
    pub fn materialize_plaintext(root: &Path, id: &str) -> Result<PlaintextGuard> {
        let capsule = Self::load(root, id)?;
        let dir = crate::layout::tmp_dir(root).join("capsules");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{id}.json"));
        write_private(&path, &serde_json::to_vec(&capsule)?)?;
        Ok(PlaintextGuard { path })
    }

    pub fn materialize_candidate(&self, root: &Path) -> Result<PlaintextGuard> {
        let bytes = serde_json::to_vec(self)?;
        let dir = crate::layout::tmp_dir(root).join("capsules");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("candidate-{}.json", &hex_sha256(&bytes)[..16]));
        write_private(&path, &bytes)?;
        Ok(PlaintextGuard { path })
    }

    pub fn ingest_network_files(&mut self, run_dir: &Path) -> Result<usize> {
        self.ingest_capability_files(run_dir)?;
        let mut count = 0;
        for entry in std::fs::read_dir(run_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("network-") || !name.ends_with(".jsonl") {
                continue;
            }
            let raw = std::fs::read_to_string(entry.path())?;
            for (line_no, line) in raw.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let exchange: Exchange = serde_json::from_str(line).with_context(|| {
                    format!("parsing {} line {}", entry.path().display(), line_no + 1)
                })?;
                self.exchanges.push(exchange);
                count += 1;
            }
        }
        if count > 0 {
            self.capabilities
                .entry("http".into())
                .or_insert(Capability {
                    status: CaptureStatus::Captured,
                    detail: Some(format!("{count} causal exchange(s)")),
                });
        } else {
            self.capabilities
                .entry("http".into())
                .or_insert(Capability {
                    status: CaptureStatus::Unavailable,
                    detail: Some("runner emitted no causal HTTP exchanges".into()),
                });
        }
        Ok(count)
    }

    pub fn ingest_backend_files(&mut self, run_dir: &Path) -> Result<usize> {
        let mut encoded = BTreeSet::new();
        let mut events = Vec::new();
        for entry in std::fs::read_dir(run_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let path = entry.path();
            let parsed = if name.starts_with("backend-") && name.ends_with(".jsonl") {
                let raw = std::fs::read_to_string(&path)?;
                let mut parsed = Vec::new();
                for (line_no, line) in raw.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    parsed.push(
                        serde_json::from_str::<crate::model::backend::BackendEvent>(line)
                            .with_context(|| {
                                format!("parsing {} line {}", path.display(), line_no + 1)
                            })?,
                    );
                }
                parsed
            } else if name.starts_with("drive-") && name.ends_with(".log") {
                crate::model::backend::parse_events(&std::fs::read_to_string(&path)?)
            } else {
                Vec::new()
            };
            for event in parsed {
                let bytes = serde_json::to_vec(&event)?;
                if encoded.insert(bytes) {
                    events.push(event);
                }
            }
        }
        events.sort_by_key(|event| event.sequence);
        let count = events.len();
        self.backend_events = events;
        self.capabilities.insert(
            "backend_effects".into(),
            Capability {
                status: if count > 0 {
                    CaptureStatus::Captured
                } else {
                    CaptureStatus::Unavailable
                },
                detail: Some(if count > 0 {
                    format!("{count} structural backend event(s)")
                } else {
                    "runner emitted no structural backend events".into()
                }),
            },
        );
        Ok(count)
    }

    pub fn ingest_capability_files(&mut self, run_dir: &Path) -> Result<()> {
        let mut from_files = BTreeMap::<String, Capability>::new();
        for entry in std::fs::read_dir(run_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("capabilities-") || !name.ends_with(".json") {
                continue;
            }
            let capabilities: BTreeMap<String, Capability> =
                serde_json::from_slice(&std::fs::read(entry.path())?)?;
            for (name, capability) in capabilities {
                // A multi-actor capsule is only as complete as its least capable
                // actor. Every runner starts with the same explicit keys, so a
                // single unsupported adapter must not be hidden by another
                // actor reporting captured.
                let replace = from_files.get(&name).is_none_or(|existing| {
                    capability_rank(&capability.status) < capability_rank(&existing.status)
                });
                if replace {
                    from_files.insert(name, capability);
                }
            }
        }
        for (name, capability) in from_files {
            self.capabilities.insert(name, capability);
        }
        Ok(())
    }
}

fn maybe_rotate_key(root: &Path) -> Result<()> {
    let path = crate::layout::capsule_key_path(root);
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
        crate::layout::findings_dir(root),
        crate::layout::repros_dir(root),
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
    let capsules = crate::layout::capsules_dir(root);
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
    let capsules_dir = crate::layout::capsules_dir(root);
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
    let key_path = crate::layout::capsule_key_path(root);
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

#[derive(Debug, Clone)]
pub struct RedactionPolicy {
    pub secret_keys: BTreeSet<String>,
    pub drop_headers: BTreeSet<String>,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self {
            secret_keys: [
                "password",
                "passwd",
                "secret",
                "token",
                "access_token",
                "refresh_token",
                "authorization",
                "cookie",
                "set-cookie",
                "email",
                "phone",
                "idempotencykey",
                "idempotency_key",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            drop_headers: [
                "authorization",
                "cookie",
                "set-cookie",
                "proxy-authorization",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }
}

pub fn redact_capsule(capsule: &mut Capsule, policy: &RedactionPolicy) {
    for exchange in &mut capsule.exchanges {
        redact_exchange(exchange, policy, &mut capsule.redactions);
    }
    for event in &mut capsule.backend_events {
        redact_backend_event(event, policy, &mut capsule.redactions);
    }
    capsule.redactions.sort();
    capsule.redactions.dedup();
}

pub(crate) fn redact_backend_event(
    event: &mut crate::model::backend::BackendEvent,
    policy: &RedactionPolicy,
    manifest: &mut Vec<String>,
) {
    let identity = event.idempotency_key.take().map(|key| {
        if key.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 24 && digest.chars().all(|c| c.is_ascii_hexdigit())
        }) {
            return key;
        }
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(key.as_bytes());
        format!(
            "sha256:{}",
            digest[..12]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    });
    if let Ok(mut value) = serde_json::to_value(&*event) {
        redact_value(&mut value, policy, "$backend", manifest);
        if let Ok(redacted) = serde_json::from_value(value) {
            *event = redacted;
        }
    }
    event.idempotency_key = identity;
}

pub fn redact_exchange(
    exchange: &mut Exchange,
    policy: &RedactionPolicy,
    manifest: &mut Vec<String>,
) {
    redact_headers(&mut exchange.request_headers, policy, manifest);
    redact_headers(&mut exchange.response_headers, policy, manifest);
    if let Some(body) = &mut exchange.request_body {
        redact_value(body, policy, "$request", manifest);
    }
    if let Some(body) = &mut exchange.response_body {
        redact_value(body, policy, "$response", manifest);
    }
}

fn redact_headers(
    headers: &mut BTreeMap<String, String>,
    policy: &RedactionPolicy,
    manifest: &mut Vec<String>,
) {
    let keys: Vec<String> = headers.keys().cloned().collect();
    for key in keys {
        if policy.drop_headers.contains(&key.to_ascii_lowercase()) {
            headers.insert(key.clone(), "<reproit:secret>".into());
            manifest.push(format!("header:{key}"));
        }
    }
}

pub(crate) fn redact_value(
    value: &mut Value,
    policy: &RedactionPolicy,
    path: &str,
    manifest: &mut Vec<String>,
) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                if policy.secret_keys.contains(&key.to_ascii_lowercase()) {
                    *child = typed_placeholder(child);
                    manifest.push(child_path);
                } else {
                    redact_value(child, policy, &child_path, manifest);
                }
            }
        }
        Value::Array(values) => {
            for (i, child) in values.iter_mut().enumerate() {
                redact_value(child, policy, &format!("{path}[{i}]"), manifest);
            }
        }
        _ => {}
    }
}

fn typed_placeholder(value: &Value) -> Value {
    if value.pointer("/$reproit/redacted").and_then(Value::as_bool) == Some(true) {
        return value.clone();
    }
    let (kind, length) = match value {
        Value::Null => ("null", None),
        Value::Bool(_) => ("boolean", None),
        Value::Number(number) if number.is_i64() || number.is_u64() => ("integer", None),
        Value::Number(_) => ("number", None),
        Value::String(value) => ("string", Some(value.chars().count())),
        Value::Array(value) => ("array", Some(value.len())),
        Value::Object(_) => ("object", None),
    };
    serde_json::json!({"$reproit": {
        "redacted": true,
        "type": kind,
        "length": length,
    }})
}

#[cfg(test)]
pub fn normalized_url(raw: &str) -> String {
    let (base, query) = raw.split_once('?').unwrap_or((raw, ""));
    let mut params: Vec<&str> = query.split('&').filter(|p| !p.is_empty()).collect();
    params.sort_unstable();
    if params.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", params.join("&"))
    }
}

#[cfg(test)]
pub fn exchange_match_key(exchange: &Exchange) -> String {
    let request_hash = exchange
        .request_body
        .as_ref()
        .map(|v| hex_sha256(&serde_json::to_vec(v).unwrap_or_default()))
        .unwrap_or_default();
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        exchange.actor,
        exchange.action_index,
        exchange.method.to_ascii_uppercase(),
        normalized_url(&exchange.url),
        request_hash,
        exchange.ordinal
    )
}

/// Deterministic JSON reduction candidates, largest structural removals first.
/// The caller replays each candidate and retains it only for the exact finding.
pub fn json_reductions(value: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    match value {
        Value::Object(map) => {
            for key in map.keys() {
                let mut candidate = map.clone();
                candidate.remove(key);
                out.push(Value::Object(candidate));
            }
            for (key, child) in map {
                for reduced in json_reductions(child) {
                    let mut candidate = map.clone();
                    candidate.insert(key.clone(), reduced);
                    out.push(Value::Object(candidate));
                }
            }
        }
        Value::Array(values) => {
            for i in 0..values.len() {
                let mut candidate = values.clone();
                candidate.remove(i);
                out.push(Value::Array(candidate));
            }
            for (i, child) in values.iter().enumerate() {
                for reduced in json_reductions(child) {
                    let mut candidate = values.clone();
                    candidate[i] = reduced;
                    out.push(Value::Array(candidate));
                }
            }
        }
        Value::String(s) if !s.is_empty() => out.push(Value::String(String::new())),
        Value::Number(_) => out.push(Value::from(0)),
        Value::Bool(true) => out.push(Value::Bool(false)),
        _ => {}
    }
    out
}

/// Greedy joint minimization. `reproduces` must perform a clean replay and
/// return true only for the exact original finding identity. Action removal
/// also removes its causal exchanges and backend events, then reindexes later
/// causal inputs atomically.
#[cfg(test)]
pub fn minimize_exact<F>(capsule: &Capsule, mut reproduces: F) -> Result<Capsule>
where
    F: FnMut(&Capsule) -> bool,
{
    if !reproduces(capsule) {
        bail!("the original capsule does not reproduce its exact finding");
    }
    let mut best = capsule.clone();
    let mut i = 0;
    while i < best.actions.len() {
        let removed_index = best.actions[i].index;
        let mut candidate = best.clone();
        candidate.actions.remove(i);
        candidate
            .exchanges
            .retain(|exchange| exchange.action_index != removed_index);
        candidate
            .backend_events
            .retain(|event| event.action_index != removed_index);
        for action in &mut candidate.actions {
            if action.index > removed_index {
                action.index -= 1;
            }
        }
        for exchange in &mut candidate.exchanges {
            if exchange.action_index > removed_index {
                exchange.action_index -= 1;
            }
        }
        for event in &mut candidate.backend_events {
            if event.action_index > removed_index {
                event.action_index -= 1;
            }
        }
        if !candidate.actions.is_empty() && reproduces(&candidate) {
            best = candidate;
        } else {
            i += 1;
        }
    }

    let mut i = 0;
    while i < best.exchanges.len() {
        let mut candidate = best.clone();
        candidate.exchanges.remove(i);
        if reproduces(&candidate) {
            best = candidate;
        } else {
            i += 1;
        }
    }

    for exchange_index in 0..best.exchanges.len() {
        let Some(original) = best.exchanges[exchange_index].response_body.clone() else {
            continue;
        };
        let mut current = original;
        loop {
            let mut accepted = None;
            for reduced in json_reductions(&current) {
                let mut candidate = best.clone();
                candidate.exchanges[exchange_index].response_body = Some(reduced.clone());
                if reproduces(&candidate) {
                    accepted = Some((candidate, reduced));
                    break;
                }
            }
            let Some((candidate, reduced)) = accepted else {
                break;
            };
            best = candidate;
            current = reduced;
        }
    }
    if !reproduces(&best) {
        bail!("minimized capsule failed its final clean confirmation");
    }
    best.finalize_id()?;
    Ok(best)
}

fn hex_sha256(bytes: &[u8]) -> String {
    crate::infra::sha256_hex(bytes)
}

fn capsule_key(root: &Path) -> Result<[u8; 32]> {
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
    getrandom::fill(&mut key).map_err(|e| anyhow::anyhow!("generating capsule key: {e}"))?;
    write_private(&path, &key)?;
    Ok(key)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write as _;
    opts.open(path)?.write_all(bytes)?;
    Ok(())
}

fn encrypt(root: &Path, plaintext: &[u8]) -> Result<Vec<u8>> {
    let key = capsule_key(root)?;
    encrypt_with_key(&key, plaintext)
}

fn encrypt_with_key(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("capsule cipher: {e}"))?;
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("generating capsule nonce: {e}"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|e| anyhow::anyhow!("encrypting capsule: {e}"))?;
    let mut out = b"RPC1".to_vec();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn decrypt(root: &Path, bytes: &[u8]) -> Result<Vec<u8>> {
    let key = capsule_key(root)?;
    decrypt_with_key(&key, bytes)
}

fn decrypt_with_key(key: &[u8; 32], bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 16 || &bytes[..4] != b"RPC1" {
        bail!("invalid encrypted capsule header");
    }
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("capsule cipher: {e}"))?;
    cipher
        .decrypt(Nonce::from_slice(&bytes[4..16]), &bytes[16..])
        .map_err(|_| anyhow::anyhow!("capsule authentication failed (wrong key or corrupt data)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn finding() -> FindingIdentity {
        FindingIdentity {
            oracle: "crash".into(),
            invariant: "no-exception".into(),
            kind: "TypeError".into(),
            message: "cannot read property".into(),
            frame: "FeedItem.fromJson:42".into(),
            trigger: "key:feed".into(),
            boundary: Some("GET /feed".into()),
        }
    }

    #[test]
    fn structural_bug_identity_ignores_the_replay_path_and_is_field_sensitive() {
        let identity = finding();
        assert_eq!(identity.bug_id(), identity.clone().bug_id());
        assert!(identity.bug_id().starts_with("bug_"));

        let mut other = identity.clone();
        other.trigger.push_str("-other");
        assert_ne!(identity.bug_id(), other.bug_id());
    }

    #[test]
    fn completeness_is_derived_from_required_inputs() {
        let mut c = Capsule::new("app", finding());
        c.actions.push(Action {
            index: 0,
            actor: "a".into(),
            action: "tap:key:feed".into(),
            from_sig: None,
            to_sig: None,
        });
        c.exchanges.push(Exchange {
            id: "n1".into(),
            actor: "a".into(),
            action_index: 0,
            ordinal: 0,
            protocol: "https".into(),
            method: "GET".into(),
            url: "https://x/feed".into(),
            request_headers: BTreeMap::new(),
            request_body: None,
            status: 200,
            response_headers: BTreeMap::new(),
            response_body: Some(json!({"items":[]})),
            required: true,
        });
        c.capabilities.insert(
            "ui_actions".into(),
            Capability {
                status: CaptureStatus::Captured,
                detail: None,
            },
        );
        assert_eq!(c.missing_required_capabilities(), vec!["http"]);
        c.capabilities.insert(
            "http".into(),
            Capability {
                status: CaptureStatus::Captured,
                detail: None,
            },
        );
        assert!(c.confirmable());
    }

    #[test]
    fn bootstrap_backend_finding_is_confirmable_without_ui_actions() {
        let mut c = Capsule::new(
            "app",
            FindingIdentity {
                oracle: "contract".into(),
                invariant: "backend:response-shape".into(),
                kind: "response-shape".into(),
                message: "response omitted account id".into(),
                frame: "getAccount".into(),
                trigger: "bootstrap".into(),
                boundary: None,
            },
        );
        c.backend_events.push(crate::model::backend::BackendEvent {
            sequence: 1,
            trace_id: "trace".into(),
            span_id: "span".into(),
            action_index: 0,
            parent_span_id: None,
            operation: "getAccount".into(),
            build: None,
            config_contract: None,
            actor: None,
            tenant: None,
            idempotency_key: None,
            selections: Vec::new(),
            event: crate::model::backend::BackendEventKind::Start { input: Value::Null },
        });
        for name in ["ui_actions", "http", "backend_effects"] {
            c.capabilities.insert(
                name.into(),
                Capability {
                    status: CaptureStatus::Captured,
                    detail: None,
                },
            );
        }
        assert!(c.actions.is_empty());
        assert!(c.confirmable());
    }

    #[test]
    fn redaction_is_recursive_typed_and_manifested() {
        let mut c = Capsule::new("app", finding());
        c.exchanges.push(Exchange {
            id: "n".into(),
            actor: "a".into(),
            action_index: 0,
            ordinal: 0,
            protocol: "https".into(),
            method: "POST".into(),
            url: "https://x".into(),
            request_headers: BTreeMap::from([("Authorization".into(), "Bearer raw".into())]),
            request_body: Some(json!({"profile":{"email":"a@example.com"},"count":2})),
            status: 200,
            response_headers: BTreeMap::new(),
            response_body: None,
            required: true,
        });
        c.backend_events.push(crate::model::backend::BackendEvent {
            sequence: 1,
            trace_id: "trace".into(),
            span_id: "span".into(),
            action_index: 0,
            parent_span_id: None,
            operation: "createUser".into(),
            build: None,
            config_contract: None,
            actor: Some("a".into()),
            tenant: Some("team".into()),
            idempotency_key: Some("payment-retry-secret".into()),
            selections: Vec::new(),
            event: crate::model::backend::BackendEventKind::Start {
                input: json!({"profile":{"email":"a@example.com"}}),
            },
        });
        redact_capsule(&mut c, &RedactionPolicy::default());
        assert_eq!(
            c.exchanges[0].request_headers["Authorization"],
            "<reproit:secret>"
        );
        assert_eq!(
            c.exchanges[0].request_body.as_ref().unwrap()["profile"]["email"],
            json!({"$reproit":{"redacted":true,"type":"string","length":13}})
        );
        assert!(c.redactions.contains(&"$request.profile.email".into()));
        let crate::model::backend::BackendEventKind::Start { input } = &c.backend_events[0].event
        else {
            panic!("expected start event");
        };
        assert_eq!(
            input["profile"]["email"],
            json!({"$reproit":{"redacted":true,"type":"string","length":13}})
        );
        assert!(c
            .redactions
            .contains(&"$backend.input.profile.email".into()));
        assert_eq!(
            c.backend_events[0].idempotency_key.as_deref(),
            Some("sha256:c5f7b22400db7ee6d27dfbf7")
        );
    }

    #[test]
    fn backend_findings_require_structural_replay_capability() {
        let mut backend = finding();
        backend.oracle = "backend-contract".into();
        let mut capsule = Capsule::new("app", backend);
        capsule.capabilities.insert(
            "http_replay".into(),
            Capability {
                status: CaptureStatus::Captured,
                detail: None,
            },
        );
        assert_eq!(
            capsule.missing_required_replay_capabilities(),
            vec!["backend_effects_replay"]
        );
        capsule.capabilities.insert(
            "backend_effects_replay".into(),
            Capability {
                status: CaptureStatus::Captured,
                detail: None,
            },
        );
        assert!(capsule.missing_required_replay_capabilities().is_empty());
    }

    #[test]
    fn matching_and_reduction_are_deterministic() {
        let mut e = Exchange {
            id: "n".into(),
            actor: "bob".into(),
            action_index: 2,
            ordinal: 0,
            protocol: "https".into(),
            method: "post".into(),
            url: "https://x/p?b=2&a=1".into(),
            request_headers: BTreeMap::new(),
            request_body: Some(json!({"x":1})),
            status: 200,
            response_headers: BTreeMap::new(),
            response_body: None,
            required: true,
        };
        let a = exchange_match_key(&e);
        e.url = "https://x/p?a=1&b=2".into();
        assert_eq!(a, exchange_match_key(&e));
        let reductions = json_reductions(&json!({"items":[{"author":null,"name":"x"}],"page":1}));
        assert!(!reductions.is_empty());
        assert_eq!(
            reductions,
            json_reductions(&json!({"items":[{"author":null,"name":"x"}],"page":1}))
        );
    }

    #[test]
    fn exchange_wire_format_is_canonical_camel_case_and_reads_legacy_snake_case() {
        let exchange = Exchange {
            id: "a-1-0".into(),
            actor: "a".into(),
            action_index: 1,
            ordinal: 0,
            protocol: "https".into(),
            method: "GET".into(),
            url: "https://x.test".into(),
            request_headers: BTreeMap::new(),
            request_body: Some(json!({"q":1})),
            status: 200,
            response_headers: BTreeMap::new(),
            response_body: Some(json!({"ok":true})),
            required: true,
        };
        let value = serde_json::to_value(&exchange).unwrap();
        assert_eq!(value["actionIndex"], 1);
        assert!(value.get("action_index").is_none());
        assert!(value.get("requestHeaders").is_some());
        assert!(value.get("responseBody").is_some());
        let legacy = json!({"id":"a-1-0","actor":"a","action_index":1,"ordinal":0,
            "protocol":"https","method":"GET","url":"https://x.test","request_headers":{},
            "request_body": null,
            "status": 200,
            "response_headers": {},
            "response_body": {"ok": true},
            "required": true
        });
        assert_eq!(
            serde_json::from_value::<Exchange>(legacy)
                .unwrap()
                .action_index,
            1
        );
    }

    #[test]
    fn persisted_id_is_content_addressed_and_round_trips() {
        let root = std::env::temp_dir().join(format!("reproit-capsule-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut c = Capsule::new("app", finding());
        c.actions.push(Action {
            index: 0,
            actor: "a".into(),
            action: "tap:key:feed".into(),
            from_sig: None,
            to_sig: None,
        });
        let dir = c.persist(&root).unwrap();
        let loaded = Capsule::load(&root, &c.id).unwrap();
        assert_eq!(loaded, c);
        assert_eq!(dir, crate::layout::capsule_dir(&root, &c.id));
        assert!(dir.join("capsule.enc").is_file());
        assert!(!dir.join("capsule.json").exists());
        let plaintext_path;
        {
            let guard = Capsule::materialize_plaintext(&root, &c.id).unwrap();
            plaintext_path = guard.path().to_path_buf();
            assert!(plaintext_path.is_file());
        }
        assert!(
            !plaintext_path.exists(),
            "plaintext scratch must delete on drop"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn retention_never_removes_referenced_or_inflight_capsules() {
        let root = std::env::temp_dir().join(format!("reproit-cap-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for id in ["cap_old", "cap_pinned", "cap_current"] {
            std::fs::create_dir_all(crate::layout::capsule_dir(&root, id)).unwrap();
            std::fs::write(
                crate::layout::capsule_dir(&root, id).join("capsule.enc"),
                id,
            )
            .unwrap();
        }
        let finding = root.join(".reproit/findings/fnd");
        std::fs::create_dir_all(&finding).unwrap();
        std::fs::write(finding.join("capsule-id"), "cap_pinned").unwrap();
        assert_eq!(
            prune_unreferenced(&root, Some("cap_current"), 0, std::time::Duration::MAX).unwrap(),
            1
        );
        assert!(!crate::layout::capsule_dir(&root, "cap_old").exists());
        assert!(crate::layout::capsule_dir(&root, "cap_pinned").exists());
        assert!(crate::layout::capsule_dir(&root, "cap_current").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn key_rotation_reencrypts_every_capsule_and_preserves_content() {
        let root = std::env::temp_dir().join(format!("reproit-cap-rotate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut capsule = Capsule::new("app", finding());
        capsule.actions.push(Action {
            index: 1,
            actor: "a".into(),
            action: "tap:key:x".into(),
            from_sig: None,
            to_sig: None,
        });
        capsule.capabilities.insert(
            "ui_actions".into(),
            Capability {
                status: CaptureStatus::Captured,
                detail: None,
            },
        );
        capsule.capabilities.insert(
            "http".into(),
            Capability {
                status: CaptureStatus::Captured,
                detail: None,
            },
        );
        capsule.persist(&root).unwrap();
        let id = capsule.id.clone();
        let before_key = std::fs::read(crate::layout::capsule_key_path(&root)).unwrap();
        assert_eq!(rotate_key(&root).unwrap(), 1);
        let after_key = std::fs::read(crate::layout::capsule_key_path(&root)).unwrap();
        assert_ne!(before_key, after_key);
        assert_eq!(Capsule::load(&root, &id).unwrap(), capsule);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn joint_minimizer_removes_actions_exchanges_and_json_by_exact_contract() {
        let mut c = Capsule::new("app", finding());
        c.actions = vec![
            Action {
                index: 0,
                actor: "a".into(),
                action: "tap:key:noise".into(),
                from_sig: None,
                to_sig: None,
            },
            Action {
                index: 1,
                actor: "a".into(),
                action: "tap:key:feed".into(),
                from_sig: None,
                to_sig: None,
            },
        ];
        let exchange = |id: &str, action_index, body| Exchange {
            id: id.into(),
            actor: "a".into(),
            action_index,
            ordinal: 0,
            protocol: "https".into(),
            method: "GET".into(),
            url: format!("https://x/{id}"),
            request_headers: BTreeMap::new(),
            request_body: None,
            status: 200,
            response_headers: BTreeMap::new(),
            response_body: Some(body),
            required: true,
        };
        c.exchanges = vec![
            exchange("noise", 0, json!({"ok":true})),
            exchange(
                "feed",
                1,
                json!({"items":[{"author":null,"name":"Ada"}],"page":1}),
            ),
        ];
        let reproduces = |candidate: &Capsule| {
            candidate.actions.iter().any(|a| a.action == "tap:key:feed")
                && candidate.exchanges.iter().any(|e| {
                    e.url.ends_with("/feed")
                        && e.response_body
                            .as_ref()
                            .is_some_and(|b| b.to_string().contains("\"author\":null"))
                })
        };
        let shrunk = minimize_exact(&c, reproduces).unwrap();
        assert_eq!(shrunk.actions.len(), 1);
        assert_eq!(shrunk.actions[0].index, 0);
        assert_eq!(shrunk.exchanges.len(), 1);
        assert_eq!(shrunk.exchanges[0].action_index, 0);
        assert_eq!(
            shrunk.exchanges[0].response_body,
            Some(json!({"items":[{"author":null}]}))
        );
    }
}
