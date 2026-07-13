//! Experimental language-independent backend causal contracts.
//!
//! Static adapters, service schemas, and runtime instrumentation all normalize
//! into this module. Static and inferred facts guide exploration. Only declared
//! or schema-owned contracts paired with a concrete runtime witness can produce
//! a finding.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const EVENT_MARKER: &str = "REPROIT:BACKEND ";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Additional trusted API origins that may receive per-action correlation
    /// headers. The application origin is always included by the web runner.
    #[serde(default)]
    pub origins: Vec<String>,
    #[serde(default)]
    pub schemas: Vec<String>,
    #[serde(default)]
    pub operations: Vec<OperationContract>,
    /// Compiler and language adapters write normalized function summaries
    /// here. They guide generation and slicing but never create findings alone.
    #[serde(default)]
    pub programs: Vec<ProgramSummary>,
}

impl BackendConfig {
    pub fn load_schemas(&mut self, root: &Path) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        for relative in self.schemas.clone() {
            let path = root.join(&relative);
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading backend schema {}", path.display()))?;
            let document: Value = match path.extension().and_then(|v| v.to_str()) {
                Some("yaml" | "yml") => serde_yaml::from_str(&raw)
                    .with_context(|| format!("parsing backend schema {}", path.display()))?,
                _ => serde_json::from_str(&raw)
                    .with_context(|| format!("parsing backend schema {}", path.display()))?,
            };
            for imported in import_service_schema(&document) {
                if let Some(declared) = self.operations.iter_mut().find(|operation| {
                    operation.id == imported.id && operation.authority == Authority::Declared
                }) {
                    if declared.input.is_none() {
                        declared.input = imported.input;
                    }
                    if declared.output.is_none() {
                        declared.output = imported.output;
                    }
                    declared
                        .outputs_by_status
                        .extend(imported.outputs_by_status);
                    declared.success_statuses.extend(imported.success_statuses);
                    declared.success_statuses.sort_unstable();
                    declared.success_statuses.dedup();
                    declared.read_only |= imported.read_only;
                    declared.idempotent |= imported.idempotent;
                } else {
                    self.operations.push(imported);
                }
            }
        }
        let mut seen = BTreeSet::new();
        self.operations
            .retain(|operation| seen.insert((operation.id.clone(), operation.authority)));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Authority {
    #[default]
    Declared,
    Schema,
    Inferred,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationContract {
    pub id: String,
    #[serde(default)]
    pub authority: Authority,
    #[serde(default)]
    pub input: Option<ValueDomain>,
    #[serde(default)]
    pub output: Option<ValueDomain>,
    #[serde(default)]
    pub outputs_by_status: BTreeMap<u16, ValueDomain>,
    #[serde(default)]
    pub success_statuses: Vec<u16>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default)]
    pub tenant_isolated: bool,
    #[serde(default)]
    pub promised_effects: Vec<EffectPattern>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramSummary {
    pub language: String,
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub functions: Vec<FunctionSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FunctionSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub source: Option<String>,
    /// Public operation implemented by this function, when applicable.
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub inputs: Vec<ValueSlot>,
    #[serde(default)]
    pub output: Option<ValueDomain>,
    #[serde(default)]
    pub calls: Vec<String>,
    #[serde(default)]
    pub effects: Vec<StaticEffect>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub ensures: Vec<String>,
    #[serde(default)]
    pub authority: Authority,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValueSlot {
    pub name: String,
    pub domain: ValueDomain,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StaticEffect {
    pub kind: EffectKind,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
}

impl OperationContract {
    fn is_success(&self, returned: &ReturnEvent) -> bool {
        returned.success
            && (self.success_statuses.is_empty()
                || returned
                    .status
                    .is_some_and(|status| self.success_statuses.contains(&status)))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ValueDomain {
    Any,
    Null,
    Boolean,
    Integer {
        #[serde(default)]
        min: Option<i64>,
        #[serde(default)]
        max: Option<i64>,
    },
    /// Canonical ProtoJSON represents 64-bit integer families as decimal
    /// strings. Safe-range JSON integers are also accepted for native adapters;
    /// imprecise IEEE-754-sized numeric evidence is not.
    ProtoInteger64 {
        signed: bool,
    },
    Number,
    String {
        #[serde(default)]
        min_length: Option<usize>,
        #[serde(default)]
        max_length: Option<usize>,
        #[serde(default)]
        pattern: Option<String>,
        #[serde(default)]
        format: Option<String>,
        #[serde(default, rename = "enum")]
        variants: Vec<String>,
    },
    Array {
        items: Box<ValueDomain>,
        #[serde(default)]
        min_items: Option<usize>,
        #[serde(default)]
        max_items: Option<usize>,
        #[serde(default)]
        unique: bool,
    },
    Object {
        #[serde(default)]
        required: BTreeSet<String>,
        #[serde(default)]
        properties: BTreeMap<String, ValueDomain>,
        #[serde(default = "default_true")]
        additional: bool,
    },
    OneOf {
        variants: Vec<ValueDomain>,
    },
    GraphqlAbstract {
        variants: BTreeMap<String, ValueDomain>,
    },
    Literal {
        value: Value,
    },
    Resource {
        resource: String,
    },
}

fn default_true() -> bool {
    true
}

impl ValueDomain {
    pub fn mismatch(&self, value: &Value, path: &str) -> Option<String> {
        if let Some(metadata) = redacted_metadata(value) {
            return self.redacted_mismatch(metadata, path);
        }
        match self {
            Self::Any => None,
            Self::Null => (!value.is_null()).then(|| format!("{path} must be null")),
            Self::Boolean => (!value.is_boolean()).then(|| format!("{path} must be boolean")),
            Self::Integer { min, max } => {
                if let Some(number) = value.as_i64() {
                    if min.is_some_and(|bound| number < bound) {
                        Some(format!("{path} is below its minimum"))
                    } else if max.is_some_and(|bound| number > bound) {
                        Some(format!("{path} is above its maximum"))
                    } else {
                        None
                    }
                } else if let Some(number) = value.as_u64() {
                    if min.is_some_and(|bound| bound > 0 && number < bound as u64) {
                        Some(format!("{path} is below its minimum"))
                    } else if max.is_some_and(|bound| bound < 0 || number > bound as u64) {
                        Some(format!("{path} is above its maximum"))
                    } else {
                        None
                    }
                } else {
                    Some(format!("{path} must be an integer"))
                }
            }
            Self::ProtoInteger64 { signed } => {
                const MAX_SAFE: u64 = 9_007_199_254_740_991;
                let canonical = |text: &str, signed: bool| {
                    let digits = text.strip_prefix('-').unwrap_or(text);
                    !digits.is_empty()
                        && digits.bytes().all(|byte| byte.is_ascii_digit())
                        && (digits == "0" || !digits.starts_with('0'))
                        && (!text.starts_with('-') || (signed && digits != "0"))
                };
                let valid = if let Some(text) = value.as_str() {
                    canonical(text, *signed)
                        && if *signed {
                            text.parse::<i64>().is_ok()
                        } else {
                            text.parse::<u64>().is_ok()
                        }
                } else if *signed {
                    value
                        .as_i64()
                        .is_some_and(|number| number.unsigned_abs() <= MAX_SAFE)
                        || value.as_u64().is_some_and(|number| number <= MAX_SAFE)
                } else {
                    value.as_u64().is_some_and(|number| number <= MAX_SAFE)
                };
                (!valid).then(|| format!("{path} must be an exact 64-bit ProtoJSON integer"))
            }
            Self::Number => (!value.is_number()).then(|| format!("{path} must be a number")),
            Self::String {
                min_length,
                max_length,
                pattern,
                format,
                variants,
            } => {
                let Some(text) = value.as_str() else {
                    return Some(format!("{path} must be a string"));
                };
                let length = text.chars().count();
                if min_length.is_some_and(|bound| length < bound) {
                    return Some(format!("{path} is shorter than its minimum"));
                }
                if max_length.is_some_and(|bound| length > bound) {
                    return Some(format!("{path} is longer than its maximum"));
                }
                if !variants.is_empty() && !variants.iter().any(|variant| variant == text) {
                    return Some(format!("{path} is not an allowed variant"));
                }
                if pattern.as_ref().is_some_and(|pattern| {
                    regex::Regex::new(pattern).is_ok_and(|regex| !regex.is_match(text))
                }) {
                    return Some(format!("{path} does not match its pattern"));
                }
                if format
                    .as_deref()
                    .is_some_and(|format| !matches_format(format, text))
                {
                    return Some(format!(
                        "{path} is not a valid {}",
                        format.as_deref().unwrap_or("string")
                    ));
                }
                None
            }
            Self::Array {
                items,
                min_items,
                max_items,
                unique,
            } => {
                let Some(values) = value.as_array() else {
                    return Some(format!("{path} must be an array"));
                };
                if min_items.is_some_and(|bound| values.len() < bound) {
                    return Some(format!("{path} has too few items"));
                }
                if max_items.is_some_and(|bound| values.len() > bound) {
                    return Some(format!("{path} has too many items"));
                }
                if *unique {
                    let canonical = values.iter().map(canonical_json).collect::<BTreeSet<_>>();
                    if canonical.len() != values.len() {
                        return Some(format!("{path} must contain unique items"));
                    }
                }
                values
                    .iter()
                    .enumerate()
                    .find_map(|(index, value)| items.mismatch(value, &format!("{path}[{index}]")))
            }
            Self::Object {
                required,
                properties,
                additional,
            } => {
                let Some(object) = value.as_object() else {
                    return Some(format!("{path} must be an object"));
                };
                if let Some(missing) = required.iter().find(|name| !object.contains_key(*name)) {
                    return Some(format!("{path}.{missing} is required"));
                }
                if !additional {
                    if let Some(extra) = object.keys().find(|name| !properties.contains_key(*name))
                    {
                        return Some(format!("{path}.{extra} is not declared"));
                    }
                }
                properties.iter().find_map(|(name, domain)| {
                    object
                        .get(name)
                        .and_then(|value| domain.mismatch(value, &format!("{path}.{name}")))
                })
            }
            Self::OneOf { variants } => variants
                .iter()
                .all(|variant| variant.mismatch(value, path).is_some())
                .then(|| format!("{path} does not match any allowed variant")),
            Self::GraphqlAbstract { variants } => {
                let kind = value.get("__typename").and_then(Value::as_str)?;
                variants
                    .get(kind)
                    .and_then(|variant| variant.mismatch(value, path))
            }
            Self::Literal { value: expected } => {
                (value != expected).then(|| format!("{path} does not equal its declared literal"))
            }
            Self::Resource { .. } => (!(value.is_string() || value.is_number()))
                .then(|| format!("{path} must be a resource identifier")),
        }
    }

    fn redacted_mismatch(&self, metadata: RedactedMetadata, path: &str) -> Option<String> {
        let wrong_type = |expected: &str| {
            (metadata.kind != expected).then(|| format!("{path} must be {expected}"))
        };
        match self {
            Self::Any => None,
            Self::Null => wrong_type("null"),
            Self::Boolean => wrong_type("boolean"),
            Self::Integer { .. } => wrong_type("integer"),
            Self::ProtoInteger64 { .. } => (!matches!(metadata.kind, "string" | "integer"))
                .then(|| format!("{path} must be an exact 64-bit ProtoJSON integer")),
            Self::Number => (!matches!(metadata.kind, "integer" | "number"))
                .then(|| format!("{path} must be a number")),
            Self::String {
                min_length,
                max_length,
                ..
            } => {
                if let Some(reason) = wrong_type("string") {
                    return Some(reason);
                }
                if min_length.is_some_and(|minimum| metadata.length.is_none_or(|n| n < minimum)) {
                    Some(format!("{path} is shorter than its minimum"))
                } else if max_length
                    .is_some_and(|maximum| metadata.length.is_none_or(|n| n > maximum))
                {
                    Some(format!("{path} is longer than its maximum"))
                } else {
                    // Pattern, enum, and format require content. A redacted value
                    // proves its type and length only, never those constraints.
                    None
                }
            }
            Self::Array {
                min_items,
                max_items,
                ..
            } => {
                if let Some(reason) = wrong_type("array") {
                    return Some(reason);
                }
                if min_items.is_some_and(|minimum| metadata.length.is_none_or(|n| n < minimum)) {
                    Some(format!("{path} has too few items"))
                } else if max_items
                    .is_some_and(|maximum| metadata.length.is_none_or(|n| n > maximum))
                {
                    Some(format!("{path} has too many items"))
                } else {
                    None
                }
            }
            Self::Object { .. } => wrong_type("object"),
            Self::OneOf { variants } => variants
                .iter()
                .all(|variant| variant.redacted_mismatch(metadata, path).is_some())
                .then(|| format!("{path} does not match any allowed variant")),
            Self::GraphqlAbstract { .. } => wrong_type("object"),
            // The literal value and a resource's exact identity are intentionally
            // unavailable after redaction. Retain only safe type evidence.
            Self::Literal { .. } => None,
            Self::Resource { .. } => (!matches!(metadata.kind, "string" | "integer" | "number"))
                .then(|| format!("{path} must be a resource identifier")),
        }
    }
}

#[derive(Clone, Copy)]
struct RedactedMetadata<'a> {
    kind: &'a str,
    length: Option<usize>,
}

fn redacted_metadata(value: &Value) -> Option<RedactedMetadata<'_>> {
    let metadata = value.get("$reproit")?.as_object()?;
    if metadata.get("redacted").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    Some(RedactedMetadata {
        kind: metadata.get("type")?.as_str()?,
        length: metadata
            .get("length")
            .and_then(Value::as_u64)
            .map(|length| length as usize),
    })
}

fn matches_format(format: &str, value: &str) -> bool {
    match format {
        "uuid" => {
            let parts = value.split('-').map(str::len).collect::<Vec<_>>();
            parts == [8, 4, 4, 4, 12] && value.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
        }
        // JSON Schema treats unknown and implementation-defined formats as
        // annotations. Email syntax is deliberately not approximated here:
        // quoted local parts and internationalized domains make simple checks
        // reject valid addresses, violating the zero-false-positive boundary.
        "email" => true,
        "date-time" => chrono::DateTime::parse_from_rfc3339(value).is_ok(),
        "uri" | "url" => regex::Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:.+$")
            .is_ok_and(|pattern| pattern.is_match(value)),
        _ => true,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectPattern {
    pub kind: EffectKind,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default = "default_one")]
    pub at_least: usize,
    /// Optional upper bound for exactly-once and bounded fanout contracts.
    #[serde(default)]
    pub at_most: Option<usize>,
}

fn default_one() -> usize {
    1
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum EffectKind {
    Read,
    Write,
    Delete,
    Emit,
    Call,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackendEvent {
    pub sequence: u64,
    pub trace_id: String,
    pub span_id: String,
    /// UI action that caused this event. Zero is bootstrap traffic before the
    /// first user action. The web transport fills this automatically.
    #[serde(default)]
    pub action_index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// GraphQL response keys selected by this exact invocation. `schemaPath`
    /// uses schema field names while `responsePath` uses aliases as returned.
    /// Empty for non-GraphQL operations and for adapters without parser proof.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selections: Vec<GraphqlSelection>,
    #[serde(flatten)]
    pub event: BackendEventKind,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphqlSelection {
    pub schema_path: String,
    pub response_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_condition: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BackendEventKind {
    Start {
        #[serde(default)]
        input: Value,
    },
    Return {
        #[serde(default)]
        output: Value,
        #[serde(default)]
        status: Option<u16>,
        #[serde(default = "default_true")]
        success: bool,
        /// True only when the adapter observed every effect for this operation.
        /// Absence-based oracles are disabled when this proof is unavailable.
        #[serde(default, rename = "effectsComplete")]
        effects_complete: bool,
    },
    Effect {
        effect: EffectKind,
        #[serde(default)]
        resource: Option<String>,
        #[serde(default)]
        key: Option<String>,
        #[serde(default, rename = "effectTenant")]
        tenant: Option<String>,
        #[serde(default)]
        event: Option<String>,
        #[serde(default)]
        before: Option<Value>,
        #[serde(default)]
        after: Option<Value>,
        #[serde(default)]
        payload: Option<Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackendViolation {
    pub operation: String,
    pub contract_hash: String,
    pub fingerprint: String,
    pub oracle: String,
    pub reason: String,
    pub trace_id: String,
    pub span_id: String,
    pub action_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FrozenBackendGuard {
    pub operations: Vec<OperationContract>,
    pub fingerprints: BTreeSet<String>,
}

impl FrozenBackendGuard {
    pub fn from_findings(config: &BackendConfig, findings: &[Value]) -> Option<Self> {
        let fingerprints = findings
            .iter()
            .filter(|finding| {
                finding.get("oracle").and_then(Value::as_str) == Some("backend-contract")
            })
            .filter_map(|finding| finding.get("fingerprint").and_then(Value::as_str))
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        if fingerprints.is_empty() {
            return None;
        }
        let ids = findings
            .iter()
            .filter_map(|finding| finding.get("operation").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();
        let operations = config
            .operations
            .iter()
            .filter(|operation| ids.contains(operation.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        (!operations.is_empty()).then_some(Self {
            operations,
            fingerprints,
        })
    }

    pub fn reproduces(&self, log: &str) -> bool {
        let config = BackendConfig {
            enabled: true,
            origins: Vec::new(),
            schemas: Vec::new(),
            operations: self.operations.clone(),
            programs: Vec::new(),
        };
        evaluate(&config, &parse_events(log))
            .iter()
            .any(|violation| self.fingerprints.contains(&violation.fingerprint))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }

    pub fn load(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

#[derive(Default)]
struct Invocation<'a> {
    start: Option<&'a BackendEvent>,
    returned: Option<ReturnEvent<'a>>,
    effects: Vec<EffectEvent<'a>>,
}

struct ReturnEvent<'a> {
    event: &'a BackendEvent,
    output: &'a Value,
    status: Option<u16>,
    success: bool,
    effects_complete: bool,
}

struct EffectEvent<'a> {
    event: &'a BackendEvent,
    effect: EffectKind,
    resource: Option<&'a str>,
    key: Option<&'a str>,
    tenant: Option<&'a str>,
    emitted: Option<&'a str>,
    after: Option<&'a Value>,
}

pub fn parse_events(log: &str) -> Vec<BackendEvent> {
    let mut events = log
        .lines()
        .filter_map(|raw| {
            let line = raw.trim_start_matches("flutter: ").trim();
            let value = line
                .find(EVENT_MARKER)
                .map(|index| &line[index + EVENT_MARKER.len()..])
                .or_else(|| {
                    line.find("FUZZ:BACKEND ")
                        .map(|index| &line[index + "FUZZ:BACKEND ".len()..])
                })?;
            serde_json::from_str::<BackendEvent>(value).ok()
        })
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.sequence);
    events
}

pub fn evaluate(config: &BackendConfig, events: &[BackendEvent]) -> Vec<BackendViolation> {
    if !config.enabled {
        return Vec::new();
    }
    let mut invocations = BTreeMap::<(String, String), Invocation<'_>>::new();
    for event in events {
        let invocation = invocations
            .entry((event.trace_id.clone(), event.span_id.clone()))
            .or_default();
        match &event.event {
            BackendEventKind::Start { .. } => invocation.start = Some(event),
            BackendEventKind::Return {
                output,
                status,
                success,
                effects_complete,
            } => {
                invocation.returned = Some(ReturnEvent {
                    event,
                    output,
                    status: *status,
                    success: *success,
                    effects_complete: *effects_complete,
                })
            }
            BackendEventKind::Effect {
                effect,
                resource,
                key,
                tenant,
                event: emitted,
                after,
                ..
            } => invocation.effects.push(EffectEvent {
                event,
                effect: *effect,
                resource: resource.as_deref(),
                key: key.as_deref(),
                tenant: tenant.as_deref(),
                emitted: emitted.as_deref(),
                after: after.as_ref(),
            }),
        }
    }

    let mut contracts = BTreeMap::new();
    for contract in config
        .operations
        .iter()
        .filter(|contract| contract.authority != Authority::Inferred)
    {
        match contracts.entry(contract.id.as_str()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(contract);
            }
            std::collections::btree_map::Entry::Occupied(mut entry)
                if contract.authority == Authority::Declared
                    && entry.get().authority != Authority::Declared =>
            {
                entry.insert(contract);
            }
            _ => {}
        }
    }
    let mut violations = Vec::new();
    for invocation in invocations.values() {
        let Some(start) = invocation.start else {
            continue;
        };
        let Some(contract) = contracts.get(start.operation.as_str()) else {
            continue;
        };
        let Some(returned) = &invocation.returned else {
            continue;
        };
        if returned.success
            && !contract.success_statuses.is_empty()
            && returned
                .status
                .is_none_or(|status| !contract.success_statuses.contains(&status))
        {
            violations.push(violation(
                contract,
                returned.event,
                "response-status",
                format!(
                    "operation reported successful status {} outside its declared success statuses {:?}",
                    returned
                        .status
                        .map_or_else(|| "missing".into(), |status| status.to_string()),
                    contract.success_statuses
                ),
            ));
            continue;
        }
        if !contract.is_success(returned) {
            continue;
        }
        if let (Some(domain), BackendEventKind::Start { input }) = (&contract.input, &start.event) {
            if let Some(reason) = domain.mismatch(input, "$input") {
                violations.push(violation(
                    contract,
                    start,
                    "accepted-invalid-input",
                    format!("operation accepted input outside its declared domain: {reason}"),
                ));
            }
        }
        let output_domain = returned
            .status
            .and_then(|status| contract.outputs_by_status.get(&status))
            .or(contract.output.as_ref());
        if let Some(domain) = output_domain {
            if let Some(reason) = domain.mismatch(returned.output, "$output") {
                violations.push(violation(
                    contract,
                    returned.event,
                    "response-shape",
                    reason,
                ));
            } else if let Some(reason) =
                selection_mismatch(domain, returned.output, &returned.event.selections)
            {
                violations.push(violation(
                    contract,
                    returned.event,
                    "response-selection",
                    reason,
                ));
            }
        }
        if contract.read_only {
            if let Some(effect) = invocation
                .effects
                .iter()
                .find(|effect| matches!(effect.effect, EffectKind::Write | EffectKind::Delete))
            {
                violations.push(violation(
                    contract,
                    effect.event,
                    "read-only-mutation",
                    format!(
                        "read-only operation mutated {}",
                        effect.resource.unwrap_or("persistent state")
                    ),
                ));
            }
        }
        for promised in &contract.promised_effects {
            let count = invocation
                .effects
                .iter()
                .filter(|effect| effect_matches(effect, promised))
                .count();
            if returned.effects_complete
                && count < promised.at_least
                && !idempotent_group_satisfies(contract, start, promised, &invocations)
            {
                violations.push(violation(
                    contract,
                    returned.event,
                    "missing-effect",
                    format!(
                        "successful operation promised at least {} {:?} effect(s) on {}, but observed {}",
                        promised.at_least,
                        promised.kind,
                        promised.resource.as_deref().or(promised.event.as_deref()).unwrap_or("any resource"),
                        count
                    ),
                ));
            }
            if promised.at_most.is_some_and(|maximum| count > maximum) {
                violations.push(violation(
                    contract,
                    returned.event,
                    "excess-effect",
                    format!(
                        "successful operation allowed at most {} {:?} effect(s) on {}, but observed {}",
                        promised.at_most.unwrap_or_default(),
                        promised.kind,
                        promised.resource.as_deref().or(promised.event.as_deref()).unwrap_or("any resource"),
                        count
                    ),
                ));
            }
        }
        if contract.tenant_isolated {
            if let Some(operation_tenant) = start.tenant.as_deref() {
                if let Some(effect) = invocation.effects.iter().find(|effect| {
                    effect
                        .tenant
                        .is_some_and(|tenant| tenant != operation_tenant)
                }) {
                    violations.push(violation(
                        contract,
                        effect.event,
                        "tenant-isolation",
                        "operation crossed its declared tenant boundary".into(),
                    ));
                }
            }
        }
    }
    evaluate_idempotency(&contracts, &invocations, &mut violations);
    violations.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    violations.dedup_by(|a, b| a.fingerprint == b.fingerprint);
    violations
}

fn selection_mismatch(
    domain: &ValueDomain,
    output: &Value,
    selections: &[GraphqlSelection],
) -> Option<String> {
    for selection in selections {
        let schema = normalized_selection_path(&selection.schema_path)?;
        let response = normalized_selection_path(&selection.response_path)?;
        if schema.len() != response.len() {
            continue;
        }
        if let Some(reason) = selected_path_mismatch(
            domain,
            output,
            &schema,
            &response,
            "$output",
            selection.type_condition.as_deref(),
        ) {
            return Some(reason);
        }
    }
    None
}

fn normalized_selection_path(path: &str) -> Option<Vec<(String, bool)>> {
    let name = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").ok()?;
    let mut out = Vec::new();
    for raw in path.split('.') {
        let (raw, array) = raw
            .strip_suffix("[]")
            .map_or((raw, false), |field| (field, true));
        if !name.is_match(raw) {
            return None;
        }
        out.push((raw.to_string(), array));
    }
    (!out.is_empty()).then_some(out)
}

fn selected_path_mismatch(
    domain: &ValueDomain,
    value: &Value,
    schema: &[(String, bool)],
    response: &[(String, bool)],
    path: &str,
    type_condition: Option<&str>,
) -> Option<String> {
    if value.is_null() && domain.mismatch(value, path).is_none() {
        return None;
    }
    if let Some(condition) = type_condition {
        if graphql_abstract_has_variant(domain, condition)
            && value.get("__typename").and_then(Value::as_str) != Some(condition)
        {
            // A conditional fragment only promises fields for the matching
            // concrete object. Missing or different runtime type evidence is
            // not enough to make a hard selected-field claim.
            return None;
        }
    }
    let domain = concrete_domain(domain, value)?;
    if let ValueDomain::Array { items, .. } = domain {
        let values = value.as_array()?;
        return values.iter().enumerate().find_map(|(index, item)| {
            selected_path_mismatch(
                items,
                item,
                schema,
                response,
                &format!("{path}[{index}]"),
                type_condition,
            )
        });
    }
    let ValueDomain::Object { properties, .. } = domain else {
        return None;
    };
    let ((schema_name, schema_array), schema_rest) = schema.split_first()?;
    let ((response_name, response_array), response_rest) = response.split_first()?;
    if schema_array != response_array {
        return None;
    }
    let field_domain = properties.get(schema_name)?;
    let object = value.as_object()?;
    let Some(field_value) = object.get(response_name) else {
        return Some(format!(
            "{path}.{response_name} was selected by the GraphQL operation but is absent"
        ));
    };
    let field_path = format!("{path}.{response_name}");
    if *schema_array {
        let array_domain = concrete_domain(field_domain, field_value)?;
        let ValueDomain::Array { items, .. } = array_domain else {
            return field_domain.mismatch(field_value, &field_path);
        };
        let Some(values) = field_value.as_array() else {
            return field_domain.mismatch(field_value, &field_path);
        };
        if schema_rest.is_empty() {
            return field_domain.mismatch(field_value, &field_path);
        }
        return values.iter().enumerate().find_map(|(index, item)| {
            selected_path_mismatch(
                items,
                item,
                schema_rest,
                response_rest,
                &format!("{field_path}[{index}]"),
                type_condition,
            )
        });
    }
    if schema_rest.is_empty() {
        field_domain.mismatch(field_value, &field_path)
    } else {
        selected_path_mismatch(
            field_domain,
            field_value,
            schema_rest,
            response_rest,
            &field_path,
            type_condition,
        )
    }
}

fn graphql_abstract_has_variant(domain: &ValueDomain, condition: &str) -> bool {
    match domain {
        ValueDomain::OneOf { variants } => variants
            .iter()
            .any(|variant| graphql_abstract_has_variant(variant, condition)),
        ValueDomain::GraphqlAbstract { variants } => variants.contains_key(condition),
        _ => false,
    }
}

fn concrete_domain<'a>(domain: &'a ValueDomain, value: &Value) -> Option<&'a ValueDomain> {
    match domain {
        ValueDomain::OneOf { variants } => variants
            .iter()
            .find(|variant| {
                !matches!(variant, ValueDomain::Null) && variant.mismatch(value, "$value").is_none()
            })
            .or_else(|| {
                variants
                    .iter()
                    .find(|variant| !matches!(variant, ValueDomain::Null))
            })
            .and_then(|variant| concrete_domain(variant, value)),
        ValueDomain::GraphqlAbstract { variants } => value
            .get("__typename")
            .and_then(Value::as_str)
            .and_then(|kind| variants.get(kind))
            .and_then(|variant| concrete_domain(variant, value)),
        _ => Some(domain),
    }
}

/// A correct idempotent retry commonly returns the original success without
/// repeating its write or event. Judge promised effects across the complete
/// actor, tenant, operation, and key group so safe retries remain clean.
fn idempotent_group_satisfies(
    contract: &OperationContract,
    start: &BackendEvent,
    promised: &EffectPattern,
    invocations: &BTreeMap<(String, String), Invocation<'_>>,
) -> bool {
    if !contract.idempotent || start.idempotency_key.is_none() {
        return false;
    }
    let count = invocations
        .values()
        .filter(|candidate| {
            candidate.start.is_some_and(|other| {
                other.operation == start.operation
                    && other.idempotency_key == start.idempotency_key
                    && other.actor == start.actor
                    && other.tenant == start.tenant
            }) && candidate
                .returned
                .as_ref()
                .is_some_and(|returned| contract.is_success(returned))
        })
        .flat_map(|candidate| candidate.effects.iter())
        .filter(|effect| effect_matches(effect, promised))
        .count();
    count >= promised.at_least
}

fn effect_matches(effect: &EffectEvent<'_>, pattern: &EffectPattern) -> bool {
    effect.effect == pattern.kind
        && pattern
            .resource
            .as_deref()
            .is_none_or(|resource| effect.resource == Some(resource))
        && pattern
            .event
            .as_deref()
            .is_none_or(|event| effect.emitted == Some(event))
}

fn evaluate_idempotency(
    contracts: &BTreeMap<&str, &OperationContract>,
    invocations: &BTreeMap<(String, String), Invocation<'_>>,
    violations: &mut Vec<BackendViolation>,
) {
    let mut groups =
        BTreeMap::<(String, String, Option<String>, Option<String>), Vec<&Invocation<'_>>>::new();
    for invocation in invocations.values() {
        let Some(start) = invocation.start else {
            continue;
        };
        let Some(key) = start.idempotency_key.as_ref() else {
            continue;
        };
        if contracts
            .get(start.operation.as_str())
            .is_some_and(|contract| contract.idempotent)
        {
            groups
                .entry((
                    start.operation.clone(),
                    key.clone(),
                    start.actor.clone(),
                    start.tenant.clone(),
                ))
                .or_default()
                .push(invocation);
        }
    }
    for ((operation, _, _, _), group) in groups {
        let Some(contract) = contracts.get(operation.as_str()) else {
            continue;
        };
        let successful = group
            .into_iter()
            .filter(|invocation| {
                invocation
                    .returned
                    .as_ref()
                    .is_some_and(|r| contract.is_success(r))
            })
            .collect::<Vec<_>>();
        if successful.len() < 2 {
            continue;
        }
        let outputs = successful
            .iter()
            .filter_map(|invocation| invocation.returned.as_ref())
            .map(|returned| {
                format!(
                    "{}:{}",
                    returned.status.unwrap_or_default(),
                    canonical_json(returned.output)
                )
            })
            .collect::<BTreeSet<_>>();
        let mutation_attempts = successful
            .iter()
            .filter(|invocation| !mutation_fingerprint(&invocation.effects).is_empty())
            .count();
        if outputs.len() > 1 || mutation_attempts > 1 {
            let event = successful[1]
                .returned
                .as_ref()
                .expect("filtered return")
                .event;
            violations.push(violation(
                contract,
                event,
                "idempotency",
                "repeating the same idempotency key changed the response or repeated a persistent effect".into(),
            ));
        }
    }
}

fn mutation_fingerprint(effects: &[EffectEvent<'_>]) -> String {
    effects
        .iter()
        .filter(|effect| {
            matches!(
                effect.effect,
                EffectKind::Write | EffectKind::Delete | EffectKind::Emit
            )
        })
        .map(|effect| {
            format!(
                "{:?}:{}:{}:{}:{}",
                effect.effect,
                effect.resource.unwrap_or(""),
                effect.key.unwrap_or(""),
                effect.emitted.unwrap_or(""),
                effect.after.map(canonical_json).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn violation(
    contract: &OperationContract,
    event: &BackendEvent,
    oracle: &str,
    reason: String,
) -> BackendViolation {
    let contract_hash =
        hash(&serde_json::to_vec(contract).expect("contract serialization"))[..16].to_string();
    let identity = format!("{}:{contract_hash}:{oracle}:{reason}", contract.id);
    BackendViolation {
        operation: contract.id.clone(),
        contract_hash,
        fingerprint: hash(identity.as_bytes())[..20].to_string(),
        oracle: oracle.into(),
        reason,
        trace_id: event.trace_id.clone(),
        span_id: event.span_id.clone(),
        action_index: event.action_index,
    }
}

pub fn finding(violation: &BackendViolation) -> Value {
    json!({
        "oracle": "backend-contract",
        "invariant": format!("backend:{}", violation.oracle),
        "kind": violation.oracle,
        "message": violation.reason,
        "operation": violation.operation,
        "contract_hash": violation.contract_hash,
        "fingerprint": violation.fingerprint,
        "trigger": violation.fingerprint,
        "trace_id": violation.trace_id,
        "span_id": violation.span_id,
        "action_index": violation.action_index,
        "frames": [format!("backend:{}", violation.operation)],
    })
}

pub fn write_evidence(
    path: &Path,
    config: &BackendConfig,
    events: &[BackendEvent],
    violations: &[BackendViolation],
) -> std::io::Result<()> {
    if !config.enabled || (events.is_empty() && violations.is_empty()) {
        return Ok(());
    }
    let payload = json!({
        "version": 1,
        "operations": config.operations,
        "graph": build_graph(config, events),
        "events": events,
        "violations": violations,
    });
    std::fs::write(path, serde_json::to_vec_pretty(&payload)?)
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CausalContractGraph {
    pub nodes: BTreeMap<String, GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphNode {
    pub id: String,
    pub kind: GraphNodeKind,
    #[serde(default)]
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GraphNodeKind {
    Operation,
    Function,
    ValueDomain,
    Resource,
    Event,
    Actor,
    Tenant,
    Contract,
    RuntimeObservation,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub relation: GraphRelation,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GraphRelation {
    Implements,
    Calls,
    Consumes,
    Produces,
    Reads,
    Writes,
    Deletes,
    Emits,
    Requires,
    Ensures,
    ObservedAs,
    ActsAs,
    BelongsTo,
    HappensBefore,
}

pub fn build_graph(config: &BackendConfig, events: &[BackendEvent]) -> CausalContractGraph {
    let mut graph = CausalContractGraph::default();
    for contract in &config.operations {
        let operation = format!("operation:{}", contract.id);
        graph.nodes.insert(
            operation.clone(),
            GraphNode {
                id: operation.clone(),
                kind: GraphNodeKind::Operation,
                attributes: BTreeMap::new(),
            },
        );
        let contract_id = format!("contract:{}:{}", contract.id, contract.authority as u8);
        graph.nodes.insert(
            contract_id.clone(),
            GraphNode {
                id: contract_id.clone(),
                kind: GraphNodeKind::Contract,
                attributes: BTreeMap::new(),
            },
        );
        graph.edges.push(GraphEdge {
            from: contract_id,
            to: operation,
            relation: GraphRelation::Ensures,
        });
    }
    for program in &config.programs {
        for function in &program.functions {
            let function_id = format!("function:{}", function.id);
            graph.nodes.insert(
                function_id.clone(),
                GraphNode {
                    id: function_id.clone(),
                    kind: GraphNodeKind::Function,
                    attributes: BTreeMap::from([
                        ("name".into(), Value::String(function.name.clone())),
                        ("language".into(), Value::String(program.language.clone())),
                    ]),
                },
            );
            if let Some(operation) = &function.operation {
                let operation = format!("operation:{operation}");
                graph.nodes.entry(operation.clone()).or_insert(GraphNode {
                    id: operation.clone(),
                    kind: GraphNodeKind::Operation,
                    attributes: BTreeMap::new(),
                });
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: operation,
                    relation: GraphRelation::Implements,
                });
            }
            for call in &function.calls {
                let callee = format!("function:{call}");
                graph.nodes.entry(callee.clone()).or_insert(GraphNode {
                    id: callee.clone(),
                    kind: GraphNodeKind::Function,
                    attributes: BTreeMap::new(),
                });
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: callee,
                    relation: GraphRelation::Calls,
                });
            }
            for input in &function.inputs {
                let domain = format!("domain:{}:input:{}", function.id, input.name);
                graph.nodes.insert(
                    domain.clone(),
                    GraphNode {
                        id: domain.clone(),
                        kind: GraphNodeKind::ValueDomain,
                        attributes: BTreeMap::from([(
                            "shape".into(),
                            serde_json::to_value(&input.domain).unwrap_or(Value::Null),
                        )]),
                    },
                );
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: domain,
                    relation: GraphRelation::Consumes,
                });
            }
            if let Some(output) = &function.output {
                let domain = format!("domain:{}:output", function.id);
                graph.nodes.insert(
                    domain.clone(),
                    GraphNode {
                        id: domain.clone(),
                        kind: GraphNodeKind::ValueDomain,
                        attributes: BTreeMap::from([(
                            "shape".into(),
                            serde_json::to_value(output).unwrap_or(Value::Null),
                        )]),
                    },
                );
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: domain,
                    relation: GraphRelation::Produces,
                });
            }
            for effect in &function.effects {
                let (name, kind, relation) = match effect.kind {
                    EffectKind::Read => (
                        effect.resource.as_deref().unwrap_or("unknown"),
                        GraphNodeKind::Resource,
                        GraphRelation::Reads,
                    ),
                    EffectKind::Write => (
                        effect.resource.as_deref().unwrap_or("unknown"),
                        GraphNodeKind::Resource,
                        GraphRelation::Writes,
                    ),
                    EffectKind::Delete => (
                        effect.resource.as_deref().unwrap_or("unknown"),
                        GraphNodeKind::Resource,
                        GraphRelation::Deletes,
                    ),
                    EffectKind::Emit => (
                        effect.event.as_deref().unwrap_or("unknown"),
                        GraphNodeKind::Event,
                        GraphRelation::Emits,
                    ),
                    EffectKind::Call => continue,
                };
                let target = format!(
                    "{}:{name}",
                    if kind == GraphNodeKind::Event {
                        "event"
                    } else {
                        "resource"
                    }
                );
                graph.nodes.entry(target.clone()).or_insert(GraphNode {
                    id: target.clone(),
                    kind,
                    attributes: BTreeMap::new(),
                });
                graph.edges.push(GraphEdge {
                    from: function_id.clone(),
                    to: target,
                    relation,
                });
            }
        }
    }
    let mut previous = BTreeMap::<String, String>::new();
    for event in events {
        let observation = format!(
            "observation:{}:{}:{}",
            event.trace_id, event.span_id, event.sequence
        );
        let operation = format!("operation:{}", event.operation);
        graph.nodes.entry(operation.clone()).or_insert(GraphNode {
            id: operation.clone(),
            kind: GraphNodeKind::Operation,
            attributes: BTreeMap::new(),
        });
        graph.nodes.insert(
            observation.clone(),
            GraphNode {
                id: observation.clone(),
                kind: GraphNodeKind::RuntimeObservation,
                attributes: BTreeMap::new(),
            },
        );
        graph.edges.push(GraphEdge {
            from: operation.clone(),
            to: observation.clone(),
            relation: GraphRelation::ObservedAs,
        });
        if let Some(prior) = previous.insert(event.trace_id.clone(), observation.clone()) {
            graph.edges.push(GraphEdge {
                from: prior,
                to: observation.clone(),
                relation: GraphRelation::HappensBefore,
            });
        }
        if let BackendEventKind::Effect {
            effect,
            resource,
            event: emitted,
            ..
        } = &event.event
        {
            let (target, kind, relation) = if *effect == EffectKind::Emit {
                (
                    emitted.as_deref().unwrap_or("unknown"),
                    GraphNodeKind::Event,
                    GraphRelation::Emits,
                )
            } else {
                let relation = match effect {
                    EffectKind::Read => GraphRelation::Reads,
                    EffectKind::Write => GraphRelation::Writes,
                    EffectKind::Delete => GraphRelation::Deletes,
                    EffectKind::Call => GraphRelation::Calls,
                    EffectKind::Emit => GraphRelation::Emits,
                };
                (
                    resource.as_deref().unwrap_or("unknown"),
                    GraphNodeKind::Resource,
                    relation,
                )
            };
            let target = format!(
                "{}:{target}",
                match kind {
                    GraphNodeKind::Event => "event",
                    _ => "resource",
                }
            );
            graph.nodes.entry(target.clone()).or_insert(GraphNode {
                id: target.clone(),
                kind,
                attributes: BTreeMap::new(),
            });
            graph.edges.push(GraphEdge {
                from: observation,
                to: target,
                relation,
            });
        }
    }
    graph
}

pub fn import_service_schema(document: &Value) -> Vec<OperationContract> {
    if document.get("openapi").is_some() || document.get("swagger").is_some() {
        import_openapi(document)
    } else if document.pointer("/data/__schema").is_some() || document.get("__schema").is_some() {
        import_graphql(document)
    } else if document.get("file").is_some() || document.get("files").is_some() {
        import_protobuf_descriptor(document)
    } else {
        Vec::new()
    }
}

pub fn import_openapi(document: &Value) -> Vec<OperationContract> {
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut operations = Vec::new();
    for (path, path_item) in paths {
        let Some(methods) = path_item.as_object() else {
            continue;
        };
        for (method, operation) in methods {
            if !["get", "post", "put", "patch", "delete", "head", "options"]
                .contains(&method.as_str())
            {
                continue;
            }
            let id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} {}", method.to_ascii_uppercase(), path));
            let body = operation
                .pointer("/requestBody/content")
                .and_then(Value::as_object)
                .and_then(|content| safe_content_domain(content, document, false));
            let input = openapi_input(path_item, operation, body, document);
            let mut success_statuses = Vec::new();
            let mut outputs_by_status = BTreeMap::new();
            if let Some(responses) = operation.get("responses").and_then(Value::as_object) {
                for (status, response) in responses {
                    let Some(code) = status.parse::<u16>().ok() else {
                        continue;
                    };
                    if (200..400).contains(&code) {
                        success_statuses.push(code);
                        if let Some(domain) = response
                            .get("content")
                            .and_then(Value::as_object)
                            .and_then(|content| safe_content_domain(content, document, true))
                        {
                            outputs_by_status.insert(code, domain);
                        }
                    }
                }
            }
            let output = match outputs_by_status.len() {
                0 => None,
                1 => outputs_by_status.values().next().cloned(),
                _ => Some(ValueDomain::OneOf {
                    variants: outputs_by_status.values().cloned().collect(),
                }),
            };
            operations.push(OperationContract {
                id,
                authority: Authority::Schema,
                input,
                output,
                outputs_by_status,
                success_statuses,
                read_only: matches!(method.as_str(), "get" | "head" | "options"),
                idempotent: matches!(
                    method.as_str(),
                    "get" | "put" | "delete" | "head" | "options"
                ),
                tenant_isolated: false,
                promised_effects: Vec::new(),
            });
        }
    }
    operations
}

/// Import only encodings whose decoded value is structurally unambiguous.
/// JSON (including vendor `+json`) carries the complete JSON domain. Plain text
/// is safe only for a string schema, and form-urlencoded is safe only for an
/// object schema. XML, multipart, and binary bodies remain guidance-free until
/// an adapter can prove their decoded structure.
fn safe_content_domain(
    content: &serde_json::Map<String, Value>,
    document: &Value,
    response: bool,
) -> Option<ValueDomain> {
    let mut domains = Vec::new();
    for (media_type, media) in content {
        let media_type = media_type
            .split(';')
            .next()
            .unwrap_or(media_type)
            .trim()
            .to_ascii_lowercase();
        let Some(domain) = media
            .get("schema")
            .and_then(|schema| schema_domain(schema, document))
        else {
            continue;
        };
        let safe = media_type == "application/json"
            || media_type.ends_with("+json")
            || (media_type == "text/plain" && matches!(&domain, ValueDomain::String { .. }))
            || (!response
                && media_type == "application/x-www-form-urlencoded"
                && matches!(&domain, ValueDomain::Object { .. }));
        if safe {
            domains.push(domain);
        }
    }
    match domains.len() {
        0 => None,
        1 => domains.pop(),
        _ => Some(ValueDomain::OneOf { variants: domains }),
    }
}

fn openapi_input(
    path_item: &Value,
    operation: &Value,
    body: Option<ValueDomain>,
    document: &Value,
) -> Option<ValueDomain> {
    let mut groups = BTreeMap::<String, ValueDomain>::new();
    let mut required_groups = BTreeSet::new();
    let parameters = path_item
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(
            operation
                .get("parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        );
    let mut fields = BTreeMap::<String, (BTreeMap<String, ValueDomain>, BTreeSet<String>)>::new();
    for raw in parameters {
        let parameter = resolve_local_ref(raw, document).unwrap_or(raw);
        let Some(location) = parameter.get("in").and_then(Value::as_str) else {
            continue;
        };
        // Cookie values are secrets by default. Object/deepObject serialization
        // is not canonical across clients, so neither can be exact evidence.
        if !matches!(location, "path" | "query" | "header")
            || parameter.get("content").is_some()
            || parameter.get("style").and_then(Value::as_str) == Some("deepObject")
        {
            continue;
        }
        let Some(name) = parameter.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(domain) = parameter
            .get("schema")
            .and_then(|schema| schema_domain(schema, document))
        else {
            continue;
        };
        if matches!(&domain, ValueDomain::Object { .. }) {
            continue;
        }
        let group = match location {
            "path" => "path",
            "query" => "query",
            _ => "headers",
        };
        let normalized = if location == "header" {
            name.to_ascii_lowercase()
        } else {
            name.to_string()
        };
        let entry = fields.entry(group.into()).or_default();
        entry.0.insert(normalized.clone(), domain);
        if location == "path" || parameter.get("required").and_then(Value::as_bool) == Some(true) {
            entry.1.insert(normalized);
            required_groups.insert(group.into());
        }
    }
    for (group, (properties, required)) in fields {
        groups.insert(
            group,
            ValueDomain::Object {
                required,
                properties,
                additional: true,
            },
        );
    }
    if groups.is_empty() {
        return body;
    }
    if let Some(body) = body {
        groups.insert("body".into(), body);
        if operation
            .pointer("/requestBody/required")
            .and_then(Value::as_bool)
            == Some(true)
        {
            required_groups.insert("body".into());
        }
    }
    Some(ValueDomain::Object {
        required: required_groups,
        properties: groups,
        additional: false,
    })
}

fn resolve_local_ref<'a>(value: &'a Value, document: &'a Value) -> Option<&'a Value> {
    let reference = value.get("$ref")?.as_str()?.strip_prefix('#')?;
    document.pointer(reference)
}

fn import_graphql(document: &Value) -> Vec<OperationContract> {
    let schema = document
        .pointer("/data/__schema")
        .or_else(|| document.get("__schema"));
    let Some(schema) = schema else {
        return Vec::new();
    };
    let types = schema
        .get("types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(|name| (name, value))
        })
        .collect::<BTreeMap<_, _>>();
    let roots = [
        ("queryType", true),
        ("mutationType", false),
        ("subscriptionType", true),
    ];
    let mut operations = Vec::new();
    for (root_key, read_only) in roots {
        let Some(root_name) = schema
            .get(root_key)
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(root) = types.get(root_name) else {
            continue;
        };
        for field in root
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = field.get("name").and_then(Value::as_str) else {
                continue;
            };
            let args = field
                .get("args")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|argument| {
                    let name = argument.get("name")?.as_str()?.to_string();
                    let domain = graphql_domain(
                        argument.get("type")?,
                        &types,
                        &mut BTreeSet::new(),
                        GraphqlDomainContext::Input,
                    )?;
                    Some((name, domain, graphql_non_null(argument.get("type")?)))
                })
                .collect::<Vec<_>>();
            let input = (!args.is_empty()).then(|| ValueDomain::Object {
                required: args
                    .iter()
                    .filter(|(_, _, required)| *required)
                    .map(|(name, _, _)| name.clone())
                    .collect(),
                properties: args
                    .into_iter()
                    .map(|(name, domain, _)| (name, domain))
                    .collect(),
                additional: false,
            });
            operations.push(OperationContract {
                id: id.to_string(),
                authority: Authority::Schema,
                input,
                output: field.get("type").and_then(|value| {
                    graphql_domain(
                        value,
                        &types,
                        &mut BTreeSet::new(),
                        GraphqlDomainContext::Output,
                    )
                }),
                outputs_by_status: BTreeMap::new(),
                success_statuses: Vec::new(),
                read_only,
                idempotent: read_only,
                tenant_isolated: false,
                promised_effects: Vec::new(),
            });
        }
    }
    operations
}

fn graphql_non_null(reference: &Value) -> bool {
    reference.get("kind").and_then(Value::as_str) == Some("NON_NULL")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphqlDomainContext {
    Input,
    Output,
}

fn graphql_domain(
    reference: &Value,
    types: &BTreeMap<&str, &Value>,
    visiting: &mut BTreeSet<String>,
    context: GraphqlDomainContext,
) -> Option<ValueDomain> {
    let kind = reference.get("kind").and_then(Value::as_str)?;
    if kind == "NON_NULL" {
        return graphql_non_null_domain(reference.get("ofType")?, types, visiting, context);
    }
    let domain = graphql_non_null_domain(reference, types, visiting, context)?;
    Some(ValueDomain::OneOf {
        variants: vec![ValueDomain::Null, domain],
    })
}

fn graphql_non_null_domain(
    reference: &Value,
    types: &BTreeMap<&str, &Value>,
    visiting: &mut BTreeSet<String>,
    context: GraphqlDomainContext,
) -> Option<ValueDomain> {
    let kind = reference.get("kind").and_then(Value::as_str)?;
    if kind == "LIST" {
        return Some(ValueDomain::Array {
            items: Box::new(graphql_domain(
                reference.get("ofType")?,
                types,
                visiting,
                context,
            )?),
            min_items: None,
            max_items: None,
            unique: false,
        });
    }
    let name = reference
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("String");
    match kind {
        "SCALAR" => Some(match name {
            "Int" => ValueDomain::Integer {
                min: None,
                max: None,
            },
            "Float" => ValueDomain::Number,
            "Boolean" => ValueDomain::Boolean,
            "ID" => ValueDomain::Resource {
                resource: "graphql-id".into(),
            },
            _ => ValueDomain::String {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                variants: Vec::new(),
            },
        }),
        "ENUM" => Some(ValueDomain::String {
            min_length: None,
            max_length: None,
            pattern: None,
            format: None,
            variants: types
                .get(name)
                .and_then(|value| value.get("enumValues"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|value| value.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect(),
        }),
        "INTERFACE" | "UNION" if context == GraphqlDomainContext::Output => {
            let definition = types.get(name)?;
            let variants = definition
                .get("possibleTypes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|possible| {
                    let possible_name = possible.get("name")?.as_str()?.to_string();
                    let reference = json!({"kind":"OBJECT","name":possible_name});
                    let domain = graphql_non_null_domain(&reference, types, visiting, context)?;
                    Some((possible_name, domain))
                })
                .collect::<BTreeMap<_, _>>();
            Some(ValueDomain::GraphqlAbstract { variants })
        }
        "OBJECT" | "INPUT_OBJECT" => {
            if !visiting.insert(name.to_string()) {
                return Some(ValueDomain::Any);
            }
            let definition = types.get(name)?;
            let fields = definition
                .get(if kind == "OBJECT" {
                    "fields"
                } else {
                    "inputFields"
                })
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let required = fields
                .iter()
                .filter(|field| field.get("type").is_some_and(graphql_non_null))
                .filter_map(|field| field.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            let properties = fields
                .into_iter()
                .filter_map(|field| {
                    let field_name = field.get("name")?.as_str()?.to_string();
                    let domain = graphql_domain(field.get("type")?, types, visiting, context)?;
                    Some((field_name, domain))
                })
                .collect();
            visiting.remove(name);
            Some(ValueDomain::Object {
                // A GraphQL response contains only the client's selection set.
                // Introspection describes the complete object type, not the
                // fields selected by this invocation, so requiring every
                // NON_NULL schema field would reject valid partial responses.
                // Keep validating selected fields, but leave presence open
                // until runtime evidence carries a normalized selection set.
                required: if context == GraphqlDomainContext::Input {
                    required
                } else {
                    BTreeSet::new()
                },
                properties,
                // `__typename` is always selectable but is not part of the
                // ordinary field list returned by introspection.
                additional: context == GraphqlDomainContext::Output,
            })
        }
        _ => Some(ValueDomain::Any),
    }
}

fn import_protobuf_descriptor(document: &Value) -> Vec<OperationContract> {
    let files = document
        .get("file")
        .or_else(|| document.get("files"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut messages = BTreeMap::<String, &Value>::new();
    for file in &files {
        let package = file.get("package").and_then(Value::as_str).unwrap_or("");
        for message in file
            .get("messageType")
            .or_else(|| file.get("message_type"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = message.get("name").and_then(Value::as_str) {
                messages.insert(format!(".{package}.{name}"), message);
            }
        }
    }
    let mut operations = Vec::new();
    for file in files {
        let package = file.get("package").and_then(Value::as_str).unwrap_or("");
        for service in file
            .get("service")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let service_name = service
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Service");
            for method in service
                .get("method")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let method_name = method
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Method");
                let prefix = if package.is_empty() {
                    service_name.to_string()
                } else {
                    format!("{package}.{service_name}")
                };
                let input = method
                    .get("inputType")
                    .or_else(|| method.get("input_type"))
                    .and_then(Value::as_str)
                    .and_then(|name| {
                        protobuf_message_domain(name, &messages, &mut BTreeSet::new())
                    });
                let output = method
                    .get("outputType")
                    .or_else(|| method.get("output_type"))
                    .and_then(Value::as_str)
                    .and_then(|name| {
                        protobuf_message_domain(name, &messages, &mut BTreeSet::new())
                    });
                operations.push(OperationContract {
                    id: format!("{prefix}/{method_name}"),
                    authority: Authority::Schema,
                    input,
                    output,
                    outputs_by_status: BTreeMap::new(),
                    success_statuses: Vec::new(),
                    read_only: false,
                    idempotent: false,
                    tenant_isolated: false,
                    promised_effects: Vec::new(),
                });
            }
        }
    }
    operations
}

fn protobuf_message_domain(
    name: &str,
    messages: &BTreeMap<String, &Value>,
    visiting: &mut BTreeSet<String>,
) -> Option<ValueDomain> {
    if !visiting.insert(name.to_string()) {
        return Some(ValueDomain::Any);
    }
    let message = messages.get(name)?;
    let mut properties = BTreeMap::new();
    for field in message
        .get("field")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(field_name) = field.get("name").and_then(Value::as_str) else {
            continue;
        };
        let kind = field
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("TYPE_STRING");
        let mut domain = match kind {
            "TYPE_BOOL" => ValueDomain::Boolean,
            "TYPE_DOUBLE" | "TYPE_FLOAT" => ValueDomain::Number,
            "TYPE_INT32" | "TYPE_UINT32" | "TYPE_SINT32" | "TYPE_FIXED32" | "TYPE_SFIXED32" => {
                ValueDomain::Integer {
                    min: None,
                    max: None,
                }
            }
            "TYPE_INT64" | "TYPE_SINT64" | "TYPE_SFIXED64" => {
                ValueDomain::ProtoInteger64 { signed: true }
            }
            "TYPE_UINT64" | "TYPE_FIXED64" => ValueDomain::ProtoInteger64 { signed: false },
            "TYPE_MESSAGE" => field
                .get("typeName")
                .or_else(|| field.get("type_name"))
                .and_then(Value::as_str)
                .and_then(|nested| protobuf_message_domain(nested, messages, visiting))
                .unwrap_or(ValueDomain::Any),
            "TYPE_ENUM" | "TYPE_STRING" | "TYPE_BYTES" => ValueDomain::String {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                variants: Vec::new(),
            },
            _ => ValueDomain::Any,
        };
        if field.get("label").and_then(Value::as_str) == Some("LABEL_REPEATED") {
            domain = ValueDomain::Array {
                items: Box::new(domain),
                min_items: None,
                max_items: None,
                unique: false,
            };
        }
        properties.insert(field_name.to_string(), domain);
    }
    visiting.remove(name);
    Some(ValueDomain::Object {
        required: BTreeSet::new(),
        properties,
        additional: false,
    })
}

fn schema_domain(schema: &Value, document: &Value) -> Option<ValueDomain> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let pointer = reference.strip_prefix('#')?;
        return document
            .pointer(pointer)
            .and_then(|schema| schema_domain(schema, document));
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        return Some(ValueDomain::OneOf {
            variants: one_of
                .iter()
                .filter_map(|value| schema_domain(value, document))
                .collect(),
        });
    }
    if let Some(value) = schema.get("const") {
        return Some(ValueDomain::Literal {
            value: value.clone(),
        });
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("null") => Some(ValueDomain::Null),
        Some("boolean") => Some(ValueDomain::Boolean),
        Some("integer") => Some(ValueDomain::Integer {
            min: schema.get("minimum").and_then(Value::as_i64),
            max: schema.get("maximum").and_then(Value::as_i64),
        }),
        Some("number") => Some(ValueDomain::Number),
        Some("string") => Some(ValueDomain::String {
            min_length: schema
                .get("minLength")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
            max_length: schema
                .get("maxLength")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
            pattern: schema
                .get("pattern")
                .and_then(Value::as_str)
                .map(str::to_string),
            format: schema
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_string),
            variants: schema
                .get("enum")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        }),
        Some("array") => Some(ValueDomain::Array {
            items: Box::new(
                schema
                    .get("items")
                    .and_then(|value| schema_domain(value, document))
                    .unwrap_or(ValueDomain::Any),
            ),
            min_items: schema
                .get("minItems")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
            max_items: schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
            unique: schema
                .get("uniqueItems")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        Some("object") | None if schema.get("properties").is_some() => Some(ValueDomain::Object {
            required: schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            properties: schema
                .get("properties")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(name, value)| {
                    schema_domain(value, document).map(|domain| (name.clone(), domain))
                })
                .collect(),
            additional: schema
                .get("additionalProperties")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }),
        _ => Some(ValueDomain::Any),
    }
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let fields = object
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_default(),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{fields}}}")
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, span: &str, operation: &str, kind: BackendEventKind) -> BackendEvent {
        BackendEvent {
            sequence,
            trace_id: "trace-a".into(),
            span_id: span.into(),
            action_index: 1,
            parent_span_id: None,
            operation: operation.into(),
            actor: Some("alice".into()),
            tenant: Some("tenant-a".into()),
            idempotency_key: None,
            selections: Vec::new(),
            event: kind,
        }
    }

    fn contract() -> OperationContract {
        OperationContract {
            id: "createMessage".into(),
            authority: Authority::Declared,
            input: None,
            output: Some(ValueDomain::Object {
                required: BTreeSet::from(["id".into()]),
                properties: BTreeMap::from([(
                    "id".into(),
                    ValueDomain::String {
                        min_length: Some(1),
                        max_length: None,
                        pattern: None,
                        format: None,
                        variants: vec![],
                    },
                )]),
                additional: true,
            }),
            outputs_by_status: BTreeMap::new(),
            success_statuses: vec![201],
            read_only: false,
            idempotent: false,
            tenant_isolated: true,
            promised_effects: vec![EffectPattern {
                kind: EffectKind::Write,
                resource: Some("messages".into()),
                event: None,
                at_least: 1,
                at_most: None,
            }],
        }
    }

    #[test]
    fn hard_oracles_require_concrete_authoritative_witnesses() {
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![contract()],
            programs: vec![],
        };
        let events = vec![
            event(
                1,
                "span-a",
                "createMessage",
                BackendEventKind::Start {
                    input: json!({"body":"hello"}),
                },
            ),
            event(
                2,
                "span-a",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-b".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m1"})),
                    payload: None,
                },
            ),
            event(
                3,
                "span-a",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 2);
        assert!(violations.iter().any(|v| v.oracle == "response-shape"));
        assert!(violations.iter().any(|v| v.oracle == "tenant-isolation"));
    }

    #[test]
    fn inferred_contracts_never_create_findings() {
        let mut inferred = contract();
        inferred.authority = Authority::Inferred;
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![inferred],
            programs: vec![],
        };
        let events = vec![
            event(
                1,
                "span-a",
                "createMessage",
                BackendEventKind::Start { input: Value::Null },
            ),
            event(
                2,
                "span-a",
                "createMessage",
                BackendEventKind::Return {
                    output: Value::Null,
                    // Even a status outside this inferred operation's imported
                    // shape is guidance only; inferred facts never become hard
                    // response-status findings.
                    status: Some(200),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        assert!(evaluate(&config, &events).is_empty());
    }

    #[test]
    fn imports_openapi_operations_and_resolves_schema_references() {
        let document = json!({
            "openapi":"3.1.0",
            "paths":{"/messages":{"post":{
                "operationId":"createMessage",
                "responses":{"201":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Message"}}}}}
            }}},
            "components":{"schemas":{"Message":{"type":"object","required":["id"],"properties":{"id":{"type":"string","format":"uuid"}}}}}
        });
        let operations = import_openapi(&document);
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].authority, Authority::Schema);
        assert_eq!(operations[0].success_statuses, [201]);
        assert!(operations[0]
            .output
            .as_ref()
            .unwrap()
            .mismatch(&json!({}), "$output")
            .is_some());
    }

    #[test]
    fn openapi_imports_exact_parameters_and_only_safe_media() {
        let document = json!({
            "openapi":"3.1.0",
            "paths":{"/projects/{project}/export":{"post":{
                "operationId":"exportProject",
                "parameters":[
                    {"in":"path","name":"project","required":true,"schema":{"type":"string"}},
                    {"in":"query","name":"limit","required":true,"schema":{"type":"integer","minimum":1}},
                    {"in":"header","name":"X-Mode","schema":{"type":"string","enum":["safe"]}},
                    {"in":"cookie","name":"session","required":true,"schema":{"type":"string"}},
                    {"in":"query","name":"filter","style":"deepObject","schema":{"type":"object","properties":{"x":{"type":"string"}}}}
                ],
                "requestBody":{"required":true,"content":{
                    "application/vnd.reproit+json":{"schema":{"type":"object","required":["format"],"properties":{"format":{"type":"string"}}}},
                    "application/xml":{"schema":{"type":"object","required":["unsafe"],"properties":{"unsafe":{"type":"string"}}}}
                }},
                "responses":{"200":{"content":{
                    "text/plain":{"schema":{"type":"string"}},
                    "application/octet-stream":{"schema":{"type":"string"}}
                }}}
            }}}
        });
        let operation = import_openapi(&document).pop().unwrap();
        let input = operation.input.unwrap();
        assert!(input
            .mismatch(
                &json!({
                    "path":{"project":"p1"},
                    "query":{"limit":1},
                    "headers":{"x-mode":"safe"},
                    "body":{"format":"text"}
                }),
                "$input"
            )
            .is_none());
        assert!(input
            .mismatch(
                &json!({
                    "path":{}, "query":{"limit":1}, "body":{"format":"text"}
                }),
                "$input"
            )
            .is_some());
        assert!(operation
            .output
            .unwrap()
            .mismatch(&json!("ok"), "$output")
            .is_none());
    }

    #[test]
    fn openapi_response_shapes_are_bound_to_the_observed_status() {
        let document = json!({"openapi":"3.1.0","paths":{"/items":{"post":{
            "operationId":"createItem","responses":{
                "200":{"content":{"application/json":{"schema":{"type":"object","required":["existing"],"properties":{"existing":{"type":"boolean"}}}}}},
                "201":{"content":{"application/json":{"schema":{"type":"object","required":["id"],"properties":{"id":{"type":"string"}}}}}}
            }
        }}}});
        let operation = import_openapi(&document).pop().unwrap();
        assert!(operation.outputs_by_status[&200]
            .mismatch(&json!({"existing":true}), "$output")
            .is_none());
        assert!(operation.outputs_by_status[&201]
            .mismatch(&json!({"existing":true}), "$output")
            .is_some());
    }

    #[test]
    fn imports_graphql_introspection_without_localized_names() {
        let document = json!({"data":{"__schema":{
            "queryType":{"name":"Query"},
            "mutationType":{"name":"Mutation"},
            "types":[
                {"kind":"OBJECT","name":"Query","fields":[{"name":"message","args":[{"name":"id","type":{"kind":"NON_NULL","name":null,"ofType":{"kind":"SCALAR","name":"ID"}}}],"type":{"kind":"OBJECT","name":"Message"}}]},
                {"kind":"OBJECT","name":"Mutation","fields":[{"name":"createMessage","args":[{"name":"body","type":{"kind":"SCALAR","name":"String"}}],"type":{"kind":"OBJECT","name":"Message"}}]},
                {"kind":"OBJECT","name":"Message","fields":[{"name":"id","type":{"kind":"NON_NULL","name":null,"ofType":{"kind":"SCALAR","name":"ID"}}},{"name":"body","type":{"kind":"SCALAR","name":"String"}}]}
            ]
        }}});
        let operations = import_service_schema(&document);
        assert_eq!(operations.len(), 2);
        assert!(
            operations
                .iter()
                .find(|op| op.id == "message")
                .unwrap()
                .read_only
        );
        assert!(
            !operations
                .iter()
                .find(|op| op.id == "createMessage")
                .unwrap()
                .read_only
        );
        let query = operations.iter().find(|op| op.id == "message").unwrap();
        assert!(query
            .input
            .as_ref()
            .unwrap()
            .mismatch(&json!({}), "$input")
            .is_some());
    }

    #[test]
    fn graphql_output_contract_respects_selection_sets_without_losing_type_checks() {
        // Reduced from the open-source Countries GraphQL API. Both Country
        // fields are NON_NULL in the schema, but selecting only `code` is a
        // complete and valid GraphQL response. Until traces carry a normalized
        // selection set, absence of an unselected field cannot be a finding.
        let document = json!({"data":{"__schema":{
            "queryType":{"name":"Query"},
            "types":[
                {"kind":"OBJECT","name":"Query","fields":[{
                    "name":"country",
                    "args":[{"name":"code","type":{"kind":"NON_NULL","name":null,"ofType":{"kind":"SCALAR","name":"String"}}}],
                    "type":{"kind":"OBJECT","name":"Country"}
                }]},
                {"kind":"OBJECT","name":"Country","fields":[
                    {"name":"code","type":{"kind":"NON_NULL","name":null,"ofType":{"kind":"SCALAR","name":"String"}}},
                    {"name":"awsRegion","type":{"kind":"NON_NULL","name":null,"ofType":{"kind":"SCALAR","name":"String"}}}
                ]}
            ]
        }}});
        let operations = import_service_schema(&document);
        let country = operations
            .iter()
            .find(|operation| operation.id == "country")
            .unwrap();
        let input = country.input.as_ref().unwrap();
        assert!(input.mismatch(&json!({"code":"US"}), "$input").is_none());
        assert!(input.mismatch(&json!({}), "$input").is_some());

        let output = country.output.as_ref().unwrap();
        assert!(output.mismatch(&json!({"code":"US"}), "$output").is_none());
        assert!(output.mismatch(&Value::Null, "$output").is_none());
        assert_eq!(
            output.mismatch(&json!({"code": 7}), "$output"),
            Some("$output does not match any allowed variant".into())
        );
        let selected = [GraphqlSelection {
            schema_path: "awsRegion".into(),
            response_path: "region".into(),
            type_condition: None,
        }];
        assert!(selection_mismatch(output, &json!({"code":"US"}), &selected)
            .unwrap()
            .contains("region was selected"));
        assert!(selection_mismatch(
            output,
            &json!({"code":"US","region":"us-east-2"}),
            &selected,
        )
        .is_none());
    }

    #[test]
    fn graphql_union_selection_applies_only_to_the_exact_runtime_type() {
        let document = json!({"data":{"__schema":{
            "queryType":{"name":"Query"},
            "types":[
                {"kind":"OBJECT","name":"Query","fields":[{"name":"search","args":[],"type":{"kind":"UNION","name":"SearchResult"}}]},
                {"kind":"UNION","name":"SearchResult","possibleTypes":[{"kind":"OBJECT","name":"Human"},{"kind":"OBJECT","name":"Bot"}]},
                {"kind":"OBJECT","name":"Human","fields":[{"name":"handle","type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"String"}}}]},
                {"kind":"OBJECT","name":"Bot","fields":[{"name":"id","type":{"kind":"NON_NULL","ofType":{"kind":"SCALAR","name":"ID"}}}]}
            ]
        }}});
        let operation = import_graphql(&document).pop().unwrap();
        let output = operation.output.unwrap();
        let selected = [GraphqlSelection {
            schema_path: "handle".into(),
            response_path: "name".into(),
            type_condition: Some("Human".into()),
        }];
        assert!(
            selection_mismatch(&output, &json!({"__typename":"Bot","id":"b1"}), &selected,)
                .is_none()
        );
        assert!(
            selection_mismatch(&output, &json!({"__typename":"Human"}), &selected,)
                .unwrap()
                .contains("name was selected")
        );
        assert!(selection_mismatch(
            &output,
            &json!({"__typename":"Human","name":"ada"}),
            &selected,
        )
        .is_none());

        let list = ValueDomain::Array {
            items: Box::new(output.clone()),
            min_items: None,
            max_items: None,
            unique: false,
        };
        assert!(selection_mismatch(
            &list,
            &json!([
                {"__typename":"Bot","id":"b1"},
                {"__typename":"Human","name":7}
            ]),
            &selected,
        )
        .unwrap()
        .contains("$output[1].name"));
    }

    #[test]
    fn imports_protobuf_descriptor_json_as_grpc_operations() {
        let document = json!({"file":[{
            "package":"chat.v1",
            "messageType":[
                {"name":"GetRequest","field":[{"name":"id","type":"TYPE_STRING"}]},
                {"name":"Message","field":[{"name":"id","type":"TYPE_STRING"},{"name":"tags","type":"TYPE_STRING","label":"LABEL_REPEATED"}]}
            ],
            "service":[{"name":"Chat","method":[{"name":"Get","inputType":".chat.v1.GetRequest","outputType":".chat.v1.Message"}]}]
        }]});
        let operations = import_service_schema(&document);
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].id, "chat.v1.Chat/Get");
        assert!(operations[0]
            .output
            .as_ref()
            .unwrap()
            .mismatch(&json!({"id":"m1","tags":["a"]}), "$output")
            .is_none());
    }

    #[test]
    fn protobuf_64_bit_domains_follow_exact_protojson_encoding() {
        let signed = ValueDomain::ProtoInteger64 { signed: true };
        let unsigned = ValueDomain::ProtoInteger64 { signed: false };
        for value in [
            json!("-9223372036854775808"),
            json!("9223372036854775807"),
            json!(0),
            json!(9_007_199_254_740_991_i64),
        ] {
            assert!(signed.mismatch(&value, "$value").is_none(), "{value}");
        }
        for value in [
            json!("0"),
            json!("18446744073709551615"),
            json!(9_007_199_254_740_991_u64),
        ] {
            assert!(unsigned.mismatch(&value, "$value").is_none(), "{value}");
        }
        for value in [
            json!("01"),
            json!("-0"),
            json!("1.0"),
            json!("9223372036854775808"),
            json!(9_007_199_254_740_992_u64),
        ] {
            assert!(signed.mismatch(&value, "$value").is_some(), "{value}");
        }
        for value in [
            json!("-1"),
            json!("18446744073709551616"),
            json!("0x10"),
            json!(9_007_199_254_740_992_u64),
        ] {
            assert!(unsigned.mismatch(&value, "$value").is_some(), "{value}");
        }

        let repeated = ValueDomain::Array {
            items: Box::new(unsigned),
            min_items: None,
            max_items: None,
            unique: false,
        };
        assert!(repeated
            .mismatch(&json!(["0", "18446744073709551615"]), "$values")
            .is_none());
        assert!(repeated.mismatch(&json!(["0", "-1"]), "$values").is_some());
    }

    #[test]
    fn graph_joins_runtime_effects_to_declared_operations() {
        let mut config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![contract()],
            programs: vec![],
        };
        config.programs.push(ProgramSummary {
            language: "rust".into(),
            build: Some("abc123".into()),
            functions: vec![FunctionSummary {
                id: "handlers::create_message".into(),
                name: "create_message".into(),
                source: Some("src/handlers.rs:42".into()),
                operation: Some("createMessage".into()),
                inputs: vec![ValueSlot {
                    name: "body".into(),
                    domain: ValueDomain::String {
                        min_length: Some(1),
                        max_length: Some(4000),
                        pattern: None,
                        format: None,
                        variants: vec![],
                    },
                }],
                output: Some(ValueDomain::Resource {
                    resource: "message".into(),
                }),
                calls: vec!["repository::insert_message".into()],
                effects: vec![StaticEffect {
                    kind: EffectKind::Write,
                    resource: Some("messages".into()),
                    event: None,
                }],
                requires: vec!["actor.member_of(room)".into()],
                ensures: vec!["message.exists".into()],
                authority: Authority::Inferred,
            }],
        });
        let events = vec![event(
            1,
            "span-a",
            "createMessage",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("messages".into()),
                key: Some("m1".into()),
                tenant: Some("tenant-a".into()),
                event: None,
                before: None,
                after: None,
                payload: None,
            },
        )];
        let graph = build_graph(&config, &events);
        assert!(graph.nodes.contains_key("operation:createMessage"));
        assert!(graph
            .nodes
            .contains_key("function:handlers::create_message"));
        assert!(graph
            .nodes
            .contains_key("function:repository::insert_message"));
        assert!(graph.nodes.contains_key("resource:messages"));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.relation == GraphRelation::Writes));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.relation == GraphRelation::Implements));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.relation == GraphRelation::Calls));
    }

    #[test]
    fn read_only_and_missing_effect_oracles_are_exact() {
        let mut read = contract();
        read.id = "getMessage".into();
        read.read_only = true;
        read.promised_effects = vec![EffectPattern {
            kind: EffectKind::Read,
            resource: Some("messages".into()),
            event: None,
            at_least: 1,
            at_most: None,
        }];
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![read],
            programs: vec![],
        };
        let events = vec![
            event(
                1,
                "read",
                "getMessage",
                BackendEventKind::Start { input: json!("m1") },
            ),
            event(
                2,
                "read",
                "getMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"seen":true})),
                    payload: None,
                },
            ),
            event(
                3,
                "read",
                "getMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 2);
        assert!(violations.iter().any(|v| v.oracle == "read-only-mutation"));
        assert!(violations.iter().any(|v| v.oracle == "missing-effect"));
    }

    #[test]
    fn incomplete_effect_telemetry_cannot_create_an_absence_finding() {
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![contract()],
            programs: vec![],
        };
        let events = vec![
            event(
                1,
                "incomplete",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "incomplete",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: false,
                },
            ),
        ];
        assert!(evaluate(&config, &events).is_empty());
    }

    #[test]
    fn upper_effect_bound_confirms_duplicate_side_effects() {
        let mut create = contract();
        create.promised_effects[0].at_most = Some(1);
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![create],
            programs: vec![],
        };
        let events = vec![
            event(
                1,
                "duplicate",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "duplicate",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m1"})),
                    payload: None,
                },
            ),
            event(
                3,
                "duplicate",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m2".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m2"})),
                    payload: None,
                },
            ),
            event(
                4,
                "duplicate",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "excess-effect");
    }

    #[test]
    fn idempotency_compares_persistent_effects_for_the_same_actor_and_tenant() {
        let mut create = contract();
        create.idempotent = true;
        create.output = None;
        create.promised_effects.clear();
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![create],
            programs: vec![],
        };
        let invocation = |sequence, span: &str, key: &str| {
            let mut start = event(
                sequence,
                span,
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            );
            start.idempotency_key = Some("same-key".into());
            vec![
                start,
                event(
                    sequence + 1,
                    span,
                    "createMessage",
                    BackendEventKind::Effect {
                        effect: EffectKind::Write,
                        resource: Some("messages".into()),
                        key: Some(key.into()),
                        tenant: Some("tenant-a".into()),
                        event: None,
                        before: None,
                        after: Some(json!({"id":key})),
                        payload: None,
                    },
                ),
                event(
                    sequence + 2,
                    span,
                    "createMessage",
                    BackendEventKind::Return {
                        output: json!({"id":key}),
                        status: Some(201),
                        success: true,
                        effects_complete: true,
                    },
                ),
            ]
        };
        let events = invocation(1, "one", "m1")
            .into_iter()
            .chain(invocation(4, "two", "m2"))
            .collect::<Vec<_>>();
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "idempotency");
    }

    #[test]
    fn idempotent_cached_retry_without_a_second_effect_is_clean() {
        let mut create = contract();
        create.idempotent = true;
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![create],
            programs: vec![],
        };
        let mut first = event(
            1,
            "one",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        );
        first.idempotency_key = Some("same-key".into());
        let mut retry = event(
            4,
            "two",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        );
        retry.idempotency_key = Some("same-key".into());
        let events = vec![
            first,
            event(
                2,
                "one",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m1"})),
                    payload: None,
                },
            ),
            event(
                3,
                "one",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
            retry,
            event(
                5,
                "two",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        assert!(evaluate(&config, &events).is_empty());
    }

    #[test]
    fn accepted_input_outside_declared_domain_is_a_hard_finding() {
        let mut create = contract();
        create.input = Some(ValueDomain::Object {
            required: BTreeSet::from(["body".into()]),
            properties: BTreeMap::from([(
                "body".into(),
                ValueDomain::String {
                    min_length: Some(1),
                    max_length: None,
                    pattern: None,
                    format: None,
                    variants: vec![],
                },
            )]),
            additional: true,
        });
        create.promised_effects.clear();
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![create],
            programs: vec![],
        };
        let events = vec![
            event(
                1,
                "invalid",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "invalid",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":"m1"}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let violations = evaluate(&config, &events);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].oracle, "accepted-invalid-input");
    }

    #[test]
    fn schema_formats_do_not_reject_valid_edge_cases() {
        let unbounded = ValueDomain::Integer {
            min: None,
            max: None,
        };
        assert!(unbounded.mismatch(&json!(u64::MAX), "$value").is_none());
        assert!(matches_format("date-time", "2026-07-13T03:10:00-07:00"));
        assert!(matches_format("uri", "mailto:person@example.com"));
        assert!(matches_format("email", "\"quoted.local\"@example.test"));
    }

    #[test]
    fn redacted_metadata_preserves_type_and_length_without_content_claims() {
        let secret = json!({"$reproit":{"redacted":true,"type":"string","length":8}});
        let domain = ValueDomain::String {
            min_length: Some(8),
            max_length: Some(12),
            pattern: Some("^visible-content$".into()),
            format: Some("email".into()),
            variants: vec!["visible-content".into()],
        };
        assert!(domain.mismatch(&secret, "$secret").is_none());
        let short = json!({"$reproit":{"redacted":true,"type":"string","length":2}});
        assert_eq!(
            domain.mismatch(&short, "$secret"),
            Some("$secret is shorter than its minimum".into())
        );
        let wrong = json!({"$reproit":{"redacted":true,"type":"boolean"}});
        assert_eq!(
            domain.mismatch(&wrong, "$secret"),
            Some("$secret must be string".into())
        );
    }

    #[test]
    fn marker_parser_is_structural_and_ignores_malformed_noise() {
        let log = concat!(
            "unrelated output\n",
            "REPROIT:BACKEND not-json\n",
            "flutter: REPROIT:BACKEND {\"sequence\":1,\"traceId\":\"t\",\"spanId\":\"s\",\"operation\":\"op\",\"kind\":\"start\",\"input\":{}}\n"
        );
        let events = parse_events(log);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].operation, "op");
    }

    #[test]
    fn validation_fixture_loads_and_merges_declared_and_schema_contracts() {
        let mut config: BackendConfig = serde_yaml::from_str(include_str!(
            "../../../../validation/backend/backend-contract.yaml"
        ))
        .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        config.load_schemas(&root).unwrap();
        assert_eq!(config.operations.len(), 1);
        let operation = &config.operations[0];
        assert_eq!(operation.authority, Authority::Declared);
        assert_eq!(operation.success_statuses, [201]);
        assert!(operation.input.is_some());
        assert!(operation.output.is_some());
        assert_eq!(operation.promised_effects.len(), 2);
        assert_eq!(config.programs.len(), 1);
    }

    #[test]
    fn adversarial_service_fixtures_have_zero_clean_false_positives() {
        let mut config: BackendConfig = serde_yaml::from_str(include_str!(
            "../../../../validation/backend/backend-contract.yaml"
        ))
        .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        config.load_schemas(&root).unwrap();

        let clean = evaluate(
            &config,
            &parse_events(include_str!("../../../../validation/backend/clean.ndjson")),
        );
        assert!(clean.is_empty(), "clean fixture produced {clean:?}");

        for (log, expected, action) in [
            (
                include_str!("../../../../validation/backend/broken-response.ndjson"),
                "response-shape",
                2,
            ),
            (
                include_str!("../../../../validation/backend/broken-tenant.ndjson"),
                "tenant-isolation",
                3,
            ),
            (
                include_str!("../../../../validation/backend/broken-duplicate.ndjson"),
                "excess-effect",
                4,
            ),
        ] {
            let violations = evaluate(&config, &parse_events(log));
            assert_eq!(
                violations.len(),
                1,
                "expected {expected}, got {violations:?}"
            );
            assert_eq!(violations[0].oracle, expected);
            assert_eq!(violations[0].action_index, action);
        }
    }

    #[test]
    fn reproit_cloud_schema_and_trace_contracts_catch_json_drift() {
        let mut config: BackendConfig = serde_yaml::from_str(include_str!(
            "../../../../validation/backend/cloud-contract.yaml"
        ))
        .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        config.load_schemas(&root).unwrap();

        assert_eq!(config.operations.len(), 5);
        let project = config
            .operations
            .iter()
            .find(|operation| operation.id == "cloudCreateProject")
            .unwrap();
        assert_eq!(project.authority, Authority::Declared);
        assert_eq!(project.success_statuses, [201]);
        assert!(project.input.is_some());
        assert!(project.output.is_some());
        assert!(project.tenant_isolated);

        let clean = evaluate(
            &config,
            &parse_events(include_str!(
                "../../../../validation/backend/cloud-clean.ndjson"
            )),
        );
        assert!(clean.is_empty(), "cloud clean trace produced {clean:?}");
        let live_signup = evaluate(
            &config,
            &parse_events(include_str!(
                "../../../../validation/backend/cloud-live-signup-clean.ndjson"
            )),
        );
        assert!(
            live_signup.is_empty(),
            "live Cloud signup trace produced {live_signup:?}"
        );

        for (log, expected, action) in [
            (
                include_str!("../../../../validation/backend/cloud-broken-shape.ndjson"),
                "response-shape",
                8,
            ),
            (
                include_str!("../../../../validation/backend/cloud-broken-input.ndjson"),
                "accepted-invalid-input",
                9,
            ),
            (
                include_str!("../../../../validation/backend/cloud-broken-status.ndjson"),
                "response-status",
                10,
            ),
            (
                include_str!("../../../../validation/backend/cloud-live-signup-broken.ndjson"),
                "response-shape",
                3,
            ),
        ] {
            let violations = evaluate(&config, &parse_events(log));
            assert_eq!(
                violations.len(),
                1,
                "expected {expected}, got {violations:?}"
            );
            assert_eq!(violations[0].oracle, expected);
            assert_eq!(violations[0].action_index, action);
        }
    }

    #[test]
    fn frozen_guard_preserves_exact_backend_violation_across_trace_positions() {
        let config = BackendConfig {
            enabled: true,
            origins: vec![],
            schemas: vec![],
            operations: vec![contract()],
            programs: vec![],
        };
        let original = vec![
            event(
                1,
                "span-a",
                "createMessage",
                BackendEventKind::Start { input: json!({}) },
            ),
            event(
                2,
                "span-a",
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some("m1".into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":"m1"})),
                    payload: None,
                },
            ),
            event(
                3,
                "span-a",
                "createMessage",
                BackendEventKind::Return {
                    output: json!({}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ];
        let findings = evaluate(&config, &original)
            .iter()
            .map(finding)
            .collect::<Vec<_>>();
        let guard = FrozenBackendGuard::from_findings(&config, &findings).unwrap();
        let mut moved = original;
        for event in &mut moved {
            event.sequence += 40;
            event.trace_id = "different-trace".into();
            event.span_id = "different-span".into();
        }
        let log = moved
            .iter()
            .map(|event| format!("{EVENT_MARKER}{}", serde_json::to_string(event).unwrap()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(guard.reproduces(&log));
    }
}
