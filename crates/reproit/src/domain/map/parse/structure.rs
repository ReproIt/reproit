//! Structural state, transition, and operability marker reduction.

use super::RunObs;
use crate::domain::appmap::{StateElement, StateText};
use serde_json::Value;

pub(super) fn absorb(obs: &mut RunObs, line: &str) -> bool {
    if line.starts_with("EXPLORE:STATE ") {
        if let Some(json) = super::extract(line, "EXPLORE:STATE ") {
            absorb_state(obs, &json);
        }
        return true;
    }
    if line.starts_with("EXPLORE:EDGE ") {
        if let Some(json) = super::extract(line, "EXPLORE:EDGE ") {
            absorb_edge(obs, &json);
        }
        return true;
    }
    if line.starts_with("EXPLORE:GROUNDTRUTH ") {
        if let Some(json) = super::extract(line, "EXPLORE:GROUNDTRUTH ") {
            if let Some(sig) = json.get("sig").and_then(Value::as_str) {
                obs.gaps
                    .insert(sig.to_string(), super::gaps_from_groundtruth(&json));
            }
        }
        return true;
    }
    if line.starts_with("EXPLORE:RERENDER ") {
        if let Some(json) = super::extract(line, "EXPLORE:RERENDER ") {
            absorb_rerender(obs, &json);
        }
        return true;
    }
    if line.starts_with("EXPLORE:FLICKER ") {
        if let Some(json) = super::extract(line, "EXPLORE:FLICKER ") {
            absorb_flicker(obs, &json);
        }
        return true;
    }
    false
}

fn absorb_state(obs: &mut RunObs, json: &Value) {
    let (Some(sig), Some(labels)) = (
        json.get("sig").and_then(Value::as_str),
        json.get("labels").and_then(Value::as_array),
    ) else {
        return;
    };
    if obs.start.is_none() {
        obs.start = Some(sig.to_string());
    }
    if let Some(route) = json
        .get("route")
        .and_then(Value::as_str)
        .filter(|route| !route.is_empty())
    {
        obs.routes
            .entry(sig.to_string())
            .or_insert_with(|| route.to_string());
    }
    absorb_elements(obs, sig, json);
    absorb_texts(obs, sig, json);
    obs.states.entry(sig.to_string()).or_insert_with(|| {
        labels
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect()
    });
}

fn absorb_elements(obs: &mut RunObs, sig: &str, json: &Value) {
    let Some(raw_elements) = json.get("elements").and_then(Value::as_array) else {
        return;
    };
    obs.tappables
        .entry(sig.to_string())
        .or_insert(raw_elements.len());
    let elements = raw_elements
        .iter()
        .filter_map(|element| {
            let sel = element.get("sel").and_then(Value::as_str)?.to_string();
            Some(StateElement {
                input_purpose: crate::domain::appmap::normalize_input_purpose(
                    element.get("inputPurpose").and_then(Value::as_str),
                    &sel,
                ),
                sel,
                role: element
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                label: element
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                bounds: super::parse_bounds(element.get("bounds")),
            })
        })
        .collect::<Vec<_>>();
    if !elements.is_empty() {
        obs.elements.entry(sig.to_string()).or_insert(elements);
    }
}

fn absorb_texts(obs: &mut RunObs, sig: &str, json: &Value) {
    let Some(raw_texts) = json.get("texts").and_then(Value::as_array) else {
        return;
    };
    let texts = raw_texts
        .iter()
        .filter_map(|raw| {
            let text = raw.get("text").and_then(Value::as_str)?.trim().to_string();
            if text.is_empty() {
                return None;
            }
            Some(StateText {
                text,
                bounds: super::parse_bounds(raw.get("bounds")),
            })
        })
        .collect::<Vec<_>>();
    if !texts.is_empty() {
        obs.texts.entry(sig.to_string()).or_insert(texts);
    }
}

fn absorb_edge(obs: &mut RunObs, json: &Value) {
    if let (Some(from), Some(action), Some(to)) = (
        json.get("from").and_then(Value::as_str),
        json.get("action").and_then(Value::as_str),
        json.get("to").and_then(Value::as_str),
    ) {
        obs.edges
            .push((from.to_string(), action.to_string(), to.to_string()));
    }
}

fn absorb_rerender(obs: &mut RunObs, json: &Value) {
    let (Some(from), Some(action), Some(churned)) = (
        json.get("from").and_then(Value::as_str),
        json.get("action").and_then(Value::as_str),
        json.get("churned").and_then(Value::as_array),
    ) else {
        return;
    };
    let keys = churned
        .iter()
        .filter_map(Value::as_str)
        .map(String::from)
        .collect::<Vec<_>>();
    if !keys.is_empty() {
        obs.rerenders
            .insert((from.to_string(), action.to_string()), keys);
    }
}

fn absorb_flicker(obs: &mut RunObs, json: &Value) {
    if let (Some(from), Some(action), Some(peak)) = (
        json.get("from").and_then(Value::as_str),
        json.get("action").and_then(Value::as_str),
        json.get("peak").and_then(Value::as_f64),
    ) {
        obs.paint_flickers
            .insert((from.to_string(), action.to_string()), peak);
    }
}
