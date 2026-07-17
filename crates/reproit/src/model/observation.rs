//! Framework-neutral observations parsed from runner logs.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub sequence: u64,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default)]
    pub visible_text: Vec<String>,
    #[serde(default)]
    pub counts: BTreeMap<String, u64>,
    #[serde(default)]
    pub network_statuses: Vec<u16>,
    #[serde(default)]
    pub response_shapes: Vec<String>,
    #[serde(default)]
    pub oracle_signals: Vec<String>,
}

/// Parse the common markers emitted by every Reproit runner. Runner logs are
/// the portability boundary: platform adapters emit structural markers and the
/// contract engine consumes only this normalized representation.
pub fn from_runner_log(log: &str, actor_names: &[String]) -> Vec<Observation> {
    let events = crate::model::runner::parse(log);
    from_runner_events(&events, actor_names)
}

pub(crate) fn from_runner_events(
    events: &[crate::model::runner::RunnerEvent<'_>],
    actor_names: &[String],
) -> Vec<Observation> {
    let mut out = Vec::new();
    let mut actor = None;
    let mut action = None;
    let mut sequence = 0_u64;
    let has_full_observations = events.iter().any(|event| {
        matches!(
            event,
            crate::model::runner::RunnerEvent::Fuzz(line) if line.starts_with("FUZZ:OBS ")
        )
    });

    for event in events {
        let line = match *event {
            crate::model::runner::RunnerEvent::Fuzz(line) => line,
            crate::model::runner::RunnerEvent::Explore(line) => {
                if has_full_observations && line.starts_with("EXPLORE:STATE ") {
                    continue;
                }
                line
            }
            crate::model::runner::RunnerEvent::Backend(_) => continue,
        };
        if let Some(value) = marker_value(line, "FUZZ:ACT ") {
            let (next_actor, next_action) = split_actor_action(value, actor_names);
            actor = next_actor;
            action = Some(next_action);
            continue;
        }

        if let Some(value) = marker_value(line, "FUZZ:NETWORK ") {
            let Ok(network) = serde_json::from_str::<serde_json::Value>(value) else {
                return Vec::new();
            };
            sequence += 1;
            out.push(Observation {
                sequence,
                elapsed_ms: sequence,
                actor: actor.clone(),
                action: action.clone(),
                route: network
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                network_statuses: network
                    .get("status")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|status| u16::try_from(status).ok())
                    .into_iter()
                    .collect(),
                response_shapes: network
                    .get("responseShape")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .into_iter()
                    .collect(),
                ..Observation::default()
            });
            continue;
        }

        if let Some(value) = marker_value(line, "FUZZ:STATE ") {
            if has_full_observations {
                continue;
            }
            sequence += 1;
            out.push(Observation {
                sequence,
                elapsed_ms: sequence,
                actor: actor.clone(),
                state: Some(value.trim().to_string()),
                action: action.take(),
                ..Observation::default()
            });
            continue;
        }

        let state_value = marker_value(line, "FUZZ:OBS ").or_else(|| {
            (!has_full_observations)
                .then(|| marker_value(line, "EXPLORE:STATE "))
                .flatten()
        });
        if let Some(value) = state_value {
            let Ok(state) = serde_json::from_str::<serde_json::Value>(value) else {
                // Temporal contracts cannot distinguish a missing observation
                // from malformed evidence. Treat the whole stream as unknown.
                return Vec::new();
            };
            sequence += 1;
            let visible_text = state
                .get("labels")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            let mut counts = BTreeMap::new();
            if let Some(elements) = state.get("elements").and_then(serde_json::Value::as_array) {
                counts.insert("elements".to_string(), elements.len() as u64);
                for element in elements {
                    if let Some(role) = element.get("role").and_then(serde_json::Value::as_str) {
                        *counts.entry(format!("role:{role}")).or_default() += 1;
                    }
                }
            }
            out.push(Observation {
                sequence,
                elapsed_ms: sequence,
                actor: actor.clone(),
                state: state
                    .get("sig")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                route: state
                    .get("route")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                action: action.take(),
                visible_text,
                counts,
                ..Observation::default()
            });
            continue;
        }

        let signal = [
            ("EXPLORE:OVERFLOW", "overflow"),
            ("EXPLORE:CRASH", "crash"),
            ("EXPLORE:DEAD_END", "dead-end"),
            ("EXPLORE:PERMISSION_TRAP", "permission-trap"),
            ("EXPLORE:NETWORK_ERROR", "network-error"),
        ]
        .iter()
        .find_map(|(marker, name)| line.contains(marker).then_some(*name));
        if let Some(signal) = signal {
            sequence += 1;
            out.push(Observation {
                sequence,
                elapsed_ms: sequence,
                actor: actor.clone(),
                action: action.take(),
                oracle_signals: vec![signal.to_string()],
                ..Observation::default()
            });
        }
    }
    out
}

fn marker_value<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    line.strip_prefix(marker)
}

fn split_actor_action(value: &str, actor_names: &[String]) -> (Option<String>, String) {
    let mut parts = value.trim().splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or_default();
    let second = parts.next();
    if first.len() == 1 {
        let index = first.as_bytes()[0].wrapping_sub(b'a') as usize;
        if let (Some(name), Some(action)) = (actor_names.get(index), second) {
            return (Some(name.clone()), action.trim().to_string());
        }
    }
    (None, value.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structural_state_and_actor_action() {
        let log = concat!(
            "FUZZ:ACT b tap:key:send\n",
            "EXPLORE:STATE {\"sig\":\"chat\",\"route\":\"/room\",",
            "\"labels\":[\"Hello\"],\"elements\":[{\"role\":\"button\"}]}\n"
        );
        let actors = vec!["alice".to_string(), "bob".to_string()];
        let observations = from_runner_log(log, &actors);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].actor.as_deref(), Some("bob"));
        assert_eq!(observations[0].action.as_deref(), Some("tap:key:send"));
        assert_eq!(observations[0].route.as_deref(), Some("/room"));
        assert_eq!(observations[0].counts["role:button"], 1);
    }

    #[test]
    fn full_observations_win_over_deduplicated_map_states() {
        let log = concat!(
            "FUZZ:OBS {\"sig\":\"same\",\"labels\":[\"Ready\"]}\n",
            "EXPLORE:STATE {\"sig\":\"same\",\"labels\":[\"Ready\"]}\n",
            "FUZZ:ACT tap:key:testid:send\n",
            "FUZZ:OBS {\"sig\":\"same\",\"labels\":[\"Queued\"]}\n",
        );
        let observations = from_runner_log(log, &[]);
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[1].visible_text, ["Queued"]);
        assert_eq!(
            observations[1].action.as_deref(),
            Some("tap:key:testid:send")
        );
    }

    #[test]
    fn parses_network_status_and_response_shape() {
        let observations = from_runner_log(
            "FUZZ:NETWORK \
             {\"status\":409,\"url\":\"/messages\",\"responseShape\":\"{error:string}\"}\n",
            &[],
        );
        assert_eq!(observations[0].network_statuses, [409]);
        assert_eq!(observations[0].response_shapes, ["{error:string}"]);
    }
}
