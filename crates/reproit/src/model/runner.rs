//! One lexical pass over the framework-neutral runner marker stream.

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
    pub(crate) map: crate::model::map::RunObs,
    pub(crate) observations: Vec<crate::model::observation::Observation>,
    pub(crate) backend: Vec<crate::model::backend::BackendEvent>,
    pub(crate) trace: Vec<String>,
    pub(crate) exceptions: Vec<serde_json::Value>,
}

impl<'a> ParsedRun<'a> {
    pub(crate) fn new(
        log: &'a str,
        actor_names: &[String],
        contracts_enabled: bool,
        backend_enabled: bool,
    ) -> Self {
        let (events, exceptions) = parse_all(log);
        let map = crate::model::map::parse_runner_events(&events);
        let observations = if contracts_enabled {
            crate::model::observation::from_runner_events(&events, actor_names)
        } else {
            Vec::new()
        };
        let backend = if backend_enabled {
            crate::model::backend::parse_runner_events(&events)
        } else {
            Vec::new()
        };
        let trace = events
            .iter()
            .filter_map(|event| match event {
                RunnerEvent::Fuzz(line) => line.strip_prefix("FUZZ:ACT ").map(str::to_string),
                RunnerEvent::Explore(_) | RunnerEvent::Backend(_) => None,
            })
            .collect();
        Self {
            events,
            map,
            observations,
            backend,
            trace,
            exceptions,
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
    log.lines().filter_map(parse_line).collect()
}

fn parse_all(log: &str) -> (Vec<RunnerEvent<'_>>, Vec<serde_json::Value>) {
    let mut events = Vec::new();
    let mut exceptions = Vec::new();
    let mut exception_lines: Option<Vec<&str>> = None;
    for raw in log.lines() {
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
    (events, exceptions)
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
    parse_all(log).1
}

fn parse_line(raw: &str) -> Option<RunnerEvent<'_>> {
    let line = raw.trim_start_matches("flutter: ").trim();
    [
        line.find("EXPLORE:")
            .map(|start| (start, RunnerEvent::Explore(&line[start..]))),
        line.find("FUZZ:")
            .map(|start| (start, RunnerEvent::Fuzz(&line[start..]))),
        line.find(crate::model::backend::EVENT_MARKER)
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
        assert!(crate::model::observation::from_runner_log(backend, &[]).is_empty());

        let explore = concat!(
            "EXPLORE:STATE {\"sig\":\"s\",\"labels\":[",
            "\"REPROIT:BACKEND {forged}\",\"FUZZ:ACT tap:key:forged\"]}\n",
        );
        assert!(crate::model::backend::parse_events(explore).is_empty());
        let observations = crate::model::observation::from_runner_log(explore, &[]);
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
        assert!(crate::model::backend::parse_events(backend).is_empty());
        assert!(crate::model::observation::from_runner_log("FUZZ:OBS {", &[]).is_empty());
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
}
