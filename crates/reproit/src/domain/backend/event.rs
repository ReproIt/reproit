use super::{default_true, EffectKind, ProtocolEvidence, EVENT_MARKER};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub build: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_contract: Option<String>,
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
    /// Determinism envelope (capture mode only): wall-clock epoch millis at
    /// the event, and monotonic nanoseconds since the operation started.
    /// Scan-time traces never carry these; hermetic replay pins its clock to
    /// them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    #[serde(default, rename = "monoNs", skip_serializing_if = "Option::is_none")]
    pub mono_ns: Option<u64>,
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
        /// Captured dependency exchange (capture version 2): the request the
        /// app sent and the response the dependency returned, recorded by the
        /// SDK's outbound instrumentation. This is what hermetic local replay
        /// stubs; evaluation oracles ignore it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exchange: Option<Value>,
    },
    /// Complete transport evidence emitted by an adapter. The proof payload is
    /// self-contained so replay retains the exact oracle subtype and inputs.
    Protocol { proof: ProtocolEvidence },
}

pub fn parse_events(log: &str) -> Vec<BackendEvent> {
    let runner_events = crate::domain::runner::parse(log);
    parse_runner_events(&runner_events)
}

pub(crate) fn parse_runner_events(
    runner_events: &[crate::domain::runner::RunnerEvent<'_>],
) -> Vec<BackendEvent> {
    let mut events = Vec::new();
    for event in runner_events {
        let value = match *event {
            crate::domain::runner::RunnerEvent::Backend(line) => line.strip_prefix(EVENT_MARKER),
            crate::domain::runner::RunnerEvent::Fuzz(line) => line.strip_prefix("FUZZ:BACKEND "),
            crate::domain::runner::RunnerEvent::Explore(_) => None,
        };
        let Some(value) = value else { continue };
        let Ok(event) = serde_json::from_str::<BackendEvent>(value) else {
            // A recognized but malformed evidence envelope makes the stream
            // incomplete. Abstain instead of turning a dropped event into an
            // absence-based backend finding.
            return Vec::new();
        };
        events.push(event);
    }
    events.sort_by_key(|event| event.sequence);
    events
}

pub(crate) fn from_protocol_frames(frames: &[reproit_protocol::EventFrame]) -> Vec<BackendEvent> {
    let mut events = frames
        .iter()
        .filter_map(|frame| match &frame.event {
            reproit_protocol::Event::Backend { evidence } => {
                serde_json::from_value::<BackendEvent>(evidence.clone()).ok()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.sequence);
    events
}
