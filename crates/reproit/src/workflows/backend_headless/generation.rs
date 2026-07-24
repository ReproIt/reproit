use super::*;

pub(super) const MAX_INVALID_PROBES_PER_OPERATION: usize = 8;

/// Contract-invalid inputs for one operation: each probe is a full valid
/// sample with exactly one declared body field replaced by a wrong JSON type
/// (present-but-wrong optional fields included). Only body-carried fields are
/// mutated; path, query, and header values serialize as text, so a wrong JSON
/// type is not observable at the transport. Every returned input is proven
/// out-of-domain, deterministic for a given `seed`, and capped at
/// `MAX_INVALID_PROBES_PER_OPERATION`.
pub(super) fn invalid_probes(domain: &ValueDomain, seed: u64, body_only: bool) -> Vec<Value> {
    let target = if body_only {
        Some(domain)
    } else if let ValueDomain::Object { properties, .. } = domain {
        properties.get("body")
    } else {
        None
    };
    let Some(ValueDomain::Object { properties, .. }) = target else {
        return Vec::new();
    };
    let mut probes = Vec::new();
    for (name, property) in properties {
        if probes.len() >= MAX_INVALID_PROBES_PER_OPERATION {
            break;
        }
        let Some(wrong) = wrong_typed_value(property, seed) else {
            continue;
        };
        let mut input = sample_domain(domain, seed, true, 0);
        let slot = if body_only {
            Some(&mut input)
        } else {
            input.get_mut("body")
        };
        let Some(object) = slot.and_then(Value::as_object_mut) else {
            break;
        };
        object.insert(name.clone(), wrong);
        // Send only inputs the contract provably rejects; anything else would
        // be ordinary valid traffic mislabeled as a probe.
        if domain.mismatch(&input, "$input").is_some() {
            probes.push(input);
        }
    }
    probes
}

/// A value guaranteed to fall outside `domain`, or `None` when the domain has
/// no definite type to violate (`Any`, compositions, and literals).
fn wrong_typed_value(domain: &ValueDomain, seed: u64) -> Option<Value> {
    Some(match domain {
        ValueDomain::String { .. } => Value::from(seed.max(1)),
        ValueDomain::Null
        | ValueDomain::Boolean
        | ValueDomain::Integer { .. }
        | ValueDomain::Number
        | ValueDomain::Array { .. }
        | ValueDomain::Object { .. } => Value::String(format!("reproit-wrong-type-{seed}")),
        ValueDomain::ProtoInteger64 { .. } | ValueDomain::Resource { .. } => Value::Bool(true),
        ValueDomain::Any
        | ValueDomain::OneOf { .. }
        | ValueDomain::AllOf { .. }
        | ValueDomain::GraphqlAbstract { .. }
        | ValueDomain::Literal { .. } => return None,
    })
}

pub(super) fn sample_domain(
    domain: &ValueDomain,
    seed: u64,
    include_optional: bool,
    depth: usize,
) -> Value {
    if depth > MAX_GENERATED_VALUE_DEPTH {
        return Value::Null;
    }
    match domain {
        ValueDomain::Any => Value::Null,
        ValueDomain::Null => Value::Null,
        ValueDomain::Boolean => Value::Bool(seed.is_multiple_of(2)),
        ValueDomain::Integer { min, max } => {
            let value = min.unwrap_or(1).max(0);
            Value::from(max.map_or(value, |maximum| value.min(maximum)))
        }
        ValueDomain::ProtoInteger64 { .. } => Value::String((seed.max(1)).to_string()),
        ValueDomain::Number => Value::from(seed.max(1) as f64),
        ValueDomain::String {
            min_length,
            max_length,
            format,
            variants,
            ..
        } => {
            if let Some(value) = variants.first() {
                return Value::String(value.clone());
            }
            let base = match format.as_deref() {
                Some("date-time") => "2026-01-01T00:00:00Z".to_string(),
                Some("date") => "2026-01-01".to_string(),
                Some("uuid") => format!("00000000-0000-4000-8000-{seed:012x}"),
                Some("email") => format!("reproit-{seed}@example.test"),
                Some("uri" | "url") => format!("https://example.test/{seed}"),
                _ => format!("reproit-{seed}"),
            };
            let minimum = min_length.unwrap_or(0).min(MAX_GENERATED_STRING_CHARS);
            let maximum = max_length.unwrap_or(usize::MAX);
            let mut value = base;
            while value.chars().count() < minimum {
                value.push('x');
            }
            if value.chars().count() > maximum {
                value = value.chars().take(maximum).collect();
            }
            Value::String(value)
        }
        ValueDomain::Array {
            items,
            min_items,
            max_items,
            ..
        } => {
            let desired = if include_optional {
                min_items.unwrap_or(1).max(1)
            } else {
                min_items.unwrap_or(0)
            };
            let count = max_items
                .map_or(desired, |maximum| desired.min(maximum))
                .min(MAX_GENERATED_ARRAY_ITEMS);
            Value::Array(
                (0..count)
                    .map(|index| {
                        sample_domain(
                            items,
                            seed.saturating_add(index as u64),
                            include_optional,
                            depth + 1,
                        )
                    })
                    .collect(),
            )
        }
        ValueDomain::Object {
            required,
            properties,
            ..
        } => Value::Object(
            properties
                .iter()
                .filter(|(name, _)| include_optional || required.contains(*name))
                .map(|(name, property)| {
                    (
                        name.clone(),
                        sample_domain(property, seed, include_optional, depth + 1),
                    )
                })
                .collect(),
        ),
        ValueDomain::OneOf { variants } => variants
            .first()
            .map(|variant| sample_domain(variant, seed, include_optional, depth + 1))
            .unwrap_or(Value::Null),
        ValueDomain::AllOf { variants } => variants
            .first()
            .map(|variant| sample_domain(variant, seed, include_optional, depth + 1))
            .unwrap_or(Value::Null),
        ValueDomain::GraphqlAbstract { variants } => variants
            .values()
            .next()
            .map(|variant| sample_domain(variant, seed, include_optional, depth + 1))
            .unwrap_or(Value::Null),
        ValueDomain::Literal { value } => value.clone(),
        ValueDomain::Resource { .. } => Value::String(format!("reproit-{seed}")),
    }
}
