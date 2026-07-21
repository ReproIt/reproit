//! Bounded normalization and pure evaluation for layout-overflow evidence.

use serde_json::Value;

pub(crate) const PROTOCOL_VERSION: u64 = 1;
pub(crate) const MAX_CHECKS: usize = 128;
const MAX_KEY_BYTES: usize = 256;
const MAX_COORDINATE: f64 = 1_000_000.0;
const EPSILON_PX: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverflowOutcome {
    Violation,
    Satisfied,
    Abstain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OverflowCheck {
    pub subject_key: String,
    pub container_key: String,
    pub fingerprint: String,
    pub outcome: OverflowOutcome,
    pub reason: String,
    pub spill_x_centipx: i64,
    pub spill_y_centipx: i64,
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Rect {
    fn parse(value: Option<&Value>) -> Option<Self> {
        let value = value?;
        let number = |name| value.get(name)?.as_f64();
        let rect = Self {
            left: number("left")?,
            top: number("top")?,
            right: number("right")?,
            bottom: number("bottom")?,
        };
        let coordinates = [rect.left, rect.top, rect.right, rect.bottom];
        if coordinates
            .iter()
            .any(|coordinate| !coordinate.is_finite() || coordinate.abs() > MAX_COORDINATE)
            || rect.right <= rect.left
            || rect.bottom <= rect.top
        {
            return None;
        }
        Some(rect)
    }

    fn spill(self, container: Self) -> (f64, f64) {
        let horizontal = (container.left - self.left)
            .max(self.right - container.right)
            .max(0.0);
        let vertical = (container.top - self.top)
            .max(self.bottom - container.bottom)
            .max(0.0);
        (horizontal, vertical)
    }
}

fn bounded_key(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty() && value.len() <= MAX_KEY_BYTES).then(|| value.to_string())
}

fn fingerprint(subject: &str, container: &str) -> String {
    let material = format!("overflow\0{subject}\0{container}");
    let digest = crate::domain::hash::sha256_hex(material.as_bytes());
    format!("sha256:{}", &digest[..24])
}

fn abstain(subject_key: String, container_key: String, reason: &str) -> OverflowCheck {
    OverflowCheck {
        fingerprint: fingerprint(&subject_key, &container_key),
        subject_key,
        container_key,
        outcome: OverflowOutcome::Abstain,
        reason: reason.to_string(),
        spill_x_centipx: 0,
        spill_y_centipx: 0,
    }
}

/// Parse and evaluate one versioned marker. The adapter supplies facts only.
/// This function owns the verdict and refuses incomplete or ambiguous evidence.
pub(crate) fn evaluate_marker(value: &Value) -> Vec<OverflowCheck> {
    if value.get("version").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
        return Vec::new();
    }
    let Some(items) = value.get("checks").and_then(Value::as_array) else {
        return Vec::new();
    };
    if value.get("complete").and_then(Value::as_bool) != Some(true) || items.len() > MAX_CHECKS {
        return Vec::new();
    }
    items.iter().filter_map(evaluate_item).collect()
}

fn evaluate_item(item: &Value) -> Option<OverflowCheck> {
    let subject_key = bounded_key(item.get("subjectKey"))?;
    let container_key = bounded_key(item.get("containerKey"))?;
    if item.get("authority").and_then(Value::as_str) != Some("exact-layout") {
        return Some(abstain(subject_key, container_key, "unsupported-authority"));
    }
    if item.get("ownership").and_then(Value::as_str) != Some("app") {
        return Some(abstain(subject_key, container_key, "unproven-ownership"));
    }
    if item
        .get("stableSamples")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        < 2
    {
        return Some(abstain(subject_key, container_key, "unstable-layout"));
    }
    if item.get("transformed").and_then(Value::as_bool) != Some(false) {
        return Some(abstain(subject_key, container_key, "transformed-layout"));
    }
    let policy = item.get("policy").and_then(Value::as_str).unwrap_or("");
    if matches!(policy, "scroll" | "truncate") {
        return Some(abstain(
            subject_key,
            container_key,
            "intentional-overflow-policy",
        ));
    }
    if policy != "contain" {
        return Some(abstain(subject_key, container_key, "unproven-containment"));
    }
    let Some(subject) = Rect::parse(item.get("subjectRect")) else {
        return Some(abstain(
            subject_key,
            container_key,
            "invalid-subject-geometry",
        ));
    };
    let Some(container) = Rect::parse(item.get("containerRect")) else {
        return Some(abstain(
            subject_key,
            container_key,
            "invalid-container-geometry",
        ));
    };
    let (spill_x, spill_y) = subject.spill(container);
    let outcome = if spill_x > EPSILON_PX || spill_y > EPSILON_PX {
        OverflowOutcome::Violation
    } else {
        OverflowOutcome::Satisfied
    };
    let reason = match (spill_x > EPSILON_PX, spill_y > EPSILON_PX) {
        (true, true) => "content-outside-container-xy",
        (true, false) => "content-outside-container-x",
        (false, true) => "content-outside-container-y",
        (false, false) => "within-container",
    };
    Some(OverflowCheck {
        fingerprint: fingerprint(&subject_key, &container_key),
        subject_key,
        container_key,
        outcome,
        reason: reason.to_string(),
        spill_x_centipx: (spill_x * 100.0).round() as i64,
        spill_y_centipx: (spill_y * 100.0).round() as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn marker(subject: Value) -> Value {
        json!({
            "version": 1,
            "complete": true,
            "checks": [{
                "subjectKey": "key:id:message",
                "containerKey": "key:id:card",
                "authority": "exact-layout",
                "ownership": "app",
                "stableSamples": 2,
                "transformed": false,
                "policy": "contain",
                "subjectRect": subject,
                "containerRect": {"left": 0.0, "top": 0.0, "right": 100.0, "bottom": 40.0}
            }]
        })
    }

    #[test]
    fn confirms_stable_app_owned_content_outside_declared_container() {
        let checks = evaluate_marker(&marker(json!({
            "left": 4.0, "top": 4.0, "right": 108.0, "bottom": 36.0
        })));
        assert_eq!(checks[0].outcome, OverflowOutcome::Violation);
        assert_eq!(checks[0].spill_x_centipx, 800);
    }

    #[test]
    fn records_satisfied_for_exact_replay() {
        let checks = evaluate_marker(&marker(json!({
            "left": 4.0, "top": 4.0, "right": 96.0, "bottom": 36.0
        })));
        assert_eq!(checks[0].outcome, OverflowOutcome::Satisfied);
    }

    #[test]
    fn ambiguous_policy_abstains() {
        let mut value = marker(json!({
            "left": 4.0, "top": 4.0, "right": 108.0, "bottom": 36.0
        }));
        value["checks"][0]["policy"] = json!("visible");
        assert_eq!(evaluate_marker(&value)[0].outcome, OverflowOutcome::Abstain);
    }

    #[test]
    fn oversized_batch_is_rejected_as_one_defect() {
        let item = marker(json!({
            "left": 4.0, "top": 4.0, "right": 96.0, "bottom": 36.0
        }))["checks"][0]
            .clone();
        let value = json!({"version": 1, "checks": vec![item; MAX_CHECKS + 1]});
        assert!(evaluate_marker(&value).is_empty());
    }
}
