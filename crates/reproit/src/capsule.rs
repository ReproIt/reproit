//! Versioned causal capsules: the framework-neutral input artifact behind
//! `pull`, `run`, fuzz confirmation, and guards.
//!
//! Runners only capture facts. This module owns normalization, privacy,
//! completeness, matching, identity, and durable layout so every platform has
//! one trust contract.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

mod causal_graph;
mod crypto;
mod environment;
mod matching;
mod redaction;
use crypto::{
    capsule_key, decrypt, decrypt_with_key, encrypt, encrypt_with_key, hex_sha256, write_private,
};
pub use environment::{EnvironmentEnvelope, EnvironmentOutcome, EnvironmentTrial};
#[cfg(test)]
pub use matching::exchange_match_key;
pub use matching::json_reductions;
pub(crate) use redaction::redact_backend_event;
pub use redaction::{redact_capsule, redact_exchange};
use reproit_protocol::CausalGraph;

pub const CAPSULE_VERSION: u32 = 2;

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
    #[serde(rename = "causalGraph", alias = "causal_graph")]
    pub causal_graph: CausalGraph,
    #[serde(rename = "environmentEnvelope", alias = "environment_envelope")]
    pub environment_envelope: EnvironmentEnvelope,
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
            causal_graph: CausalGraph::default(),
            environment_envelope: EnvironmentEnvelope::default(),
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
        self.causal_graph = causal_graph::build(self)?;
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
        self.version == CAPSULE_VERSION && self.missing_required_capabilities().is_empty()
    }

    fn validate_integrity(&self) -> Result<()> {
        if self.version != CAPSULE_VERSION {
            bail!(
                "capsule schema {} is unsupported; expected {}",
                self.version,
                CAPSULE_VERSION
            );
        }
        self.causal_graph.validate()?;
        let expected = causal_graph::build(self)?;
        if self.causal_graph != expected {
            bail!("capsule causal graph does not match its executable inputs");
        }
        if self.environment.len() > reproit_protocol::MAX_ENVIRONMENT_TRIALS {
            bail!("capsule environment exceeds the bounded proof capacity");
        }
        self.environment_envelope.validate(&self.environment)?;
        Ok(())
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
        capsule.validate_integrity()?;
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

#[cfg(test)]
mod tests;
