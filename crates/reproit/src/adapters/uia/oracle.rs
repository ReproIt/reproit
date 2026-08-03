//! Deterministic, provider-independent UI oracle helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use regex::Regex;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, IUIAutomationScrollPattern, UIA_ScrollPatternId,
};

use super::{
    children_of, el_automation_id, el_bounds, el_control_type, el_role_live, emit, get_pattern,
    label_of, MAX_UIA_NODES,
};
use crate::domain::native_oracle::{
    focus_was_lost, FocusAfter, FocusArm, NativeNode, ScrollPoint, ScrollSample,
};

pub(super) struct OracleNode {
    pub(super) observation: NativeNode,
    pub(super) element: IUIAutomationElement,
}

pub(super) fn native_node(el: &IUIAutomationElement, role: &str) -> Option<NativeNode> {
    let id = el_automation_id(el)?;
    Some(NativeNode {
        key: format!("id:{id}"),
        identity: 0,
        role: role.to_string(),
    })
}

pub(super) struct UiaFocusArm {
    decision: FocusArm,
    element: IUIAutomationElement,
}

pub(super) fn arm_focus(node: Option<&OracleNode>, dialog_count: usize) -> Option<UiaFocusArm> {
    let node = node?;
    if !unsafe { node.element.CurrentHasKeyboardFocus() }
        .ok()?
        .as_bool()
    {
        return None;
    }
    Some(UiaFocusArm {
        decision: FocusArm {
            key: node.observation.key.clone(),
            identity: 1,
            role: node.observation.role.clone(),
            dialog_count,
        },
        element: node.element.clone(),
    })
}

pub(super) fn lost_focus(
    automation: &IUIAutomation,
    window: &IUIAutomationElement,
    arm: Option<&UiaFocusArm>,
    after: &BTreeMap<String, OracleNode>,
    dialog_count: usize,
    same_screen: bool,
) -> bool {
    let Some(arm) = arm else {
        return false;
    };
    let Some(target) = after.get(&arm.decision.key) else {
        return false;
    };
    let same_target = unsafe { automation.CompareElements(&arm.element, &target.element) }
        .map(|same| same.as_bool())
        .ok();
    if same_target != Some(true) {
        return false;
    }
    let Ok(focused) = (unsafe { automation.GetFocusedElement() }) else {
        return false;
    };
    let focused_is_target = unsafe { automation.CompareElements(&focused, &target.element) }
        .map(|same| same.as_bool())
        .ok();
    let focused_is_window = unsafe { automation.CompareElements(&focused, window) }
        .map(|same| same.as_bool())
        .ok();
    let (Some(focused_is_target), Some(focused_is_window)) = (focused_is_target, focused_is_window)
    else {
        return false;
    };
    let mut target_observation = target.observation.clone();
    target_observation.identity = 1;
    focus_was_lost(
        Some(&arm.decision),
        Some(&FocusAfter {
            target: target_observation,
            focused_identity: focused_is_target.then_some(1),
            focus_is_window: focused_is_window,
            dialog_count,
            same_screen,
        }),
    )
}

fn scroll_sample(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
    pattern: &IUIAutomationScrollPattern,
) -> Option<ScrollSample> {
    let bounds = el_bounds(element)?;
    let offset = unsafe { pattern.CurrentVerticalScrollPercent() }.ok()?;
    let width = bounds.2 - bounds.0;
    let height = bounds.3 - bounds.1;
    if width < 8 || height < 8 {
        return None;
    }
    let mut points = Vec::new();
    for fraction in [2, 5, 8] {
        let point = POINT {
            x: bounds.0 + width / 2,
            y: bounds.1 + (height * fraction) / 10,
        };
        let observed = unsafe { automation.ElementFromPoint(point) }
            .ok()
            .and_then(|hit| {
                let text = label_of(&hit);
                let hit_bounds = el_bounds(&hit)?;
                (!text.is_empty()).then(|| ScrollPoint {
                    position: format!("y={fraction}"),
                    text: normalize_scroll_text(&text),
                    shape: format!(
                        "{}|{}|{}",
                        el_role_live(&hit, el_control_type(&hit)),
                        hit_bounds.2 - hit_bounds.0,
                        hit_bounds.3 - hit_bounds.1
                    ),
                })
            });
        points.push(observed);
    }
    Some(ScrollSample {
        offset_milli: (offset * 10.0).round() as i64,
        points,
    })
}

fn normalize_scroll_text(text: &str) -> String {
    let mut out = String::new();
    let mut in_number = false;
    for character in text.chars().take(120) {
        if character.is_ascii_digit() || matches!(character, '.' | ',' | ':') {
            if !in_number {
                out.push('#');
                in_number = true;
            }
        } else {
            in_number = false;
            out.push(character);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn scroll_round_trip(
    automation: &IUIAutomation,
    window: &IUIAutomationElement,
) -> Vec<serde_json::Value> {
    let mut candidates = Vec::new();
    let mut stack = vec![window.clone()];
    for _ in 0..MAX_UIA_NODES {
        let Some(element) = stack.pop() else {
            break;
        };
        if let Some(pattern) =
            get_pattern::<IUIAutomationScrollPattern>(&element, UIA_ScrollPatternId.0)
        {
            let scrollable = unsafe { pattern.CurrentVerticallyScrollable() }
                .map(|value| value.as_bool())
                .unwrap_or(false);
            if scrollable {
                if let Some(bounds) = el_bounds(&element) {
                    candidates.push((
                        (bounds.2 - bounds.0) * (bounds.3 - bounds.1),
                        element.clone(),
                        pattern,
                    ));
                }
            }
        }
        stack.extend(children_of(automation, &element));
    }
    let Some((_, element, pattern)) = candidates.into_iter().max_by_key(|candidate| candidate.0)
    else {
        return Vec::new();
    };
    let Some(before) = scroll_sample(automation, &element, &pattern) else {
        return Vec::new();
    };
    let original = before.offset_milli as f64 / 10.0;
    let away_percent = if original < 50.0 { 100.0 } else { 0.0 };
    let horizontal = unsafe { pattern.CurrentHorizontalScrollPercent() }.ok();
    let Some(horizontal) = horizontal else {
        return Vec::new();
    };
    if unsafe { pattern.SetScrollPercent(horizontal, away_percent) }.is_err() {
        return Vec::new();
    }
    std::thread::sleep(Duration::from_millis(120));
    let away = scroll_sample(automation, &element, &pattern);
    let restored = unsafe { pattern.SetScrollPercent(horizontal, original) }.is_ok();
    if !restored {
        return Vec::new();
    }
    std::thread::sleep(Duration::from_millis(120));
    let returned = scroll_sample(automation, &element, &pattern);
    std::thread::sleep(Duration::from_millis(120));
    let confirmed = scroll_sample(automation, &element, &pattern);
    crate::domain::native_oracle::scroll_round_trip_changes(
        Some(&before),
        away.as_ref(),
        returned.as_ref(),
        confirmed.as_ref(),
    )
}

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
