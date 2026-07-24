//! Rendering and transcript evidence for backend inspection.
//!
//! Bounded by construction: the planner caps steps at `MAX_INSPECT_STEPS`,
//! every embedded JSON value is truncated to `MAX_VALUE_CHARS` with an
//! explicit marker, effect lists are capped, and the final transcript is
//! refused (replaced by a truncation marker) if it would ever exceed the
//! same 4 MiB evidence bound the UI inspection path uses.

use super::*;
use crate::domain::backend::PendingObligation;

/// Mirrors the UI inspection bounds (`workflows/inspect.rs`).
pub(super) const MAX_INSPECT_STEPS: usize = 128;
const MAX_TRANSCRIPT_BYTES: usize = 4 * 1024 * 1024;
const MAX_VALUE_CHARS: usize = 2_048;
const MAX_EFFECTS_SHOWN: usize = 64;
const MAX_VIOLATIONS_SHOWN: usize = 8;

/// What one executed (or revealed) member of a step looked like.
pub(super) struct MemberOutcome {
    pub(super) operation: String,
    /// `METHOD url` for live sends, `recorded` for offline reveals.
    pub(super) request_line: String,
    pub(super) input: Value,
    pub(super) status: Option<u16>,
    pub(super) output: Value,
    /// Effect identities observed in this member (live trail or recording).
    pub(super) observed_effects: Vec<String>,
    /// Live only: diff of the live trail against the recorded invocation.
    pub(super) diff: Option<EffectDiff>,
    /// Why no live effect trail is shown, when it is not.
    pub(super) trail_note: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct EffectDiff {
    pub(super) matched: Vec<String>,
    pub(super) missing: Vec<String>,
    pub(super) unexpected: Vec<String>,
}

/// Identity of one effect event for matching live against recorded evidence:
/// kind + resource + key + tenant. Non-effect events have no identity.
pub(super) fn effect_identity(event: &BackendEvent) -> Option<String> {
    let BackendEventKind::Effect {
        effect,
        resource,
        key,
        tenant,
        ..
    } = &event.event
    else {
        return None;
    };
    let kind = serde_json::to_value(effect)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{effect:?}").to_ascii_lowercase());
    Some(format!(
        "{kind}:{}:{}:{}",
        resource.as_deref().unwrap_or("-"),
        key.as_deref().unwrap_or("-"),
        tenant.as_deref().unwrap_or("-"),
    ))
}

pub(super) fn effect_identities(events: &[BackendEvent]) -> Vec<String> {
    events.iter().filter_map(effect_identity).collect()
}

/// Multiset diff of the live effect trail against the recorded invocation:
/// matched (in both), missing (recorded but not observed live), unexpected
/// (observed live but never recorded).
pub(super) fn diff_effects(recorded: &[BackendEvent], live: &[BackendEvent]) -> EffectDiff {
    let mut remaining = BTreeMap::<String, usize>::new();
    for identity in effect_identities(recorded) {
        *remaining.entry(identity).or_default() += 1;
    }
    let mut diff = EffectDiff::default();
    for identity in effect_identities(live) {
        match remaining.get_mut(&identity) {
            Some(count) if *count > 0 => {
                *count -= 1;
                diff.matched.push(identity);
            }
            _ => diff.unexpected.push(identity),
        }
    }
    for (identity, count) in remaining {
        for _ in 0..count {
            diff.missing.push(identity.clone());
        }
    }
    diff
}

/// Additions and removals between two pending-obligation snapshots.
pub(super) fn pending_delta(
    before: &[PendingObligation],
    after: &[PendingObligation],
) -> (Vec<String>, Vec<String>) {
    let summarize = |items: &[PendingObligation]| {
        items
            .iter()
            .map(|item| format!("[{}] {}", item.oracle, item.summary))
            .collect::<Vec<_>>()
    };
    let (before, after) = (summarize(before), summarize(after));
    let added = after
        .iter()
        .filter(|item| !before.contains(item))
        .cloned()
        .collect();
    let removed = before
        .iter()
        .filter(|item| !after.contains(item))
        .cloned()
        .collect();
    (added, removed)
}

/// A JSON value bounded for evidence: values whose serialization exceeds the
/// cap are replaced by a preview string with an explicit truncation marker.
pub(super) fn bounded_value(value: &Value) -> Value {
    let serialized = value.to_string();
    if serialized.chars().count() <= MAX_VALUE_CHARS {
        return value.clone();
    }
    let preview: String = serialized.chars().take(MAX_VALUE_CHARS).collect();
    Value::String(format!(
        "{preview}... [truncated: {} bytes total]",
        serialized.len()
    ))
}

fn bounded_list(items: &[String]) -> Vec<String> {
    let mut shown: Vec<String> = items.iter().take(MAX_EFFECTS_SHOWN).cloned().collect();
    if items.len() > MAX_EFFECTS_SHOWN {
        shown.push(format!(
            "... [{} more truncated]",
            items.len() - MAX_EFFECTS_SHOWN
        ));
    }
    shown
}

/// The accumulated inspection transcript, written as bounded machine JSON and
/// a self-contained human Markdown report suitable for attaching to an issue.
pub(super) struct Transcript {
    mode: &'static str,
    source: String,
    reference: String,
    expected: String,
    steps: Vec<Value>,
}

impl Transcript {
    pub(super) fn new(
        mode: &'static str,
        source: String,
        reference: String,
        expected: String,
    ) -> Self {
        Self {
            mode,
            source,
            reference,
            expected,
            steps: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_step(
        &mut self,
        index: usize,
        total: usize,
        label: &str,
        grouped: bool,
        members: &[MemberOutcome],
        pending: &[PendingObligation],
        delta: &(Vec<String>, Vec<String>),
        prefix_violations: &[BackendViolation],
        fired: Option<&BackendViolation>,
    ) {
        if self.steps.len() >= MAX_INSPECT_STEPS {
            return; // planner already bounds steps; fail closed regardless.
        }
        let members: Vec<Value> = members
            .iter()
            .map(|member| {
                json!({
                    "operation": member.operation,
                    "request": member.request_line,
                    "input": bounded_value(&member.input),
                    "status": member.status,
                    "output": bounded_value(&member.output),
                    "effectsObserved": bounded_list(&member.observed_effects),
                    "effectDiff": member.diff.as_ref().map(|diff| json!({
                        "matched": bounded_list(&diff.matched),
                        "missing": bounded_list(&diff.missing),
                        "unexpected": bounded_list(&diff.unexpected),
                    })),
                    "trailNote": member.trail_note,
                })
            })
            .collect();
        self.steps.push(json!({
            "index": index,
            "total": total,
            "label": label,
            "grouped": grouped,
            "members": members,
            "oracleState": {
                "pending": pending
                    .iter()
                    .take(MAX_EFFECTS_SHOWN)
                    .map(|item| format!("[{}] {}", item.oracle, item.summary))
                    .collect::<Vec<_>>(),
                "added": bounded_list(&delta.0),
                "resolved": bounded_list(&delta.1),
            },
            "prefixViolations": prefix_violations
                .iter()
                .take(MAX_VIOLATIONS_SHOWN)
                .map(|violation| json!({
                    "oracle": public_oracle(violation),
                    "operation": violation.operation,
                    "fingerprint": violation.fingerprint,
                }))
                .collect::<Vec<_>>(),
            "violation": fired.map(|violation| json!({
                "oracle": public_oracle(violation),
                "operation": violation.operation,
                "reason": violation.reason,
                "fingerprint": violation.fingerprint,
            })),
        }));
    }

    /// Write `inspect-transcript.json` and `inspect-transcript.md` into a
    /// fresh `.reproit/runs/backend-inspect-<stamp>/` evidence directory.
    pub(super) fn write(
        self,
        root: &Path,
        verdict: &str,
        reproduced: bool,
    ) -> Result<(PathBuf, PathBuf)> {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        let directory = root
            .join(".reproit/runs")
            .join(format!("backend-inspect-{stamp}"));
        std::fs::create_dir_all(&directory)?;
        let mut packet = json!({
            "format": "reproit-backend-inspect-transcript",
            "version": 1,
            "mode": self.mode,
            "source": self.source,
            "reference": self.reference,
            "expected": self.expected,
            "verdict": verdict,
            "reproduced": reproduced,
            "steps": self.steps,
        });
        let mut bytes = serde_json::to_vec_pretty(&packet)?;
        if bytes.len() > MAX_TRANSCRIPT_BYTES {
            packet["steps"] = Value::String(format!(
                "[truncated: {} steps exceeded the {MAX_TRANSCRIPT_BYTES} byte evidence bound]",
                self.steps.len()
            ));
            bytes = serde_json::to_vec_pretty(&packet)?;
        }
        let json_path = directory.join("inspect-transcript.json");
        std::fs::write(&json_path, bytes)
            .with_context(|| format!("writing {}", json_path.display()))?;
        let markdown_path = directory.join("inspect-transcript.md");
        std::fs::write(&markdown_path, render_markdown(&packet))
            .with_context(|| format!("writing {}", markdown_path.display()))?;
        Ok((markdown_path, json_path))
    }
}

/// The registry-facing oracle id of a violation (`backend-server-error`),
/// matching what findings and the console output report.
fn public_oracle(violation: &BackendViolation) -> String {
    backend::finding(violation)["oracle"]
        .as_str()
        .unwrap_or(&violation.oracle)
        .to_string()
}

fn render_markdown(packet: &Value) -> String {
    let mut text = format!(
        "# ReproIt backend inspection\n\nMode: **{}**\n\nSource: {}\n\nReference: `{}`\n\n\
         Expected: {}\n\nVerdict: **{}**\n\n## Steps\n\n",
        packet["mode"].as_str().unwrap_or("unknown"),
        packet["source"].as_str().unwrap_or("unknown"),
        packet["reference"].as_str().unwrap_or("unknown"),
        packet["expected"].as_str().unwrap_or("unknown"),
        packet["verdict"].as_str().unwrap_or("unknown"),
    );
    let Some(steps) = packet["steps"].as_array() else {
        text.push_str(packet["steps"].as_str().unwrap_or("(no steps recorded)"));
        text.push('\n');
        return text;
    };
    for step in steps {
        let violating = step["violation"].is_object();
        let marker = if violating { " **VIOLATION**" } else { "" };
        let grouped = if step["grouped"].as_bool() == Some(true) {
            " (concurrent group)"
        } else {
            ""
        };
        text.push_str(&format!(
            "### Step {}/{}: {}{grouped}{marker}\n\n",
            step["index"].as_u64().unwrap_or(0),
            step["total"].as_u64().unwrap_or(0),
            step["label"].as_str().unwrap_or("?"),
        ));
        for member in step["members"].as_array().into_iter().flatten() {
            text.push_str(&format!(
                "- request: `{}`\n- input: `{}`\n- response: {} `{}`\n",
                member["request"].as_str().unwrap_or("?"),
                compact(&member["input"]),
                member["status"]
                    .as_u64()
                    .map_or("(none)".into(), |status| status.to_string()),
                compact(&member["output"]),
            ));
            markdown_effects(&mut text, member);
        }
        markdown_oracle_state(&mut text, step);
        if let Some(violation) = step["violation"].as_object() {
            text.push_str(&format!(
                "- **violation**: `{}` on `{}`: {}\n- fingerprint: `{}`\n",
                violation
                    .get("oracle")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                violation
                    .get("operation")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                violation
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                violation
                    .get("fingerprint")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
            ));
        }
        text.push('\n');
    }
    text
}

fn markdown_effects(text: &mut String, member: &Value) {
    let observed = string_list(&member["effectsObserved"]);
    if !observed.is_empty() {
        text.push_str(&format!("- effects observed: {}\n", observed.join(", ")));
    }
    if let Some(diff) = member["effectDiff"].as_object() {
        for (name, title) in [
            ("matched", "matched recorded effects"),
            ("missing", "recorded but missing live"),
            ("unexpected", "live but never recorded"),
        ] {
            let items = string_list(&diff[name]);
            if !items.is_empty() {
                text.push_str(&format!("- {title}: {}\n", items.join(", ")));
            }
        }
    }
    if let Some(note) = member["trailNote"].as_str() {
        text.push_str(&format!("- effect trail: {note}\n"));
    }
}

fn markdown_oracle_state(text: &mut String, step: &Value) {
    for (name, prefix) in [("added", "+"), ("resolved", "-")] {
        for item in string_list(&step["oracleState"][name]) {
            text.push_str(&format!("- oracle state {prefix} {item}\n"));
        }
    }
    let pending = string_list(&step["oracleState"]["pending"]);
    if !pending.is_empty() {
        text.push_str(&format!("- pending after step: {}\n", pending.join("; ")));
    }
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|item| format!("`{}`", item.replace('`', "'")))
        .collect()
}

fn compact(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mut compacted: String = text.chars().take(200).collect();
    if text.chars().count() > 200 {
        compacted.push_str("...");
    }
    compacted.replace('`', "'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn effect(resource: &str, key: &str) -> BackendEvent {
        serde_json::from_value(json!({
            "sequence": 1, "traceId": "t", "spanId": "s", "operation": "op",
            "kind": "effect", "effect": "write", "resource": resource, "key": key,
        }))
        .expect("test effect")
    }

    #[test]
    fn effect_diff_is_a_multiset_over_kind_resource_key_tenant() {
        let recorded = vec![
            effect("orders", "o1"),
            effect("orders", "o1"),
            effect("audit", "a"),
        ];
        let live = vec![effect("orders", "o1"), effect("mail", "m")];
        let diff = diff_effects(&recorded, &live);
        assert_eq!(diff.matched, vec!["write:orders:o1:-"]);
        assert_eq!(diff.missing, vec!["write:audit:a:-", "write:orders:o1:-"]);
        assert_eq!(diff.unexpected, vec!["write:mail:m:-"]);
    }

    #[test]
    fn oversized_values_truncate_with_an_explicit_marker() {
        let value = json!({"blob": "x".repeat(10_000)});
        let bounded = bounded_value(&value);
        let text = bounded.as_str().expect("truncated to a string");
        assert!(text.contains("[truncated:"));
        assert!(text.chars().count() < 2_200);
        // Small values pass through untouched.
        assert_eq!(bounded_value(&json!({"a": 1})), json!({"a": 1}));
    }

    #[test]
    fn transcript_marks_the_violating_step_and_stays_bounded() {
        let mut transcript = Transcript::new(
            "live",
            "capture x".into(),
            "capture.json".into(),
            "backend-server-error on createOrder".into(),
        );
        let member = MemberOutcome {
            operation: "createOrder".into(),
            request_line: "POST http://127.0.0.1:1/orders".into(),
            input: json!({"body": {"blob": "y".repeat(9_000)}}),
            status: Some(500),
            output: json!({"error": "internal"}),
            observed_effects: vec!["read:inventory:widget:-".into()],
            diff: Some(EffectDiff {
                matched: vec!["read:inventory:widget:-".into()],
                missing: Vec::new(),
                unexpected: Vec::new(),
            }),
            trail_note: None,
        };
        let violation = BackendViolation {
            operation: "createOrder".into(),
            contract_hash: "h".into(),
            fingerprint: "f".repeat(20),
            oracle: "server-error".into(),
            reason: "contract-valid request returned HTTP 500".into(),
            trace_id: "t".into(),
            span_id: "s".into(),
            action_index: 1,
        };
        transcript.record_step(
            1,
            1,
            "createOrder",
            false,
            std::slice::from_ref(&member),
            &[],
            &(Vec::new(), Vec::new()),
            std::slice::from_ref(&violation),
            Some(&violation),
        );
        let root = std::env::temp_dir().join(format!("reproit-inspect-md-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        let (markdown_path, json_path) = transcript
            .write(&root, "reproduced at step 1", true)
            .expect("transcript written");
        let markdown = std::fs::read_to_string(&markdown_path).expect("markdown");
        assert!(markdown.contains("**VIOLATION**"));
        assert!(markdown.contains("matched recorded effects"));
        let packet: Value =
            serde_json::from_slice(&std::fs::read(&json_path).expect("json")).expect("parse");
        assert_eq!(packet["reproduced"], json!(true));
        let input = packet["steps"][0]["members"][0]["input"]
            .as_str()
            .expect("oversized input truncated");
        assert!(input.contains("[truncated:"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
