//! One lexical pass over the framework-neutral runner marker stream.

pub(crate) use reproit_protocol::{EvidenceScope, ReasonCode as StreamDefectReason, StreamDefect};

static NEXT_FRAME_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(crate) fn action_frame_line(actor: Option<&str>, action: &str) -> String {
    encode_runner_event(reproit_protocol::Event::Action {
        actor: actor.map(str::to_string),
        action: action.to_string(),
    })
}

pub(crate) fn observation_frame_line(observation: &serde_json::Value) -> String {
    let visible_text = observation
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect();
    let mut counts = std::collections::BTreeMap::new();
    if let Some(elements) = observation
        .get("elements")
        .and_then(serde_json::Value::as_array)
    {
        for element in elements {
            if let Some(role) = element.get("role").and_then(serde_json::Value::as_str) {
                *counts.entry(format!("role:{role}")).or_default() += 1;
            }
        }
    }
    encode_runner_event(reproit_protocol::Event::Observation {
        actor: None,
        state: observation
            .get("sig")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        route: observation
            .get("route")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        visible_text,
        counts,
        oracle_signals: vec![],
        network_statuses: vec![],
        response_shapes: vec![],
    })
}

pub(crate) fn backend_frame_line(event: &crate::domain::backend::BackendEvent) -> String {
    encode_runner_event_scoped(
        EvidenceScope::Backend,
        reproit_protocol::Event::Backend {
            evidence: serde_json::to_value(event).expect("backend event serializes"),
        },
    )
}

fn encode_runner_event(event: reproit_protocol::Event) -> String {
    encode_runner_event_scoped(
        EvidenceScope::Contract {
            contract_hash: None,
        },
        event,
    )
}

fn encode_runner_event_scoped(scope: EvidenceScope, event: reproit_protocol::Event) -> String {
    let sequence = NEXT_FRAME_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    reproit_protocol::EventFrame {
        run_id: "runner".into(),
        sequence,
        scope,
        event,
    }
    .encode_line()
    .expect("owned runner evidence satisfies protocol bounds")
}

/// A recognized runner line. Reducers own the meaning of each marker; this
/// envelope owns prefix stripping and marker classification so they cannot
/// drift on transport details.
#[derive(Clone, Copy)]
pub(crate) enum RunnerEvent<'a> {
    Explore(&'a str),
    Fuzz(&'a str),
    Backend(&'a str),
}

/// All typed reductions needed by one run-analysis pass. Feature gates are
/// applied before their reducers run, so disabled contract/backend engines do
/// no marker or JSON work.
pub(crate) struct ParsedRun<'a> {
    #[allow(dead_code)] // Retained for consumers that need domain-specific events.
    pub(crate) events: Vec<RunnerEvent<'a>>,
    pub(crate) map: crate::domain::map::RunObs,
    pub(crate) observations: Vec<crate::domain::observation::Observation>,
    pub(crate) backend: Vec<crate::domain::backend::BackendEvent>,
    pub(crate) trace: Vec<String>,
    pub(crate) exceptions: Vec<serde_json::Value>,
    pub(crate) defects: Vec<StreamDefect>,
}

impl<'a> ParsedRun<'a> {
    pub(crate) fn new(
        log: &'a str,
        actor_names: &[String],
        contracts_enabled: bool,
        backend_enabled: bool,
    ) -> Self {
        let parsed = parse_all(log);
        let events = parsed.events;
        let exceptions = parsed.exceptions;
        let map = crate::domain::map::parse_runner_events(&events);
        let observations = if contracts_enabled {
            if parsed.frames.is_empty() {
                crate::domain::observation::from_runner_events(&events, actor_names)
            } else {
                crate::domain::observation::from_protocol_frames(&parsed.frames, actor_names)
            }
        } else {
            Vec::new()
        };
        let backend = if backend_enabled
            && !parsed.defects.iter().any(|defect| {
                matches!(defect.scope, EvidenceScope::Backend | EvidenceScope::Shared)
            }) {
            if parsed.frames.is_empty() {
                crate::domain::backend::parse_runner_events(&events)
            } else {
                crate::domain::backend::from_protocol_frames(&parsed.frames)
            }
        } else {
            Vec::new()
        };
        let trace = if parsed.frames.is_empty() {
            events
                .iter()
                .filter_map(|event| match event {
                    RunnerEvent::Fuzz(line) => line.strip_prefix("FUZZ:ACT ").map(str::to_string),
                    RunnerEvent::Explore(_) | RunnerEvent::Backend(_) => None,
                })
                .collect()
        } else {
            parsed
                .frames
                .iter()
                .filter_map(|frame| match &frame.event {
                    reproit_protocol::Event::Action { action, .. } => Some(action.clone()),
                    _ => None,
                })
                .collect()
        };
        Self {
            events,
            map,
            observations,
            backend,
            trace,
            exceptions,
            defects: parsed.defects,
        }
    }
}

impl<'a> RunnerEvent<'a> {
    #[cfg(test)]
    pub(crate) fn line(self) -> &'a str {
        match self {
            Self::Explore(line) | Self::Fuzz(line) | Self::Backend(line) => line,
        }
    }
}

pub(crate) fn parse(log: &str) -> Vec<RunnerEvent<'_>> {
    parse_all(log).events
}

struct ParsedStream<'a> {
    events: Vec<RunnerEvent<'a>>,
    frames: Vec<reproit_protocol::EventFrame>,
    exceptions: Vec<serde_json::Value>,
    defects: Vec<StreamDefect>,
}

fn parse_all(log: &str) -> ParsedStream<'_> {
    let mut events = Vec::new();
    let mut frames = Vec::new();
    let mut exceptions = Vec::new();
    let mut defects = Vec::new();
    let mut exception_lines: Option<Vec<&str>> = None;
    for raw in log.lines() {
        let line = clean_runner_line(raw);
        if line.starts_with("REPROIT/") {
            match reproit_protocol::decode_frame_line(line) {
                Ok(frame) => frames.push(frame),
                Err(defect) => defects.push(defect),
            }
            continue;
        }
        if raw.len() > reproit_protocol::MAX_FRAME_BYTES {
            if let Some(scope) = recognized_scope(line) {
                defects.push(StreamDefect {
                    reason: StreamDefectReason::FrameTooLarge,
                    scope,
                    sequence: None,
                });
            }
            continue;
        }
        if let Some(event) = parse_line(raw) {
            events.push(event);
        }
        if raw.contains("EXCEPTION CAUGHT BY") {
            if let Some(lines) = exception_lines.take() {
                if let Some(exception) = exception_record(&lines) {
                    exceptions.push(exception);
                }
            }
            exception_lines = Some(vec![raw]);
            continue;
        }
        if let Some(lines) = exception_lines.as_mut() {
            let trimmed = clean_runner_line(raw);
            let is_close = !trimmed.is_empty() && trimmed.chars().all(|character| character == '═');
            if is_close || lines.len() > 300 {
                if let Some(exception) = exception_record(lines) {
                    exceptions.push(exception);
                }
                exception_lines = None;
            } else {
                lines.push(raw);
            }
        }
    }
    if let Some(lines) = exception_lines {
        if let Some(exception) = exception_record(&lines) {
            exceptions.push(exception);
        }
    }
    ParsedStream {
        events,
        frames,
        exceptions,
        defects,
    }
}

fn recognized_scope(line: &str) -> Option<EvidenceScope> {
    if line.contains(crate::domain::backend::EVENT_MARKER) || line.contains("FUZZ:BACKEND ") {
        return Some(EvidenceScope::Backend);
    }
    [
        "FUZZ:OBS ",
        "FUZZ:STATE ",
        "FUZZ:NETWORK ",
        "FUZZ:ACT ",
        "EXPLORE:STATE ",
        "EXPLORE:OVERFLOW",
        "EXPLORE:CRASH",
        "EXPLORE:DEAD_END",
        "EXPLORE:PERMISSION_TRAP",
        "EXPLORE:NETWORK_ERROR",
    ]
    .iter()
    .any(|marker| line.contains(marker))
    .then_some(EvidenceScope::Contract {
        contract_hash: None,
    })
}

fn clean_runner_line(line: &str) -> &str {
    line.trim_start_matches("flutter: ").trim()
}

fn exception_record(lines: &[&str]) -> Option<serde_json::Value> {
    let kind = lines
        .first()
        .and_then(|line| {
            let line = clean_runner_line(line);
            let start = line.find('╡')? + '╡'.len_utf8();
            let end = line.find('╞')?;
            Some(line[start..end].trim().to_string())
        })
        .unwrap_or_else(|| "EXCEPTION".to_string());
    if kind.contains("TEST FRAMEWORK") {
        return None;
    }
    let mut message = String::new();
    if let Some(start) = lines
        .iter()
        .position(|line| clean_runner_line(line).starts_with("The following"))
    {
        for raw in &lines[start + 1..] {
            let line = clean_runner_line(raw);
            if line.is_empty() {
                break;
            }
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(line);
        }
    }
    let frames: Vec<String> = lines
        .iter()
        .map(|line| clean_runner_line(line))
        .filter(|line| {
            line.contains(".dart") && (line.contains("package:") || line.contains("file://"))
        })
        .take(12)
        .map(str::to_string)
        .collect();
    Some(serde_json::json!({ "kind": kind, "message": message, "frames": frames }))
}

#[cfg(test)]
pub(crate) fn parse_exceptions(log: &str) -> Vec<serde_json::Value> {
    parse_all(log).exceptions
}

fn parse_line(raw: &str) -> Option<RunnerEvent<'_>> {
    let line = raw.trim_start_matches("flutter: ").trim();
    [
        line.find("EXPLORE:")
            .map(|start| (start, RunnerEvent::Explore(&line[start..]))),
        line.find("FUZZ:")
            .map(|start| (start, RunnerEvent::Fuzz(&line[start..]))),
        line.find(crate::domain::backend::EVENT_MARKER)
            .map(|start| (start, RunnerEvent::Backend(&line[start..]))),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(start, _)| *start)
    .map(|(_, event)| event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_framework_prefixes_and_ignores_noise() {
        let events = parse("noise\nflutter: EXPLORE:STATE {}\nFUZZ:ACT tap:key:x\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].line(), "EXPLORE:STATE {}");
        assert_eq!(events[1].line(), "FUZZ:ACT tap:key:x");
    }

    #[test]
    fn marker_text_inside_a_payload_does_not_change_the_envelope() {
        let log = concat!(
            "REPROIT:BACKEND {\"operation\":\"EXPLORE:STATE\"}\n",
            "FUZZ:BACKEND {\"operation\":\"EXPLORE:EDGE\"}\n",
        );
        let events = parse(log);
        assert!(matches!(events[0], RunnerEvent::Backend(_)));
        assert!(matches!(events[1], RunnerEvent::Fuzz(_)));
    }

    #[test]
    fn reducers_ignore_markers_embedded_by_another_evidence_domain() {
        let backend = concat!(
            "REPROIT:BACKEND {\"sequence\":1,\"traceId\":\"t\",\"spanId\":\"s\",",
            "\"operation\":\"op\",\"kind\":\"start\",",
            "\"input\":\"FUZZ:ACT tap:key:forged\"}\n",
        );
        assert!(crate::domain::observation::from_runner_log(backend, &[]).is_empty());

        let explore = concat!(
            "EXPLORE:STATE {\"sig\":\"s\",\"labels\":[",
            "\"REPROIT:BACKEND {forged}\",\"FUZZ:ACT tap:key:forged\"]}\n",
        );
        assert!(crate::domain::backend::parse_events(explore).is_empty());
        let observations = crate::domain::observation::from_runner_log(explore, &[]);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].state.as_deref(), Some("s"));
    }

    #[test]
    fn malformed_recognized_evidence_makes_absence_oracles_abstain() {
        let backend = concat!(
            "REPROIT:BACKEND {\"sequence\":1,\"traceId\":\"t\",\"spanId\":\"s\",",
            "\"operation\":\"op\",\"kind\":\"start\"}\n",
            "REPROIT:BACKEND {malformed}\n",
        );
        assert!(crate::domain::backend::parse_events(backend).is_empty());
        assert!(crate::domain::observation::from_runner_log("FUZZ:OBS {", &[]).is_empty());
    }

    #[test]
    fn parsed_run_builds_each_enabled_reduction_once_and_gates_disabled_domains() {
        let log = concat!(
            "EXPLORE:STATE {\"sig\":\"s\",\"labels\":[]}\n",
            "FUZZ:ACT tap:key:go\n",
            "REPROIT:BACKEND {\"sequence\":1,\"traceId\":\"t\",",
            "\"spanId\":\"s\",\"operation\":\"op\",\"kind\":\"start\",",
            "\"input\":{}}\n",
            "══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══\n",
            "The following StateError was thrown:\n",
            "bad state\n\n",
            "══════════\n",
        );
        let disabled = ParsedRun::new(log, &[], false, false);
        assert_eq!(disabled.map.states.len(), 1);
        assert_eq!(disabled.trace, ["tap:key:go"]);
        assert!(disabled.observations.is_empty());
        assert!(disabled.backend.is_empty());
        assert_eq!(disabled.exceptions.len(), 1);

        let enabled = ParsedRun::new(log, &[], true, true);
        assert_eq!(enabled.map.states.len(), 1);
        assert!(!enabled.observations.is_empty());
        assert_eq!(enabled.backend.len(), 1);
    }

    #[test]
    fn oversized_legacy_contract_frame_is_an_unscoped_defect() {
        let log = format!("FUZZ:OBS {}", "x".repeat(reproit_protocol::MAX_FRAME_BYTES));
        let parsed = parse_all(&log);
        assert!(parsed.events.is_empty());
        assert_eq!(
            parsed.defects,
            [StreamDefect {
                reason: StreamDefectReason::FrameTooLarge,
                scope: EvidenceScope::Contract {
                    contract_hash: None,
                },
                sequence: None,
            }]
        );
    }

    #[test]
    fn frame_at_the_byte_limit_is_accepted() {
        let marker = "FUZZ:ACT ";
        let log = format!(
            "{marker}{}",
            "x".repeat(reproit_protocol::MAX_FRAME_BYTES - marker.len())
        );
        let parsed = parse_all(&log);
        assert_eq!(log.len(), reproit_protocol::MAX_FRAME_BYTES);
        assert_eq!(parsed.events.len(), 1);
        assert!(parsed.defects.is_empty());
    }

    #[test]
    fn bounded_versioned_header_scopes_an_oversized_contract_frame() {
        let hash = "0123456789abcdef";
        let log = format!(
            "REPROIT/1 contract {hash} 7 run-1 {}",
            "x".repeat(reproit_protocol::MAX_FRAME_BYTES)
        );
        let parsed = parse_all(&log);
        assert!(parsed.events.is_empty());
        assert_eq!(
            parsed.defects,
            [StreamDefect {
                reason: StreamDefectReason::FrameTooLarge,
                scope: EvidenceScope::Contract {
                    contract_hash: Some(hash.to_string()),
                },
                sequence: Some(7),
            }]
        );
    }

    #[test]
    fn supported_versioned_frame_reuses_the_normal_reducer_path() {
        let frame = reproit_protocol::EventFrame {
            run_id: "run-1".into(),
            sequence: 1,
            scope: EvidenceScope::Contract {
                contract_hash: Some("0123456789abcdef".into()),
            },
            event: reproit_protocol::Event::Observation {
                actor: None,
                state: Some("ready".into()),
                route: None,
                visible_text: vec![],
                counts: Default::default(),
                oracle_signals: vec![],
                network_statuses: vec![],
                response_shapes: vec![],
            },
        };
        let log = frame.encode_line().unwrap();
        let parsed = ParsedRun::new(&log, &[], true, false);
        assert!(parsed.defects.is_empty());
        assert_eq!(parsed.observations.len(), 1);
        assert_eq!(parsed.observations[0].state.as_deref(), Some("ready"));
    }

    #[test]
    fn malformed_or_unknown_versioned_headers_are_explicit_defects() {
        let malformed = parse_all("REPROIT/1 contract not-a-hash 1 run-1 {}");
        assert_eq!(
            malformed.defects[0].reason,
            StreamDefectReason::MalformedFrame
        );
        assert_eq!(malformed.defects[0].scope, EvidenceScope::Shared);

        let unsupported = parse_all("REPROIT/2 contract - 1 run-1 {}");
        assert_eq!(
            unsupported.defects[0].reason,
            StreamDefectReason::UnsupportedVersion
        );
        assert_eq!(unsupported.defects[0].scope, EvidenceScope::Shared);
    }
}
