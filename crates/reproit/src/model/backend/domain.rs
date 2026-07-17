use super::{canonical_json, matches_format, redacted_metadata, RedactedMetadata};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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
    /// Every composed schema must accept the value. OpenAPI `allOf` cannot be
    /// represented as a union without weakening its non-null constraints.
    AllOf {
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

pub(super) fn default_true() -> bool {
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
            Self::AllOf { variants } => variants
                .iter()
                .find_map(|variant| variant.mismatch(value, path)),
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
            Self::AllOf { variants } => variants
                .iter()
                .find_map(|variant| variant.redacted_mismatch(metadata, path)),
            Self::GraphqlAbstract { .. } => wrong_type("object"),
            // The literal value and a resource's exact identity are intentionally
            // unavailable after redaction. Retain only safe type evidence.
            Self::Literal { .. } => None,
            Self::Resource { .. } => (!matches!(metadata.kind, "string" | "integer" | "number"))
                .then(|| format!("{path} must be a resource identifier")),
        }
    }
}
