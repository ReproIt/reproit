//! Deterministic, provider-independent UI oracle helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;

use super::emit;

pub(super) fn parse_invariant_marker(line: &str) -> Option<(String, Vec<(String, String)>)> {
    const MARK: &str = "REPROIT_INVARIANT ";
    let idx = line.find(MARK)?;
    let json: serde_json::Value = serde_json::from_str(line[idx + MARK.len()..].trim()).ok()?;
    let items: Vec<(String, String)> = json
        .get("items")?
        .as_array()?
        .iter()
        .filter_map(|it| {
            let id = it.get("id").and_then(|v| v.as_str())?.to_string();
            let message = it
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((id, message))
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let sig = json
        .get("sig")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((sig, items))
}

#[derive(Default)]
pub(super) struct InvariantState {
    pub(super) by_sig: BTreeMap<String, Vec<(String, String)>>,
    pub(super) fallback: Option<Vec<(String, String)>>,
}

pub(super) struct InvariantScrape {
    pub(super) state: Arc<Mutex<InvariantState>>,
    pub(super) emitted: BTreeSet<String>,
}

impl InvariantScrape {
    pub(super) fn spawn(reader: impl std::io::Read + Send + 'static) -> Self {
        let state = Arc::new(Mutex::new(InvariantState::default()));
        let sink = state.clone();
        std::thread::spawn(move || {
            let mut buf = std::io::BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::BufRead::read_line(&mut buf, &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if let Some((sig, items)) = parse_invariant_marker(&line) {
                    let mut state = sink.lock().unwrap();
                    if sig.is_empty() {
                        state.fallback = Some(items);
                    } else {
                        state.by_sig.insert(sig, items);
                    }
                }
            }
        });
        Self {
            state,
            emitted: BTreeSet::new(),
        }
    }

    pub(super) fn pending_for(&mut self, sig: &str) -> Option<Vec<(String, String)>> {
        let items = {
            let mut state = self.state.lock().unwrap();
            state
                .by_sig
                .get(sig)
                .cloned()
                .or_else(|| state.fallback.take())
        }?;
        if items.is_empty() || !self.emitted.insert(sig.to_string()) {
            return None;
        }
        Some(items)
    }

    pub(super) fn flush_for(&mut self, sig: &str) {
        let Some(items) = self.pending_for(sig) else {
            return;
        };
        let items: Vec<serde_json::Value> = items
            .iter()
            .map(|(id, message)| serde_json::json!({ "id": id, "message": message }))
            .collect();
        emit(&format!(
            "EXPLORE:INVARIANT {}",
            serde_json::json!({ "sig": sig, "items": items })
        ));
    }
}

fn content_bug_regexes() -> &'static [(Regex, &'static str)] {
    static REGEXES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    REGEXES.get_or_init(|| {
        vec![
            (Regex::new(r"\{\{[^}]*\}\}").unwrap(), "unrendered-template"),
            (Regex::new(r"\$\{[^}]*\}").unwrap(), "unrendered-template"),
            (
                Regex::new(r"(^|[\s:>(\[,])undefined($|[\s.,!?)\]<])").unwrap(),
                "undefined",
            ),
            (
                Regex::new(r"(^|[\s:>(\[,])null($|[\s.,!?)\]<])").unwrap(),
                "null",
            ),
            (
                Regex::new(r"(^|[\s:>(\[,])NaN($|[\s.,!?)\]<])").unwrap(),
                "nan",
            ),
        ]
    })
}

fn label_looks_like_prose(text: &str, token: &str) -> bool {
    let stripped = text.replace(token, " ");
    let stripped = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    let has_sentence = stripped.chars().any(|c| c == '.' || c == '!' || c == '?');
    stripped.chars().count() > 24 || has_sentence
}

pub(super) fn content_bug_reason(text: &str) -> Option<&'static str> {
    if text.is_empty() {
        return None;
    }
    if text.contains("[object Object]") && !label_looks_like_prose(text, "[object Object]") {
        return Some("object-object");
    }
    for (regex, reason) in content_bug_regexes() {
        if !regex.is_match(text) {
            continue;
        }
        if *reason == "unrendered-template" {
            return Some(reason);
        }
        let token = match *reason {
            "undefined" => "undefined",
            "null" => "null",
            _ => "NaN",
        };
        if !label_looks_like_prose(text, token) {
            return Some(reason);
        }
    }
    None
}

pub(super) fn tofu_detail(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let hit = chars.iter().position(|&c| c == '\u{FFFD}')?;
    let start = hit.saturating_sub(20);
    let end = (hit + 21).min(chars.len());
    Some(
        chars[start..end]
            .iter()
            .collect::<String>()
            .trim()
            .to_string(),
    )
}
