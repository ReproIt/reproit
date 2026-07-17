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
}
