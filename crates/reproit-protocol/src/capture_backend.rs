//! Projection of a backend SDK capture batch back to the
//! `reproit-backend-capture` v1 payload that offline replay evaluates.
//!
//! The backend SDKs emit one universal capture batch per failed operation:
//! `operation-start`, a `trigger` nesting the raw start input, `effect`
//! events nesting the raw effect events verbatim, an `operation-return`
//! effect nesting the raw `return` event, `operation-end`, and an
//! `observation` carrying the oracle signature. This module inverts that
//! emission so a pulled Cloud occurrence can be re-evaluated locally.

use crate::{CaptureBatch, CaptureEventKind, CapturedValue};
use serde_json::{json, Map, Value};

/// Effect subject the backend SDKs use to nest the raw `return` event in a
/// capture batch, next to the effect events that nest raw effects.
pub const OPERATION_RETURN_SUBJECT: &str = "operation-return";

const BACKEND_CAPTURE_FORMAT: &str = "reproit-backend-capture";
const BACKEND_CAPTURE_VERSION: u16 = 1;
/// Version stamped when any projected effect event carries a captured
/// dependency `exchange`. Older CLIs reject it with a named version error
/// instead of silently evaluating a payload whose replay semantics they do
/// not understand.
const BACKEND_CAPTURE_VERSION_EXCHANGES: u16 = 2;
const SERVER_ERROR_ORACLE: &str = "backend-server-error";
/// Checkpoint name the backend SDKs use for the determinism envelope
/// (capture wall-clock, timezone, runtime identity, replay seed).
pub const DETERMINISM_ENVELOPE_CHECKPOINT: &str = "determinism-envelope";

/// Envelope fields every raw backend SDK event carries. The synthesized
/// `start` (and, for older batches, `return`) events copy them from a nested
/// raw event so replay groups the invocation under one (traceId, spanId) key.
const ENVELOPE_FIELDS: [&str; 10] = [
    "traceId",
    "spanId",
    "actionIndex",
    "operation",
    "build",
    "configContract",
    "actor",
    "tenant",
    "idempotencyKey",
    "selections",
];

/// Project a backend capture batch to the `reproit-backend-capture` v1
/// payload. Returns `None` when the batch is not a replayable backend
/// failure: no observation with a signature, no trigger, no operation, or an
/// older batch without a nested return event whose failure signature is not a
/// plain `backend-server-error`.
pub fn backend_capture_from_batch(batch: &CaptureBatch) -> Option<Value> {
    let mut operation = None;
    let mut trigger: Option<(Option<String>, Value)> = None;
    let mut oracle = None;
    let mut effects: Vec<Value> = Vec::new();
    let mut returned: Option<Value> = None;
    let mut envelope_attributes: Option<Value> = None;
    for event in &batch.events {
        match &event.event {
            CaptureEventKind::Checkpoint { name, attributes }
                if name == DETERMINISM_ENVELOPE_CHECKPOINT && envelope_attributes.is_none() =>
            {
                envelope_attributes = Some(attributes.clone());
            }
            CaptureEventKind::OperationStart { name } if operation.is_none() => {
                operation = Some(name.clone());
            }
            CaptureEventKind::Trigger { value, .. } if trigger.is_none() => {
                let input = replayable(value.as_ref()).unwrap_or(Value::Null);
                trigger = Some((event.trace_id.clone(), input));
            }
            CaptureEventKind::Effect { subject, value, .. } => {
                if subject == OPERATION_RETURN_SUBJECT {
                    if returned.is_none() {
                        returned = replayable(value.as_ref());
                    }
                } else {
                    effects.extend(replayable(value.as_ref()));
                }
            }
            // The SDK families that emit effects as dependency/state capture
            // events (Node and its ports) nest exchange-bearing raw events the
            // same way: unwrap them so the projection round-trips every
            // replayable effect regardless of the carrier kind.
            CaptureEventKind::Dependency { value, .. }
            | CaptureEventKind::StateAccess { value, .. } => {
                effects.extend(replayable(value.as_ref()));
            }
            CaptureEventKind::Observation { failure } if oracle.is_none() => {
                oracle = failure
                    .signature
                    .as_deref()
                    .map(|signature| signature.split(':').next().unwrap_or(signature).to_string());
            }
            _ => {}
        }
    }
    let operation = operation?;
    let oracle = oracle?;
    let (trace_hint, input) = trigger?;
    let raw_reference = returned.as_ref().or_else(|| effects.first());
    let envelope = envelope(batch, trace_hint.as_deref(), &operation, raw_reference);
    let first_sequence = raw_reference
        .and_then(|raw| raw.get("sequence").and_then(Value::as_u64))
        .unwrap_or(2);
    let mut start = envelope.clone();
    start.insert("sequence".into(), json!(first_sequence.saturating_sub(1)));
    start.insert("kind".into(), json!("start"));
    start.insert("input".into(), input);
    let returned = match returned {
        Some(raw) => raw,
        None => {
            // Older SDK batches never nested the raw return event. A plain
            // server error pins the return shape exactly; any other oracle
            // would need return fields the batch does not carry.
            if oracle != SERVER_ERROR_ORACLE {
                return None;
            }
            let last_sequence = effects
                .last()
                .and_then(|raw| raw.get("sequence").and_then(Value::as_u64))
                .unwrap_or(first_sequence);
            let mut synthesized = envelope;
            synthesized.insert("sequence".into(), json!(last_sequence.saturating_add(1)));
            synthesized.insert("kind".into(), json!("return"));
            synthesized.insert("status".into(), json!(500));
            synthesized.insert("success".into(), json!(false));
            synthesized.insert("effectsComplete".into(), json!(true));
            Value::Object(synthesized)
        }
    };
    let mut events = Vec::with_capacity(effects.len() + 2);
    events.push(Value::Object(start));
    events.extend(effects);
    events.push(returned);
    // Exchanges and envelope stamps both require version-2-aware consumers:
    // an older CLI must reject with a named version error rather than parse
    // events whose fields it does not know.
    let version = if events.iter().any(|event| {
        event.get("exchange").is_some_and(|value| !value.is_null())
            || event.get("at").is_some()
            || event.get("monoNs").is_some()
    }) {
        BACKEND_CAPTURE_VERSION_EXCHANGES
    } else {
        BACKEND_CAPTURE_VERSION
    };
    let mut payload = json!({
        "format": BACKEND_CAPTURE_FORMAT,
        "version": version,
        "operation": operation,
        "oracle": oracle,
        "events": events,
    });
    if let Some(attributes) = envelope_attributes {
        payload["envelope"] = attributes;
    }
    Some(payload)
}

fn replayable(value: Option<&CapturedValue>) -> Option<Value> {
    match value {
        Some(CapturedValue::Replayable { value, .. }) => Some(value.clone()),
        _ => None,
    }
}

/// Build the raw-event envelope for synthesized events: defaults derived from
/// the batch, overlaid with whatever a nested raw event actually carried.
fn envelope(
    batch: &CaptureBatch,
    trace_hint: Option<&str>,
    operation: &str,
    raw: Option<&Value>,
) -> Map<String, Value> {
    let trace = trace_hint.unwrap_or(&batch.session_id);
    let mut fields = Map::from_iter([
        ("traceId".into(), json!(trace)),
        ("spanId".into(), json!(format!("{trace}:{operation}"))),
        ("actionIndex".into(), json!(0)),
        ("operation".into(), json!(operation)),
    ]);
    if let Some(Value::Object(raw)) = raw {
        for field in ENVELOPE_FIELDS {
            if let Some(value) = raw.get(field) {
                fields.insert(field.into(), value.clone());
            }
        }
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A batch in the exact shape the Rust backend SDK emitter produces for a
    /// failed `createOrder` (see `build_batch` in `reproit-backend`), with
    /// the raw start/effect/return events nested under the capture events.
    fn sdk_batch(with_return_carrier: bool) -> CaptureBatch {
        let raw_effect = json!({
            "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder", "actionIndex": 0,
            "operation": "createOrder", "build": "1.2.3", "sequence": 2,
            "kind": "effect", "effect": "read", "resource": "inventory", "key": "widget",
        });
        let raw_return = json!({
            "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder", "actionIndex": 0,
            "operation": "createOrder", "build": "1.2.3", "sequence": 3,
            "kind": "return", "output": {"error": "boom"},
            "status": 500, "success": false, "effectsComplete": true,
        });
        let mut events = vec![
            json!({"kind": "operation-start", "name": "createOrder"}),
            json!({
                "kind": "trigger", "trigger": "http-request", "subject": "createOrder",
                "value": {
                    "representation": "replayable",
                    "value": {"body": {"item": "widget", "qty": 2}},
                    "redaction": "redacted-at-source",
                },
            }),
            json!({
                "kind": "effect", "effect": "read", "subject": "inventory",
                "value": {
                    "representation": "replayable",
                    "value": raw_effect,
                    "redaction": "redacted-at-source",
                },
            }),
        ];
        if with_return_carrier {
            events.push(json!({
                "kind": "effect", "effect": "operation-return",
                "subject": OPERATION_RETURN_SUBJECT,
                "value": {
                    "representation": "replayable",
                    "value": raw_return,
                    "redaction": "redacted-at-source",
                },
            }));
        }
        events.push(json!({
            "kind": "operation-end", "name": "createOrder", "outcome": "failed",
        }));
        events.push(json!({
            "kind": "observation",
            "failure": {
                "observation": "exception",
                "authority": "runtime-diagnosis",
                "summary": "backend operation createOrder returned HTTP 500",
                "signature": "backend-server-error:createOrder",
                "observationPoint": "createOrder",
                "artifactIds": [],
            },
        }));
        let events = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| {
                let sequence = index as u64 + 1;
                json!({
                    "id": format!("evt_backend-rust_{sequence}"),
                    "sequence": sequence,
                    "monotonicNs": sequence,
                    "traceId": "cap-1-1",
                    "causalParentIds": if sequence == 1 {
                        json!([])
                    } else {
                        json!([format!("evt_backend-rust_{}", sequence - 1)])
                    },
                    "event": event,
                })
            })
            .collect::<Vec<_>>();
        let batch: CaptureBatch = serde_json::from_value(json!({
            "version": 1,
            "batchId": "cb-rust-1-1",
            "projectId": "app-demo",
            "sessionId": "cap-1-1",
            "emitter": {
                "id": "backend-rust",
                "kind": "runtime-sdk",
                "component": "backend",
                "runtime": "rust",
            },
            "observedAt": "1753747200000",
            "policy": {"consent": "application-telemetry", "retentionClass": "standard"},
            "capabilities": [],
            "events": events,
            "artifacts": [],
        }))
        .expect("fixture matches capture-batch-v1");
        batch
            .validate()
            .expect("fixture passes protocol validation");
        batch
    }

    #[test]
    fn sdk_batch_projects_to_the_backend_capture_payload() {
        let payload = backend_capture_from_batch(&sdk_batch(true)).expect("projects");
        assert_eq!(payload["format"], "reproit-backend-capture");
        assert_eq!(payload["version"], 1);
        assert_eq!(payload["operation"], "createOrder");
        assert_eq!(payload["oracle"], "backend-server-error");
        let events = payload["events"].as_array().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["kind"], "start");
        assert_eq!(events[0]["traceId"], "cap-1-1");
        assert_eq!(events[0]["spanId"], "cap-1-1:createOrder");
        assert_eq!(events[0]["input"]["body"]["item"], "widget");
        // Raw effect and return events pass through verbatim.
        assert_eq!(events[1]["kind"], "effect");
        assert_eq!(events[1]["resource"], "inventory");
        assert_eq!(events[2]["kind"], "return");
        assert_eq!(events[2]["status"], 500);
        assert_eq!(events[2]["output"]["error"], "boom");
    }

    #[test]
    fn exchange_bearing_batches_project_at_version_2_from_any_carrier_kind() {
        // The Node SDK family emits dependency/state capture events for
        // effects; an exchange-bearing raw event nested under either kind
        // must round-trip and bump the payload version.
        let raw_exchange_effect = json!({
            "traceId": "cap-1-1", "spanId": "cap-1-1:createOrder", "actionIndex": 0,
            "operation": "createOrder", "sequence": 2,
            "kind": "effect", "effect": "call", "resource": "pricing", "key": "GET /prices",
            "exchange": {
                "protocol": "http",
                "request": {"method": "GET", "url": "http://pricing/prices"},
                "response": {"status": 200, "body": {"prices": null}},
            },
        });
        let mut batch = sdk_batch(true);
        let template = batch
            .events
            .iter()
            .find(|event| matches!(event.event, CaptureEventKind::Effect { .. }))
            .expect("fixture has an effect event")
            .clone();
        for event in &mut batch.events {
            if let CaptureEventKind::Effect { subject, value, .. } = &mut event.event {
                if subject == "inventory" {
                    *value = Some(
                        serde_json::from_value(json!({
                            "representation": "replayable",
                            "value": raw_exchange_effect,
                            "redaction": "redacted-at-source",
                        }))
                        .unwrap(),
                    );
                }
            }
        }
        let payload = backend_capture_from_batch(&batch).expect("projects");
        assert_eq!(payload["version"], 2);
        let events = payload["events"].as_array().unwrap();
        assert_eq!(events[1]["exchange"]["response"]["status"], 200);

        // Same raw event under a dependency carrier: identical projection.
        let mut dependency_batch = sdk_batch(true);
        for event in &mut dependency_batch.events {
            if let CaptureEventKind::Effect { subject, .. } = &event.event {
                if subject == "inventory" {
                    let mut replaced = template.clone();
                    replaced.event = serde_json::from_value(json!({
                        "kind": "dependency",
                        "system": "service",
                        "operation": "call",
                        "subject": "pricing",
                        "value": {
                            "representation": "replayable",
                            "value": raw_exchange_effect,
                            "redaction": "redacted-at-source",
                        },
                    }))
                    .unwrap();
                    replaced.id = event.id.clone();
                    replaced.sequence = event.sequence;
                    replaced.monotonic_ns = event.monotonic_ns;
                    replaced.causal_parent_ids = event.causal_parent_ids.clone();
                    *event = replaced;
                }
            }
        }
        let payload = backend_capture_from_batch(&dependency_batch).expect("projects");
        assert_eq!(payload["version"], 2);
        assert_eq!(
            payload["events"].as_array().unwrap()[1]["exchange"]["protocol"],
            "http"
        );
    }

    #[test]
    fn determinism_envelope_checkpoint_projects_into_the_payload() {
        let mut batch = sdk_batch(true);
        let template = batch.events[0].clone();
        let mut checkpoint = template.clone();
        checkpoint.event = serde_json::from_value(json!({
            "kind": "checkpoint",
            "name": DETERMINISM_ENVELOPE_CHECKPOINT,
            "attributes": {
                "observedAtMs": 1753747200000u64,
                "tz": "Europe/Berlin",
                "node": "v26.5.0",
                "replaySeed": "00ff00ff00ff00ff",
            },
        }))
        .unwrap();
        checkpoint.id = "evt_backend-rust_99".into();
        checkpoint.sequence = batch.events.len() as u64 + 1;
        checkpoint.monotonic_ns = checkpoint.sequence;
        checkpoint.causal_parent_ids = Vec::new();
        batch.events.push(checkpoint);
        let payload = backend_capture_from_batch(&batch).expect("projects");
        assert_eq!(payload["envelope"]["tz"], "Europe/Berlin");
        assert_eq!(payload["envelope"]["replaySeed"], "00ff00ff00ff00ff");
        // Envelope alone does not force version 2; only event-level stamps
        // or exchanges do (older CLIs ignore the unknown payload field).
        assert_eq!(payload["version"], 1);
    }

    #[test]
    fn older_server_error_batches_synthesize_the_return_event() {
        let payload = backend_capture_from_batch(&sdk_batch(false)).expect("projects");
        let events = payload["events"].as_array().unwrap();
        assert_eq!(events.len(), 3);
        let returned = &events[2];
        assert_eq!(returned["kind"], "return");
        assert_eq!(returned["status"], 500);
        assert_eq!(returned["success"], false);
        assert_eq!(returned["effectsComplete"], true);
        assert_eq!(returned["traceId"], "cap-1-1");
        assert_eq!(returned["spanId"], "cap-1-1:createOrder");
    }

    #[test]
    fn batches_without_a_failure_or_trigger_do_not_project() {
        let mut healthy = sdk_batch(true);
        healthy
            .events
            .retain(|event| !matches!(event.event, CaptureEventKind::Observation { .. }));
        assert!(backend_capture_from_batch(&healthy).is_none());

        let mut untriggered = sdk_batch(true);
        untriggered
            .events
            .retain(|event| !matches!(event.event, CaptureEventKind::Trigger { .. }));
        assert!(backend_capture_from_batch(&untriggered).is_none());
    }

    #[test]
    fn older_batches_with_a_non_server_error_oracle_do_not_project() {
        let mut batch = sdk_batch(false);
        for event in &mut batch.events {
            if let CaptureEventKind::Observation { failure } = &mut event.event {
                failure.signature = Some("backend-atomicity:createOrder".into());
            }
        }
        assert!(backend_capture_from_batch(&batch).is_none());
    }
}
