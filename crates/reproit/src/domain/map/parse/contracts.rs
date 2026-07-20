//! Authored relationship and accessibility-state marker reduction.

use super::{AccessibilityStateCheck, RelationCheck, RelationViolation, RunObs};
use serde_json::Value;

pub(super) fn absorb(obs: &mut RunObs, line: &str) -> bool {
    if line.starts_with("EXPLORE:RELATION ") {
        if let Some(json) = super::extract(line, "EXPLORE:RELATION ") {
            absorb_violations(obs, &json);
        }
        return true;
    }
    if line.starts_with("EXPLORE:RELATIONSTATUS ") {
        if let Some(json) = super::extract(line, "EXPLORE:RELATIONSTATUS ") {
            absorb_relation_checks(obs, &json);
        }
        return true;
    }
    if line.starts_with("EXPLORE:A11YSTATESTATUS ") {
        if let Some(json) = super::extract(line, "EXPLORE:A11YSTATESTATUS ") {
            absorb_accessibility_checks(obs, &json);
        }
        return true;
    }
    false
}

fn absorb_violations(obs: &mut RunObs, json: &Value) {
    let (Some(sig), Some(items)) = (
        json.get("sig").and_then(Value::as_str),
        json.get("items").and_then(Value::as_array),
    ) else {
        return;
    };
    let violations = items
        .iter()
        .filter_map(parse_relation_violation)
        .collect::<Vec<_>>();
    if !violations.is_empty() {
        obs.relations.insert(sig.to_string(), violations);
    }
}

fn parse_relation_violation(item: &Value) -> Option<RelationViolation> {
    let kind = item.get("kind").and_then(Value::as_str)?;
    let dependent_key = item.get("dependentKey").and_then(Value::as_str)?;
    let owner_key = item.get("ownerKey").and_then(Value::as_str)?;
    let container_key = item.get("containerKey").and_then(Value::as_str)?;
    let violation = item.get("violation").and_then(Value::as_str)?;
    if kind != "indicator-anchor"
        || !matches!(violation, "detached" | "escaped-container")
        || dependent_key.is_empty()
        || owner_key.is_empty()
        || container_key.is_empty()
    {
        return None;
    }
    let max_gap = item.get("maxGap").and_then(Value::as_i64).unwrap_or(8);
    if !(0..=64).contains(&max_gap) {
        return None;
    }
    let gap = item.get("gap").and_then(Value::as_f64).unwrap_or(0.0);
    Some(RelationViolation {
        kind: kind.to_string(),
        dependent_key: dependent_key.to_string(),
        owner_key: owner_key.to_string(),
        container_key: container_key.to_string(),
        violation: violation.to_string(),
        max_gap,
        gap_centipx: (gap * 100.0).round() as i64,
    })
}

fn absorb_relation_checks(obs: &mut RunObs, json: &Value) {
    let (Some(sig), Some(checks)) = (
        json.get("sig").and_then(Value::as_str),
        json.get("checks").and_then(Value::as_array),
    ) else {
        return;
    };
    let checks = checks
        .iter()
        .filter_map(parse_relation_check)
        .collect::<Vec<_>>();
    obs.relation_checks.insert(sig.to_string(), checks);
}

fn parse_relation_check(item: &Value) -> Option<RelationCheck> {
    let kind = item.get("kind").and_then(Value::as_str)?;
    let dependent_key = item.get("dependentKey").and_then(Value::as_str)?;
    let owner_key = item.get("ownerKey").and_then(Value::as_str)?;
    let container_key = item.get("containerKey").and_then(Value::as_str)?;
    let outcome = item.get("outcome").and_then(Value::as_str)?;
    if kind != "indicator-anchor"
        || !matches!(outcome, "VIOLATION" | "SATISFIED")
        || dependent_key.is_empty()
        || owner_key.is_empty()
        || container_key.is_empty()
    {
        return None;
    }
    let violation = item
        .get("violation")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "detached" | "escaped-container"))
        .map(str::to_string);
    if outcome == "VIOLATION" && violation.is_none() {
        return None;
    }
    Some(RelationCheck {
        kind: kind.to_string(),
        dependent_key: dependent_key.to_string(),
        owner_key: owner_key.to_string(),
        container_key: container_key.to_string(),
        outcome: outcome.to_string(),
        violation,
    })
}

fn absorb_accessibility_checks(obs: &mut RunObs, json: &Value) {
    let (Some(sig), Some(checks)) = (
        json.get("sig").and_then(Value::as_str),
        json.get("checks").and_then(Value::as_array),
    ) else {
        return;
    };
    let checks = checks
        .iter()
        .filter_map(parse_accessibility_check)
        .collect::<Vec<_>>();
    obs.accessibility_state_checks
        .insert(sig.to_string(), checks);
}

fn parse_accessibility_check(item: &Value) -> Option<AccessibilityStateCheck> {
    let identity = item.get("identity").and_then(Value::as_str)?;
    let property = item.get("property").and_then(Value::as_str)?;
    let fingerprint = item.get("fingerprint").and_then(Value::as_str)?;
    let expected = item.get("expected").and_then(Value::as_str)?;
    let outcome = item.get("outcome").and_then(Value::as_str)?;
    if !identity.starts_with("key:id:")
        || !matches!(property, "checked" | "disabled" | "expanded" | "selected")
        || !fingerprint.starts_with("sha256:")
        || fingerprint.len() != 31
        || !fingerprint[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !matches!(expected, "true" | "false" | "mixed")
        || !matches!(outcome, "VIOLATION" | "SATISFIED" | "ABSTAIN")
    {
        return None;
    }
    let actual = item
        .get("actual")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "true" | "false" | "mixed"))
        .map(str::to_string);
    let reason = item
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    if matches!(outcome, "VIOLATION" | "SATISFIED") && actual.is_none() {
        return None;
    }
    if outcome == "VIOLATION" && reason.as_deref() != Some("semantic-state-mismatch") {
        return None;
    }
    Some(AccessibilityStateCheck {
        identity: identity.to_string(),
        property: property.to_string(),
        fingerprint: fingerprint.to_ascii_lowercase(),
        expected: expected.to_string(),
        actual,
        outcome: outcome.to_string(),
        reason,
    })
}
