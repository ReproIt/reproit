use serde::{Deserialize, Serialize};

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
