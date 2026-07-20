use super::{EffectKind, EffectPattern, ValueDomain};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// RFC 9110 idempotency constrains the intended server effect, not response
    /// bytes. Applications that additionally cache and replay the first
    /// response can opt into that stronger contract explicitly.
    #[serde(default)]
    pub idempotency_response_replay: IdempotencyResponseReplay,
    #[serde(default)]
    pub tenant_isolated: bool,
    #[serde(default)]
    pub promised_effects: Vec<EffectPattern>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum IdempotencyResponseReplay {
    #[default]
    Unspecified,
    Exact,
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
