//! Authority carried by application-specific contracts.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContractAuthority {
    /// Explicit user policy, an application-owned assertion, or an existing
    /// application test.
    #[default]
    Declared,
    /// A mechanical fact whose source proves the complete predicate.
    Derived,
    /// A non-authoritative proposal that must never produce a verdict.
    Suggested,
}

impl ContractAuthority {
    pub const fn can_evaluate(self) -> bool {
        !matches!(self, Self::Suggested)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Derived => "derived",
            Self::Suggested => "suggested",
        }
    }

    pub const fn is_declared(&self) -> bool {
        matches!(self, Self::Declared)
    }
}
