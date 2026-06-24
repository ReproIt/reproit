//! The app map as LIVE state: every exploration/fuzz run's EXPLORE records
//! merge into .reproit/appmap.json (states/transitions union by semantics
//! signature) and .reproit/visits.json (per-sig visit counts + the start
//! state). Frontier fuzzing and author v2 path over this; `reproit map` is
//! the explicit build/label entry point.

use crate::appmap::{
    Action, AppMap, OperabilityGap, OperabilityGaps, Reversibility, State, StateSignature,
    Transition,
};
use crate::config::Config;
use crate::orchestrator;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

/// One run's observations, keyed by semantics signature.
pub(crate) struct RunObs {
    /// sig -> (labels, unlabeled tappable count)
    pub states: BTreeMap<String, (Vec<String>, u32)>,
    /// sig -> route/page identity, when the runner reports one. Framework-neutral:
    /// any runner that puts `"route"` in its EXPLORE:STATE record (the Flutter
    /// route anchor, the web URL path, ...) gets it merged, so the candidate map
    /// can reconcile by route instead of by a name that may not line up.
    pub routes: BTreeMap<String, String>,
    /// sig -> number of tappable elements the runner offered on that state (the
    /// `EXPLORE:STATE` `elements` count). Lets the dead-end oracle tell a proven
    /// sink (no actions, or all actions tried with no forward exit) from a page
    /// the walk simply never finished exploring (it offered tappables it never
    /// tapped). 0 when the runner does not report elements.
    pub tappables: BTreeMap<String, usize>,
    /// sig -> the selectors of the UNLABELED tappables on that state (the
    /// `EXPLORE:STATE` elements flagged `unlabeled`). Lets the a11y oracle name
    /// WHICH control lacks a label and recognize a persistent-chrome control (the
    /// same selector unlabeled on many screens) as one issue, not one per page.
    /// Empty for runners that don't flag per-element labeling.
    pub unlabeled_els: BTreeMap<String, Vec<String>>,
    /// (from sig, action string e.g. "tap:X"/"back", to sig)
    pub edges: Vec<(String, String, String)>,
    /// First state observed: the app's start state.
    pub start: Option<String>,
    /// Routes that have a forward (non-back) exit in the AGGREGATE map, folded in
    /// by the caller (the per-seed graph is too sparse on its own). The dead-end
    /// oracle treats a state on such a route as escapable, so an animated
    /// single-page snapshot whose one seed recorded no exit is not a false sink.
    /// Empty unless a caller populates it (parse_run leaves it empty).
    pub escapable_routes: std::collections::BTreeSet<String>,
    /// sig -> operability/accessibility gaps, from `EXPLORE:GROUNDTRUTH` records
    /// (the graph-1-minus-graph-2 diff). Empty for runners that don't emit it.
    pub gaps: BTreeMap<String, OperabilityGaps>,
    /// (from sig, action) -> the persistent anchors that were torn down and
    /// rebuilt during that transition, from `EXPLORE:RERENDER` records (the
    /// re-render flicker oracle). A transition that rebuilds chrome which did
    /// not change flickers; empty for runners/transitions that don't emit it.
    /// Last write wins, so a deterministic walk yields a stable per-transition
    /// key the `rerender-flicker` invariant and `check` re-confirm against.
    pub rerenders: BTreeMap<(String, String), Vec<String>>,
    /// (from sig, action) -> peak transient-divergence magnitude, from the gated
    /// `EXPLORE:FLICKER` records (Tier-2 pixel oracle, REPROIT_FLICKER_PIXELS).
    /// A frame that diverged from both endpoints mid-transition then settled.
    /// Empty unless the pixel oracle is enabled.
    pub paint_flickers: BTreeMap<(String, String), f64>,
    /// sig -> the overflowing/clipped nodes in that state, from `EXPLORE:OVERFLOW`
    /// records (the DOM/layout overflow oracle). Each entry is `(key, kind, by)`:
    /// the offending node's stable key, the signal (`scroll`/`clip`/`spill`), and
    /// how many CSS pixels it overflowed by. Deterministic structural measurement,
    /// so it re-confirms on replay; empty for runners/states that don't emit it.
    pub overflows: BTreeMap<String, Vec<(String, String, i64)>>,
    /// sig -> rendered broken-content artifacts in that state, from
    /// `EXPLORE:CONTENTBUG` records (the content-bug oracle). Each entry is
    /// `(key, reason, text)`: the offending node's stable key, the artifact class
    /// (`object-object`/`undefined`/`null`/`nan`/`unrendered-template`), and the
    /// clipped visible text. Pure DOM/label scan, so it re-confirms on replay;
    /// empty for runners/states that render no broken content.
    pub content_bugs: BTreeMap<String, Vec<(String, String, String)>>,
    /// (from sig, action) -> `(bucket, unit)` of a main-thread JANK stall on that
    /// transition, from `EXPLORE:JANK` records. `bucket` is the coarse magnitude
    /// and `unit` names what it measures ("ms" on the web Long-Tasks tier; a runner
    /// without frame timing may report e.g. "pct" janky frames), so the message
    /// never claims milliseconds for a non-ms metric. Empty unless an action janked.
    pub janks: BTreeMap<(String, String), (i64, String)>,
    /// (from sig, action) -> `(bucket, unit)` of a main-thread HANG/freeze on that
    /// transition, from `EXPLORE:HANG` records (the same watchdog at a higher
    /// floor). `unit` is "ms" on the web tier, but e.g. "keypresses" on the TUI
    /// (a PTY has no frame clock, so the floor is a count of ignored inputs). Empty
    /// unless an action froze the UI past the hang floor.
    pub hangs: BTreeMap<(String, String), (i64, String)>,
    /// Choice-anomaly findings, from `EXPLORE:CHOICEBUG` records (the
    /// component-choice differential oracle): one entry per multi-choice
    /// component whose options behave UNIFORMLY except one outlier. Each is
    /// `(from sig, role, outlier_label, sel, magnitude_px)`: the state, the
    /// choice role (tab/radio/...), the option that deviated, its selector, and
    /// how many px of global layout it moved. Empty unless a component has an
    /// odd-one-out choice.
    pub choice_bugs: Vec<(String, String, String, String, i64)>,
    /// Broken-route findings, from `EXPLORE:BROKENROUTE` records: a state whose
    /// document responded with a dead-link status (404/410/5xx; the runner
    /// excludes auth-gate 401/403 and rate-limit 429). Each is `(sig, route,
    /// status, from)`, where `from` is the SOURCE state sig that linked to the
    /// dead route (the runner records it at the tap), so the clip attributes the
    /// dead link to the exact page instead of reverse-matching by destination.
    /// `from` is None for a route reached without an in-app navigation (start URL).
    /// Empty unless a visited URL came back broken.
    pub broken_routes: Vec<(String, String, i64, Option<String>)>,
}

/// Compute a state's operability gaps from an `EXPLORE:GROUNDTRUTH` element
/// list. Each element carries `operable` (graph 1) and an `a11y` object with
/// `inTabOrder`/`keyboardActivatable`/`rolePresent`; a gap is a ground-truth-
/// operable element that fails an accessibility dimension. Pure + deterministic.
fn gaps_from_groundtruth(json: &Value) -> OperabilityGaps {
    let mut g = OperabilityGaps {
        focus_trap: json
            .get("focusTrap")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    };
    let Some(els) = json.get("elements").and_then(Value::as_array) else {
        return g;
    };
    for el in els {
        if !el.get("operable").and_then(Value::as_bool).unwrap_or(false) {
            continue; // not ground-truth operable -> not a gap candidate
        }
        let a = el.get("a11y");
        let get = |k: &str| a.and_then(|a| a.get(k)).and_then(Value::as_bool);
        // Default the a11y dims to "true" when unreported, so a missing field is
        // never counted as a gap (conservative: only count confirmed failures).
        let mut kinds: Vec<String> = Vec::new();
        if !get("keyboardActivatable").unwrap_or(true) {
            g.pointer_only += 1;
            kinds.push("pointer_only".into());
        }
        if !get("inTabOrder").unwrap_or(true) {
            g.keyboard_unreachable += 1;
            kinds.push("keyboard_unreachable".into());
        }
        if !get("rolePresent").unwrap_or(true) {
            g.no_role += 1;
            kinds.push("no_role".into());
        }
        // Keep the grounded per-element detail: which selector failed which
        // dimension(s), so the diff is actionable, not just a tally.
        if !kinds.is_empty() {
            let selector = el
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            g.items.push(OperabilityGap { selector, kinds });
        }
    }
    g
}

pub(crate) fn parse_run(log: &str) -> RunObs {
    let mut obs = RunObs {
        states: BTreeMap::new(),
        routes: BTreeMap::new(),
        tappables: BTreeMap::new(),
        unlabeled_els: BTreeMap::new(),
        edges: Vec::new(),
        start: None,
        escapable_routes: std::collections::BTreeSet::new(),
        gaps: BTreeMap::new(),
        rerenders: BTreeMap::new(),
        paint_flickers: BTreeMap::new(),
        overflows: BTreeMap::new(),
        content_bugs: BTreeMap::new(),
        janks: BTreeMap::new(),
        hangs: BTreeMap::new(),
        choice_bugs: Vec::new(),
        broken_routes: Vec::new(),
    };
    for line in log.lines() {
        if let Some(json) = extract(line, "EXPLORE:STATE ") {
            if let (Some(sig), Some(labels)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("labels").and_then(Value::as_array),
            ) {
                if obs.start.is_none() {
                    obs.start = Some(sig.to_string());
                }
                let unlabeled = json.get("unlabeled").and_then(Value::as_u64).unwrap_or(0) as u32;
                // Route is optional and runner-supplied; record the first
                // non-empty one seen for a signature.
                if let Some(route) = json.get("route").and_then(Value::as_str) {
                    if !route.is_empty() {
                        obs.routes
                            .entry(sig.to_string())
                            .or_insert_with(|| route.to_string());
                    }
                }
                // Tappable count: how many actionable elements the state offered.
                if let Some(els) = json.get("elements").and_then(Value::as_array) {
                    obs.tappables.entry(sig.to_string()).or_insert(els.len());
                    // Selectors of the UNLABELED tappables (per-element flag), so
                    // the a11y oracle can name the control and dedup persistent
                    // chrome across screens.
                    let unlabeled_sels: Vec<String> = els
                        .iter()
                        .filter(|e| e.get("unlabeled").and_then(Value::as_bool).unwrap_or(false))
                        .filter_map(|e| e.get("sel").and_then(Value::as_str).map(String::from))
                        .collect();
                    if !unlabeled_sels.is_empty() {
                        obs.unlabeled_els
                            .entry(sig.to_string())
                            .or_insert(unlabeled_sels);
                    }
                }
                obs.states.entry(sig.to_string()).or_insert_with(|| {
                    (
                        labels
                            .iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect(),
                        unlabeled,
                    )
                });
            }
        } else if let Some(json) = extract(line, "EXPLORE:EDGE ") {
            if let (Some(f), Some(a), Some(t)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
                json.get("to").and_then(Value::as_str),
            ) {
                obs.edges
                    .push((f.to_string(), a.to_string(), t.to_string()));
            }
        } else if let Some(json) = extract(line, "EXPLORE:GROUNDTRUTH ") {
            // The operability/accessibility graph for a state: ground-truth
            // operable elements vs their a11y/keyboard dimensions. We store the
            // computed gap counts keyed by signature (last write wins).
            if let Some(sig) = json.get("sig").and_then(Value::as_str) {
                obs.gaps
                    .insert(sig.to_string(), gaps_from_groundtruth(&json));
            }
        } else if let Some(json) = extract(line, "EXPLORE:RERENDER ") {
            // A transition that rebuilt persistent chrome which did not change:
            // the re-render flicker oracle. Key by (from, action); the churned
            // anchor selectors are the grounded detail.
            if let (Some(from), Some(action), Some(churned)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
                json.get("churned").and_then(Value::as_array),
            ) {
                let keys: Vec<String> = churned
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect();
                if !keys.is_empty() {
                    obs.rerenders
                        .insert((from.to_string(), action.to_string()), keys);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:FLICKER ") {
            // Gated Tier-2 pixel oracle: a transient divergence during a
            // transition. Keyed by (from, action); the peak magnitude is detail.
            if let (Some(from), Some(action), Some(peak)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
                json.get("peak").and_then(Value::as_f64),
            ) {
                obs.paint_flickers
                    .insert((from.to_string(), action.to_string()), peak);
            }
        } else if let Some(json) = extract(line, "EXPLORE:OVERFLOW ") {
            // DOM/layout overflow for a state: nodes whose content is clipped or
            // overflows their container/viewport. Keyed by signature (last write
            // wins); each item is (key, kind, by-pixels), the grounded detail.
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String, i64)> = items
                    .iter()
                    .filter_map(|it| {
                        let key = it.get("key").and_then(Value::as_str)?.to_string();
                        let kind = it.get("kind").and_then(Value::as_str)?.to_string();
                        let by = it.get("by").and_then(Value::as_i64).unwrap_or(0);
                        Some((key, kind, by))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.overflows.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:CONTENTBUG ") {
            // Broken rendered content for a state: labels carrying a stringify/
            // template artifact ([object Object], undefined/null/NaN, an
            // unrendered {{...}}). Keyed by signature (last write wins); each item
            // is (key, reason, text), the grounded detail.
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let key = it.get("key").and_then(Value::as_str)?.to_string();
                        let reason = it.get("reason").and_then(Value::as_str)?.to_string();
                        let text = it
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        Some((key, reason, text))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.content_bugs.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:JANK ") {
            // A main-thread JANK stall on a transition (Long Tasks trace). Keyed by
            // (from, action); the value is the coarse blocked-time bucket (ms).
            if let (Some(from), Some(action)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
            ) {
                let bucket = json.get("bucket").and_then(Value::as_i64).unwrap_or(0);
                let unit = parse_metric_unit(&json);
                obs.janks
                    .insert((from.to_string(), action.to_string()), (bucket, unit));
            }
        } else if let Some(json) = extract(line, "EXPLORE:HANG ") {
            // A main-thread HANG/freeze on a transition (Long Tasks trace, higher
            // floor). Keyed by (from, action); value is the blocked-time bucket.
            if let (Some(from), Some(action)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
            ) {
                let bucket = json.get("bucket").and_then(Value::as_i64).unwrap_or(0);
                let unit = parse_metric_unit(&json);
                obs.hangs
                    .insert((from.to_string(), action.to_string()), (bucket, unit));
            }
        } else if let Some(json) = extract(line, "EXPLORE:CHOICEBUG ") {
            // A component-choice outlier: a multi-choice component whose options
            // behave uniformly except one that deviates on the global layout.
            if let (Some(from), Some(role), Some(outlier), Some(sel)) = (
                json.get("from").and_then(Value::as_str),
                json.get("role").and_then(Value::as_str),
                json.get("outlier").and_then(Value::as_str),
                json.get("sel").and_then(Value::as_str),
            ) {
                let mag = json.get("magnitude").and_then(Value::as_i64).unwrap_or(0);
                obs.choice_bugs.push((
                    from.to_string(),
                    role.to_string(),
                    outlier.to_string(),
                    sel.to_string(),
                    mag,
                ));
            }
        } else if let Some(json) = extract(line, "EXPLORE:BROKENROUTE ") {
            // A dead route: the document for this state's URL responded 4xx/5xx.
            if let Some(sig) = json.get("sig").and_then(Value::as_str) {
                let route = json
                    .get("route")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let status = json.get("status").and_then(Value::as_i64).unwrap_or(0);
                let from = json.get("from").and_then(Value::as_str).map(str::to_string);
                obs.broken_routes
                    .push((sig.to_string(), route, status, from));
            }
        }
    }
    obs
}

fn extract(line: &str, marker: &str) -> Option<Value> {
    let idx = line.find(marker)?;
    serde_json::from_str(line[idx + marker.len()..].trim()).ok()
}

pub(crate) fn load_map(root: &Path, cfg: &Config) -> AppMap {
    std::fs::read_to_string(root.join(".reproit/appmap.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| AppMap {
            app: cfg.app.bundle_id.clone(),
            version: 1,
            states: BTreeMap::new(),
            transitions: Vec::new(),
            invariants: Vec::new(),
            interrupts: Vec::new(),
        })
}

fn save_map(root: &Path, map: &AppMap) -> Result<()> {
    let out = root.join(".reproit/appmap.json");
    std::fs::create_dir_all(out.parent().unwrap())?;
    std::fs::write(&out, serde_json::to_string_pretty(map)?)?;
    Ok(())
}

/// sig -> existing state id (states are keyed by id; sig lives in the
/// signature, so labeling renames never break identity).
fn sig_index(map: &AppMap) -> HashMap<String, String> {
    map.states
        .iter()
        .filter_map(|(id, s)| {
            s.signature
                .semantics_hash
                .clone()
                .map(|sig| (sig, id.clone()))
        })
        .collect()
}

/// Union this run's observations into the map (by sig).
pub(crate) fn merge(map: &mut AppMap, obs: &RunObs) {
    let mut index = sig_index(map);
    for (sig, (labels, unlabeled)) in &obs.states {
        match index.get(sig) {
            Some(id) => {
                // Known state: refresh the a11y observation (fixes show up
                // as the count dropping on the next exploration), and backfill
                // the route if a later run reported one we didn't have.
                if let Some(state) = map.states.get_mut(id) {
                    state.unlabeled_tappables = *unlabeled;
                    if let Some(g) = obs.gaps.get(sig) {
                        state.operability_gaps = g.clone();
                    }
                    if state.signature.route.is_none() {
                        if let Some(r) = obs.routes.get(sig) {
                            state.signature.route = Some(r.clone());
                        }
                    }
                }
            }
            None => {
                let id = format!("s_{sig}");
                map.states.insert(
                    id.clone(),
                    State {
                        description: labels
                            .iter()
                            .take(4)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                        signature: StateSignature {
                            screenshot_phash: None,
                            semantics_hash: Some(sig.clone()),
                            route: obs.routes.get(sig).cloned(),
                        },
                        parameters: vec![],
                        unlabeled_tappables: *unlabeled,
                        operability_gaps: obs.gaps.get(sig).cloned().unwrap_or_default(),
                    },
                );
                index.insert(sig.clone(), id);
            }
        }
    }
    let existing: std::collections::HashSet<String> = map
        .transitions
        .iter()
        .map(|t| format!("{}|{}|{}", t.from, action_str(&t.action), t.to))
        .collect();
    for (from, action, to) in &obs.edges {
        let (Some(f), Some(t)) = (index.get(from), index.get(to)) else {
            continue;
        };
        let key = format!("{f}|{action}|{t}");
        if existing.contains(&key) {
            continue;
        }
        map.transitions.push(Transition {
            from: f.clone(),
            to: t.clone(),
            action: parse_action(action),
            guards: vec![],
            reversibility: Reversibility::ProposedReversible,
            expected: None,
        });
    }
}

/// The metric unit on a JANK/HANG marker -- what `bucket` measures. Defaults to
/// "ms" (the web Long-Tasks tier), so a marker without an explicit `unit` keeps
/// the historical millisecond meaning. A runner whose floor is NOT milliseconds
/// (the TUI's ignored-keypress count; an RSS-only tier's janky-frame percent)
/// sets `unit` so the rendered message doesn't claim "ms" for a count/percent.
fn parse_metric_unit(json: &Value) -> String {
    json.get("unit")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("ms")
        .to_string()
}

pub(crate) fn action_str(a: &Action) -> String {
    match a {
        Action::Tap { finder } => {
            format!("tap:{}", finder.strip_prefix("label:").unwrap_or(finder))
        }
        Action::Back => "back".to_string(),
        Action::Type { finder, .. } => format!("type:{finder}"),
        Action::Scroll { finder, .. } => format!("scroll:{finder}"),
        Action::System { event } => format!("system:{event}"),
    }
}

/// Inverse of [`action_str`]: parse an `EXPLORE:EDGE` action string back into an
/// `Action`. `type:`/`scroll:`/`system:` MUST be parsed into their real variants
/// (not collapsed to `Back`) or a form-driven transition lands in the persisted
/// map as a meaningless `back` edge -- losing the finder/value, so the screen
/// behind a typed input becomes unreplayable and frontier guidance over the map
/// is wrong wherever a state is only reachable through typed input.
fn parse_action(s: &str) -> Action {
    if let Some(l) = s.strip_prefix("tap:") {
        return Action::Tap {
            finder: format!("label:{l}"),
        };
    }
    if let Some(rest) = s.strip_prefix("type:") {
        // The runner emits `type:<finder>=<text>`; the `=<text>` is optional.
        let (finder, text) = match rest.split_once('=') {
            Some((f, t)) => (f.to_string(), t.to_string()),
            None => (rest.to_string(), String::new()),
        };
        return Action::Type { finder, text };
    }
    if let Some(rest) = s.strip_prefix("scroll:") {
        // `scroll:<finder>` or `scroll:<finder>=<dy>` (dy optional/recoverable).
        let (finder, dy) = match rest.rsplit_once('=') {
            Some((f, d)) => (f.to_string(), d.parse().unwrap_or(0)),
            None => (rest.to_string(), 0),
        };
        return Action::Scroll { finder, dy };
    }
    if let Some(ev) = s.strip_prefix("system:") {
        return Action::System {
            event: ev.to_string(),
        };
    }
    Action::Back
}

/// The app's entry state: one with no incoming transition, else the first by
/// name. Where authoring/exploration starts.
// Graph helpers retained for the agnostic journey executor (goto pathfinding)
// and MCP/agent grounding; the journeys feature wires them back in.
#[allow(dead_code)]
pub(crate) fn entry_state(map: &AppMap) -> Option<String> {
    let has_incoming: std::collections::BTreeSet<&str> =
        map.transitions.iter().map(|t| t.to.as_str()).collect();
    map.states
        .keys()
        .find(|k| !has_incoming.contains(k.as_str()))
        .or_else(|| map.states.keys().next())
        .cloned()
}

/// Shortest action path from the entry state to the first state whose name OR
/// description matches `needle` (case-insensitive substring). BFS over
/// transitions. The authoring agent uses this to ground a generated journey in
/// the app's REAL navigation (discovered by `reproit map`) instead of
/// hallucinated taps. Returns (target_state_name, ordered action strings); the
/// path is empty when the entry state itself matches.
#[allow(dead_code)]
pub(crate) fn path_to_label(map: &AppMap, needle: &str) -> Option<(String, Vec<String>)> {
    let start = entry_state(map)?;
    let needle = needle.to_lowercase();
    let matches = |name: &str| -> bool {
        name.to_lowercase().contains(&needle)
            || map
                .states
                .get(name)
                .map(|s| s.description.to_lowercase().contains(&needle))
                .unwrap_or(false)
    };
    let mut adj: BTreeMap<&str, Vec<(String, &str)>> = BTreeMap::new();
    for t in &map.transitions {
        adj.entry(t.from.as_str())
            .or_default()
            .push((action_str(&t.action), t.to.as_str()));
    }
    let mut q = std::collections::VecDeque::new();
    let mut prev: BTreeMap<&str, (&str, String)> = BTreeMap::new();
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    q.push_back(start.as_str());
    seen.insert(start.as_str());
    let mut goal: Option<&str> = matches(&start).then_some(start.as_str());
    while goal.is_none() {
        let Some(cur) = q.pop_front() else { break };
        for (act, to) in adj.get(cur).into_iter().flatten() {
            if seen.insert(to) {
                prev.insert(to, (cur, act.clone()));
                if matches(to) {
                    goal = Some(to);
                    break;
                }
                q.push_back(to);
            }
        }
    }
    let goal = goal?;
    let mut path = Vec::new();
    let mut node = goal;
    while let Some((parent, act)) = prev.get(node) {
        path.push(act.clone());
        node = parent;
    }
    path.reverse();
    Some((goal.to_string(), path))
}

/// Compact "From --action--> To" edge list, for grounding the authoring prompt
/// in the app's real transitions.
#[allow(dead_code)]
pub(crate) fn edges_summary(map: &AppMap) -> Vec<String> {
    map.transitions
        .iter()
        .map(|t| format!("{} --{}--> {}", t.from, action_str(&t.action), t.to))
        .collect()
}

/// Visit counts keyed by sig + the start state. Rename-proof.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct Visits {
    pub start: Option<String>,
    pub counts: BTreeMap<String, u64>,
    /// Per-edge traversal counts keyed "fromSig|action" (action e.g.
    /// "tap:Beacons"/"back"). Feeds inverse-visit-count action scoring.
    #[serde(default)]
    pub edge_counts: BTreeMap<String, u64>,
}

/// Cap on the destination visit count used for edge weighting. The pick weight
/// is `1/(1+count)`, so an uncapped count lets a frequently-visited HUB action
/// (e.g. "add to cart", "open cart", actions you MUST repeat to reach deep
/// states) decay toward zero weight and the walk learns to avoid it, starving
/// the very paths that gate depth. Capping the count floors the weight at
/// `1/(1+CAP)`, preserving the inverse-visit bias (new states still strongly
/// preferred) while keeping hub actions reachable.
const VISIT_WEIGHT_CAP: u64 = 8;

impl Visits {
    /// edgeWeights[fromSig][action] = DESTINATION-state visit count (capped at
    /// [`VISIT_WEIGHT_CAP`]), for the explorer's pick (weight ~ 1/(1+count)).
    /// Weighting by where an edge LEADS (reward edges to rarely-seen states)
    /// rather than by how often the edge was traversed (which penalized the
    /// productive deep "Next" edges and fought depth, per the A/B). Unknown
    /// edges aren't listed, so the explorer treats them as count 0 = max weight
    /// = worth trying. Needs the map to resolve action targets.
    pub fn edge_weights(&self, map: &AppMap) -> BTreeMap<String, BTreeMap<String, u64>> {
        let sig_of: BTreeMap<&str, &str> = map
            .states
            .iter()
            .filter_map(|(id, s)| {
                s.signature
                    .semantics_hash
                    .as_deref()
                    .map(|sig| (id.as_str(), sig))
            })
            .collect();
        let mut out: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for t in &map.transitions {
            let (Some(&from_sig), Some(&to_sig)) =
                (sig_of.get(t.from.as_str()), sig_of.get(t.to.as_str()))
            else {
                continue;
            };
            let dest_visits = self
                .counts
                .get(to_sig)
                .copied()
                .unwrap_or(0)
                .min(VISIT_WEIGHT_CAP);
            out.entry(from_sig.to_string())
                .or_default()
                .insert(action_str(&t.action), dest_visits);
        }
        out
    }
}

pub(crate) fn load_visits(root: &Path) -> Visits {
    std::fs::read_to_string(root.join(".reproit/visits.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_visits(root: &Path, v: &Visits) -> Result<()> {
    std::fs::write(
        root.join(".reproit/visits.json"),
        serde_json::to_string_pretty(v)?,
    )?;
    Ok(())
}

/// Merge one run's observations into an IN-MEMORY map + visits, returning the
/// parsed observations. Does no I/O, so callers that must stay pure (notably
/// `fuzz`, which reports discoveries but never mutates the committed graph) can
/// accrue cross-seed/cross-batch coverage guidance within a single invocation
/// without touching `.reproit/appmap.json` / `.reproit/visits.json`.
pub(crate) fn absorb_run_inmem(map: &mut AppMap, visits: &mut Visits, log: &str) -> RunObs {
    let obs = parse_run(log);
    if obs.states.is_empty() {
        return obs;
    }
    merge(map, &obs);
    if visits.start.is_none() {
        visits.start = obs.start.clone();
    }
    for sig in obs.states.keys() {
        *visits.counts.entry(sig.clone()).or_insert(0) += 1;
    }
    for (from, action, _to) in &obs.edges {
        *visits
            .edge_counts
            .entry(format!("{from}|{action}"))
            .or_insert(0) += 1;
    }
    obs
}

/// Merge one run's observations into both live files and persist them. This is
/// `map`'s commit path: `map` is what folds discovered coverage into the
/// committed graph. `fuzz` must NOT call this (it would make a fixed seed drift
/// across invocations as visit counts accumulate); it uses [`absorb_run_inmem`].
pub(crate) fn absorb_run(root: &Path, cfg: &Config, log: &str) -> Result<RunObs> {
    let mut map = load_map(root, cfg);
    let mut visits = load_visits(root);
    let obs = absorb_run_inmem(&mut map, &mut visits, log);
    if !obs.states.is_empty() {
        save_map(root, &map)?;
        save_visits(root, &visits)?;
    }
    Ok(obs)
}

/// BFS shortest action-path from the start state to the least-visited
/// reachable state (ties: prefer deeper, to push the frontier outward).
pub(crate) fn frontier_path(map: &AppMap, visits: &Visits) -> Option<(String, Vec<String>)> {
    let index = sig_index(map);
    let start_sig = visits.start.clone()?;
    let start_id = index.get(&start_sig)?.clone();

    let mut adj: HashMap<&str, Vec<(&Transition, &str)>> = HashMap::new();
    for t in &map.transitions {
        adj.entry(t.from.as_str())
            .or_default()
            .push((t, t.to.as_str()));
    }
    let sig_of: HashMap<&str, &str> = map
        .states
        .iter()
        .filter_map(|(id, s)| {
            s.signature
                .semantics_hash
                .as_deref()
                .map(|sig| (id.as_str(), sig))
        })
        .collect();

    let mut paths: HashMap<String, Vec<String>> = HashMap::new();
    paths.insert(start_id.clone(), vec![]);
    let mut queue = VecDeque::from([start_id.clone()]);
    while let Some(id) = queue.pop_front() {
        let path = paths[&id].clone();
        for (t, to) in adj.get(id.as_str()).cloned().unwrap_or_default() {
            if paths.contains_key(to) {
                continue;
            }
            let mut p = path.clone();
            p.push(action_str(&t.action));
            paths.insert(to.to_string(), p);
            queue.push_back(to.to_string());
        }
    }

    paths
        .iter()
        .filter(|(id, _)| **id != start_id)
        // Deterministic frontier choice: least-visited, then deepest path, then a
        // STABLE tie-break on the structural signature (and id). Without the last
        // two keys a tie resolved on `HashMap` iteration order, which is randomized
        // per run -- so `fuzz --frontier` picked a different target (and replayed a
        // different prefix for every seed) run-to-run, breaking reproducibility.
        .min_by_key(|(id, path)| {
            let sig = sig_of.get(id.as_str()).copied().unwrap_or("");
            let count = visits.counts.get(sig).copied().unwrap_or(0);
            (count, usize::MAX - path.len(), sig, id.as_str())
        })
        .map(|(id, path)| (id.clone(), path.clone()))
}

/// Concatenate every device's drive log in a run dir (`drive-a.log`,
/// `drive-b.log`, ...), sorted by name, so a multi-actor run's full traversal
/// feeds the map and not just device a's. A single-device run just yields
/// `drive-a.log`.
fn read_all_device_logs(run_dir: &Path) -> Result<String> {
    let mut logs: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(run_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("drive-") && name.ends_with(".log") {
                if let Ok(s) = std::fs::read_to_string(e.path()) {
                    logs.push((name, s));
                }
            }
        }
    }
    if logs.is_empty() {
        anyhow::bail!("no drive-*.log files in {}", run_dir.display());
    }
    logs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(logs
        .into_iter()
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join("\n"))
}

pub async fn build_map(
    cfg: &Config,
    root: &Path,
    journey: &str,
    label: bool,
    from_run: Option<&Path>,
) -> Result<()> {
    let run_dir = match from_run {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => root.join(p),
        None => {
            let outcome = orchestrator::run_journey(
                cfg,
                root,
                journey,
                &orchestrator::RunOpts {
                    devices: 1,
                    ..Default::default()
                },
            )
            .await?;
            if !outcome.passed {
                eprintln!(
                    "  note: exploration run did not pass cleanly; mapping what was observed"
                );
            }
            outcome.run_dir
        }
    };
    // Fold in EVERY device's log, not just device a: a multi-actor scenario run
    // has each actor traverse different (often deeper) screens, and a scenario
    // now emits the same EXPLORE records the crawl does, so the dual-user
    // journeys double as the mapper for screens a single actor can't reach.
    let log = read_all_device_logs(&run_dir)?;
    let obs = absorb_run(root, cfg, &log)?;
    if obs.states.is_empty() {
        anyhow::bail!(
            "no EXPLORE:STATE records in {} (is the explorer journey installed? see templates/explorer.dart)",
            run_dir.display()
        );
    }

    if label {
        let mut map = load_map(root, cfg);
        let state_labels: BTreeMap<String, Vec<String>> = map
            .states
            .values()
            .filter_map(|s| {
                let sig = s.signature.semantics_hash.clone()?;
                Some((sig, s.description.split(", ").map(String::from).collect()))
            })
            .collect();
        match label_states(cfg, &state_labels).await {
            Ok(names) => {
                let index = sig_index(&map);
                let mut renames: Vec<(String, String)> = Vec::new();
                for (sig, name) in names {
                    if let Some(old_id) = index.get(&sig) {
                        if old_id != &name && !map.states.contains_key(&name) {
                            renames.push((old_id.clone(), name));
                        }
                    }
                }
                for (old, new) in renames {
                    if let Some(state) = map.states.remove(&old) {
                        map.states.insert(new.clone(), state);
                        for t in &mut map.transitions {
                            if t.from == old {
                                t.from = new.clone();
                            }
                            if t.to == old {
                                t.to = new.clone();
                            }
                        }
                    }
                }
                save_map(root, &map)?;
            }
            Err(e) => eprintln!("  warn: labeling pass failed ({e}); keeping current names"),
        }
    }

    let map = load_map(root, cfg);
    // Progress lines go to STDERR: stdout is reserved for machine output (e.g. a
    // `--json` sweep/fuzz that auto-builds the map on first run), and these landing
    // on stdout corrupted the JSON object a piped consumer parses.
    eprintln!(
        "  map: {} states, {} transitions -> {}",
        map.states.len(),
        map.transitions.len(),
        root.join(".reproit/appmap.json").display()
    );
    eprintln!("  view: reproit map show --format html --out map.html");
    Ok(())
}

/// Ask the LLM to name states from their visible labels. Resilient: any
/// parse failure keeps the current names.
async fn label_states(
    cfg: &Config,
    state_labels: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, String>> {
    let provider = llm::from_spec(&cfg.llm.to_spec())?;
    let mut listing = String::new();
    for (sig, labels) in state_labels {
        listing.push_str(&format!("{sig}: {}\n", labels.join(" | ")));
    }
    let prompt = format!(
        "These are screens of a mobile app, identified by signature, with the visible \
semantic labels observed on each. Give each a short snake_case name (login, meet_feed, \
profile, settings, ...). Reply with ONLY a JSON object mapping signature to name, no \
commentary, no code fences.\n\n{listing}"
    );
    let response = provider.complete(&llm::Task::new(prompt)).await?;
    let json_str = response
        .find('{')
        // Guard the slice: an LLM reply could place `}` before its first `{`, and
        // `&response[s..=e]` would panic when e < s. Require e >= s.
        .and_then(|s| {
            response
                .rfind('}')
                .filter(|&e| e >= s)
                .map(|e| &response[s..=e])
        })
        .context("no JSON object in labeling response")?;
    let parsed: BTreeMap<String, String> = serde_json::from_str(json_str)?;
    let mut used = std::collections::HashSet::new();
    let mut out = BTreeMap::new();
    for (sig, name) in parsed {
        let mut clean: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string();
        if clean.is_empty() || clean.chars().next().unwrap().is_ascii_digit() {
            clean = format!("s_{sig}");
        }
        while !used.insert(clean.clone()) {
            clean.push('_');
        }
        out.insert(sig, clean);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VERBATIM `EXPLORE:GROUNDTRUTH` JSON each in-process operability agent
    /// emits, kept in ONE shared place: `tests/golden/operability/<platform>.json`
    /// (byte-for-byte the marker the real agent prints). The engine contract tests
    /// below read these goldens instead of inlining the literal, and a per-platform
    /// capture-diff CI job (.github/workflows/ci.yml) re-runs the real agent, drops
    /// the volatile `sig`, and DIFFs the live marker against the same golden. So the
    /// golden is the single source of truth: if the real marker drifts, the test
    /// keeps asserting the old contract here while the CI diff catches the drift
    /// against production, instead of an inline literal silently going stale.
    fn golden_groundtruth(platform: &str) -> &'static str {
        match platform {
            "web" => include_str!("../../tests/golden/operability/web.json"),
            "appkit" => include_str!("../../tests/golden/operability/appkit.json"),
            "wpf" => include_str!("../../tests/golden/operability/wpf.json"),
            "qt" => include_str!("../../tests/golden/operability/qt.json"),
            "gtk" => include_str!("../../tests/golden/operability/gtk.json"),
            "flutter" => include_str!("../../tests/golden/operability/flutter.json"),
            other => panic!("no operability golden for platform {other:?}"),
        }
        .trim()
    }

    /// Parse a platform's golden marker through the real engine, returning the
    /// state's operability gaps. The golden carries the marker's own `sig`, so we
    /// read it back out of the JSON rather than hard-coding it at each call site.
    fn gaps_from_golden(platform: &str) -> OperabilityGaps {
        let payload = golden_groundtruth(platform);
        let sig = serde_json::from_str::<Value>(payload)
            .expect("golden is valid JSON")
            .get("sig")
            .and_then(Value::as_str)
            .expect("golden carries a sig")
            .to_string();
        let log = format!("EXPLORE:GROUNDTRUTH {payload}");
        parse_run(&log)
            .gaps
            .get(&sig)
            .unwrap_or_else(|| panic!("gaps for the {platform} agent state ({sig})"))
            .clone()
    }

    fn st(desc: &str) -> State {
        State {
            description: desc.to_string(),
            signature: StateSignature {
                screenshot_phash: None,
                semantics_hash: None,
                route: None,
            },
            parameters: vec![],
            unlabeled_tappables: 0,
            operability_gaps: Default::default(),
        }
    }
    fn tap(from: &str, label: &str, to: &str) -> Transition {
        Transition {
            from: from.to_string(),
            to: to.to_string(),
            action: Action::Tap {
                finder: format!("label:{label}"),
            },
            guards: vec![],
            reversibility: Reversibility::ProposedReversible,
            expected: None,
        }
    }
    fn sample() -> AppMap {
        let mut states = BTreeMap::new();
        states.insert("Home".to_string(), st("home screen"));
        states.insert("Settings".to_string(), st("settings screen"));
        states.insert("About".to_string(), st("about / version info"));
        AppMap {
            app: "demo".to_string(),
            version: 1,
            states,
            transitions: vec![
                tap("Home", "Settings", "Settings"),
                tap("Settings", "About", "About"),
            ],
            invariants: vec![],
            interrupts: vec![],
        }
    }

    #[test]
    fn entry_is_the_state_without_incoming_edges() {
        assert_eq!(entry_state(&sample()).as_deref(), Some("Home"));
    }

    #[test]
    fn path_to_label_finds_shortest_action_sequence() {
        let m = sample();
        let (target, path) = path_to_label(&m, "about").expect("About is reachable");
        assert_eq!(target, "About");
        assert_eq!(
            path,
            vec!["tap:Settings".to_string(), "tap:About".to_string()]
        );
        // the entry state itself matching yields an empty path.
        let (t0, p0) = path_to_label(&m, "home").unwrap();
        assert_eq!(t0, "Home");
        assert!(p0.is_empty());
        // an unreachable/unknown label yields None.
        assert!(path_to_label(&m, "nonexistent-screen").is_none());
    }

    #[test]
    fn frontier_path_is_deterministic_on_ties() {
        // Two unvisited frontier states, each one tap from Home: equal visit count
        // AND equal path length, so the pick comes down to the tie-break. Before
        // the fix it resolved on `HashMap` iteration order (a fresh random seed per
        // call), so `fuzz --frontier` could target a different state run-to-run.
        let sig_state = |sig: &str| {
            let mut s = st("x");
            s.signature.semantics_hash = Some(sig.to_string());
            s
        };
        let mut states = BTreeMap::new();
        states.insert("Home".to_string(), sig_state("sig-home"));
        states.insert("Alpha".to_string(), sig_state("sig-alpha"));
        states.insert("Bravo".to_string(), sig_state("sig-bravo"));
        let map = AppMap {
            app: "demo".to_string(),
            version: 1,
            states,
            transitions: vec![tap("Home", "a", "Alpha"), tap("Home", "b", "Bravo")],
            invariants: vec![],
            interrupts: vec![],
        };
        let visits = Visits {
            start: Some("sig-home".to_string()),
            counts: BTreeMap::new(),
            edge_counts: BTreeMap::new(),
        };
        // Stable across many calls (each rebuilds the internal HashMaps with a new
        // seed, so a non-deterministic tie-break would diverge over the loop)...
        let first = frontier_path(&map, &visits).expect("a frontier exists");
        for _ in 0..64 {
            assert_eq!(frontier_path(&map, &visits), Some(first.clone()));
        }
        // ...and it is the smallest-signature tied state (sig-alpha < sig-bravo),
        // not whichever happened to hash first.
        assert_eq!(first.0, "Alpha");
    }

    #[test]
    fn parse_action_recovers_typed_scroll_system_edges() {
        // type:/scroll:/system: must round-trip into their real variants, not
        // collapse to Back (which lost the finder/value of form-driven edges).
        assert!(matches!(parse_action("tap:Go"), Action::Tap { .. }));
        match parse_action("type:role:textfield#0=hello") {
            Action::Type { finder, text } => {
                assert_eq!(finder, "role:textfield#0");
                assert_eq!(text, "hello");
            }
            a => panic!("expected Type, got {a:?}"),
        }
        match parse_action("scroll:key:list=-300") {
            Action::Scroll { finder, dy } => {
                assert_eq!(finder, "key:list");
                assert_eq!(dy, -300);
            }
            a => panic!("expected Scroll, got {a:?}"),
        }
        match parse_action("system:back") {
            Action::System { event } => assert_eq!(event, "back"),
            a => panic!("expected System, got {a:?}"),
        }
        assert!(matches!(parse_action("back"), Action::Back));
        // A typed edge with no `=value` still parses as Type (empty text), not Back.
        assert!(matches!(parse_action("type:key:x"), Action::Type { .. }));
    }

    #[test]
    fn edges_summary_lists_real_transitions() {
        assert!(edges_summary(&sample())
            .iter()
            .any(|e| e == "Home --tap:Settings--> Settings"));
    }

    #[test]
    fn edge_weights_caps_the_visit_count_so_hub_actions_keep_a_floor() {
        // A hub destination visited far more than the cap must not decay the
        // edge weight toward zero: the count feeding 1/(1+count) is clamped to
        // VISIT_WEIGHT_CAP, so the walk can still reach it.
        let sig_state = |sig: &str| {
            let mut s = st("x");
            s.signature.semantics_hash = Some(sig.to_string());
            s
        };
        let mut states = BTreeMap::new();
        states.insert("A".to_string(), sig_state("sigA"));
        states.insert("B".to_string(), sig_state("sigB"));
        let map = AppMap {
            app: "demo".to_string(),
            version: 1,
            states,
            transitions: vec![tap("A", "go", "B")],
            invariants: vec![],
            interrupts: vec![],
        };
        let mut visits = Visits::default();
        visits.counts.insert("sigB".to_string(), 1000); // wildly over-visited hub
        let ew = visits.edge_weights(&map);
        let count = *ew
            .get("sigA")
            .and_then(|m| m.values().next())
            .expect("an edge from sigA");
        assert_eq!(
            count, VISIT_WEIGHT_CAP,
            "the weighting count must be capped, not the raw 1000"
        );
    }

    #[test]
    fn merge_captures_route_from_explore_state() {
        // A runner that reports a route (Flutter anchor, web URL path, ...) lands
        // it on the verified state, so the candidate map can reconcile by route.
        let log = r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":["Home"],"unlabeled":0}"#;
        let obs = parse_run(log);
        assert_eq!(obs.routes.get("abc").map(String::as_str), Some("/home"));
        let mut map = AppMap {
            app: "t".into(),
            version: 1,
            states: BTreeMap::new(),
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        };
        merge(&mut map, &obs);
        let state = map.states.values().next().expect("a merged state");
        assert_eq!(state.signature.route.as_deref(), Some("/home"));
    }

    #[test]
    fn groundtruth_marker_yields_operability_gaps() {
        // The motivating case: a control operable by pointer but not keyboard-
        // reachable and exposing no role (the finding-div in the dashboard). This
        // is the web in-process agent's marker, kept in
        // tests/golden/operability/web.json (sig "abc"); CI re-captures + diffs it.
        let log = format!(
            "{}\nEXPLORE:GROUNDTRUTH {}",
            r#"EXPLORE:STATE {"sig":"abc","labels":[],"unlabeled":0}"#,
            golden_groundtruth("web"),
        );
        let obs = parse_run(&log);
        let g = obs.gaps.get("abc").expect("gaps for abc");
        assert_eq!(
            g.pointer_only, 1,
            "one operable element not keyboard-activatable"
        );
        assert_eq!(
            g.keyboard_unreachable, 1,
            "one operable element not in tab order"
        );
        assert_eq!(g.no_role, 1, "one operable element with no role");
        assert!(!g.focus_trap);
        // The grounded per-element detail: exactly the one failing element, by
        // selector, tagged with every dimension it fails. This is what the
        // accessibility view/MCP tool serves, so it must be present, not a count.
        assert_eq!(g.items.len(), 1, "only the one failing element is recorded");
        assert_eq!(g.items[0].selector, "role:option#0");
        assert_eq!(
            g.items[0].kinds,
            vec!["pointer_only", "keyboard_unreachable", "no_role"],
            "the failing element is tagged with all three dimensions it fails"
        );
        // The non-operable decoration is never a gap; the healthy nav is not either.
        let mut map = AppMap {
            app: "t".into(),
            version: 1,
            states: BTreeMap::new(),
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        };
        merge(&mut map, &obs);
        let state = map.states.values().next().expect("a merged state");
        assert_eq!(state.operability_gaps.pointer_only, 1);
        assert_eq!(state.operability_gaps.keyboard_unreachable, 1);
    }

    #[test]
    fn rerender_marker_yields_keyed_churn() {
        // A transition that rebuilt persistent chrome which did not change: the
        // runner emits EXPLORE:RERENDER with the from sig, the action, and the
        // churned anchor selectors. parse_run keys it by (from, action). A marker
        // with an empty churned list (no flicker) is dropped, not recorded.
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[],"unlabeled":0}"#,
            "\n",
            r#"EXPLORE:RERENDER {"from":"s1","action":"tap:key:id:bad","churned":["id:hdr","id:nav"]}"#,
            "\n",
            r#"EXPLORE:RERENDER {"from":"s1","action":"tap:key:id:good","churned":[]}"#,
        );
        let obs = parse_run(log);
        assert_eq!(
            obs.rerenders.len(),
            1,
            "only the non-empty churn is recorded"
        );
        let churned = obs
            .rerenders
            .get(&("s1".to_string(), "tap:key:id:bad".to_string()))
            .expect("churn for the bad transition");
        assert_eq!(churned, &vec!["id:hdr".to_string(), "id:nav".to_string()]);
        assert!(
            !obs.rerenders
                .contains_key(&("s1".to_string(), "tap:key:id:good".to_string())),
            "the reconciled (empty-churn) transition is not a flicker"
        );
    }

    #[test]
    fn flicker_marker_records_peak_divergence() {
        // The gated Tier-2 pixel oracle: EXPLORE:FLICKER carries the peak
        // transient-divergence magnitude, keyed by (from, action).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[],"unlabeled":0}"#,
            "\n",
            r#"EXPLORE:FLICKER {"from":"s1","action":"tap:key:id:bad","peak":0.82,"frames":7}"#,
        );
        let obs = parse_run(log);
        let peak = obs
            .paint_flickers
            .get(&("s1".to_string(), "tap:key:id:bad".to_string()))
            .expect("paint flicker for the bad transition");
        assert!((peak - 0.82).abs() < 1e-9);
    }

    #[test]
    fn overflow_marker_keys_clipped_nodes_by_sig() {
        // The DOM/layout overflow oracle: EXPLORE:OVERFLOW carries the offending
        // nodes (key, kind, by-pixels), keyed by state signature. A marker with an
        // empty items list is dropped (no overflow -> nothing recorded).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[],"unlabeled":0}"#,
            "\n",
            r#"EXPLORE:OVERFLOW {"sig":"s1","items":[{"key":"id:save","kind":"clip","by":84},{"key":"tag:html","kind":"scroll","by":120}]}"#,
            "\n",
            r#"EXPLORE:OVERFLOW {"sig":"s2","items":[]}"#,
        );
        let obs = parse_run(log);
        let items = obs.overflows.get("s1").expect("overflow for s1");
        assert_eq!(
            items,
            &vec![
                ("id:save".to_string(), "clip".to_string(), 84),
                ("tag:html".to_string(), "scroll".to_string(), 120),
            ]
        );
        assert!(
            !obs.overflows.contains_key("s2"),
            "an empty overflow list is not recorded"
        );
    }

    #[test]
    fn appkit_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process AppKit operability agent
        // (runners/native/appkit-agent/main.swift). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line that the built+run Swift agent emits for a
        // window holding a real NSButton, a "fake button" (custom NSView with a
        // click gesture + handler and no a11y role), and a correctly-built
        // accessible custom control. The engine must score exactly one gap row
        // (the fake button), failing all three a11y dimensions. The marker lives
        // in tests/golden/operability/appkit.json; CI re-captures + diffs it.
        let g = gaps_from_golden("appkit");
        // The fake button alone is an operable-but-inaccessible element.
        assert_eq!(g.no_role, 1, "fake button has no a11y role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button is not in the key-view loop"
        );
        assert_eq!(g.pointer_only, 1, "fake button is pointer-only (gesture)");
        assert!(!g.focus_trap);
    }

    #[test]
    fn wpf_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process WPF operability agent
        // (runners/native/wpf-agent/Program.cs). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line that the built+run agent emits on the Windows
        // VM for a window holding a real <Button> and a "fake button" (a
        // clickable <Border>/<TextBlock> with a MouseLeftButtonUp handler and no
        // Button role / no AutomationProperties). Graph 1 (visual tree + handler
        // reflection) and graph 2 (UIElementAutomationPeer) are joined by object
        // identity. The engine must score exactly one gap row (the fake button),
        // failing all three a11y dimensions; the real Button is clean. The marker
        // lives in tests/golden/operability/wpf.json; CI re-captures + diffs it.
        let g = gaps_from_golden("wpf");
        assert_eq!(g.no_role, 1, "fake button has no Button role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button is not in the tab order"
        );
        assert_eq!(
            g.pointer_only, 1,
            "fake button is pointer-only (mouse handler)"
        );
        assert!(!g.focus_trap);
    }

    #[test]
    fn qt_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process Qt operability agent
        // (runners/native/qt-agent/qt_agent.cpp). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line the built+run agent emits in a Linux
        // container (Debian, Qt 6.8.2, `QT_QPA_PLATFORM=offscreen`) for a window
        // holding a real QPushButton, a "fake button" (custom QWidget with a
        // mousePressEvent handler and no QAccessible role), and a correctly-built
        // accessible control. Graph 1 (QObject tree + wired signals / custom
        // subclass) joins graph 2 (QAccessibleInterface) by object identity. The
        // engine must score exactly one gap row (the fake button), failing all
        // three a11y dimensions; the real button is clean. The signature matches
        // the AppKit agent's (3854aea0): same three-control structural descriptor.
        // The marker lives in tests/golden/operability/qt.json; CI re-captures it.
        let g = gaps_from_golden("qt");
        assert_eq!(g.no_role, 1, "fake button has no QAccessible role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button is not in the tab order"
        );
        assert_eq!(
            g.pointer_only, 1,
            "fake button is pointer-only (mousePressEvent)"
        );
        assert!(!g.focus_trap);
    }

    #[test]
    fn gtk_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process GTK operability agent
        // (runners/native/gtk-agent/gtk_agent.c). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line the built+run agent emits in a Linux container
        // (Debian, GTK 4.18.6, under `xvfb-run`) for a window holding a real
        // GtkButton, a "fake button" (a GtkBox carrying a GtkGestureClick +
        // handler with no button role / not focusable), and a correctly-built
        // accessible GtkButton. Graph 1 (GtkWidget tree + wired signals / click
        // gestures) joins graph 2 (GtkAccessible role/state) by object identity.
        // The fake button is the motivating finding: operable yet rolePresent
        // false and keyboard-unreachable. GTK4 also surfaces the window's
        // built-in click gesture (role:group#0, a focusless operable element) and
        // the buttons' inner GtkLabel children (operable:false, never gaps); the
        // engine counts every operable-but-inaccessible element, so no_role==1
        // (the fake button alone has no role) while the two focusless operable
        // elements (window + fake button) drive keyboard_unreachable/pointer_only.
        // The marker lives in tests/golden/operability/gtk.json; CI re-captures it.
        let g = gaps_from_golden("gtk");
        // The fake button is the only operable element with no accessible role.
        assert_eq!(g.no_role, 1, "fake button alone has no GtkAccessible role");
        // Two operable elements lack focus/keyboard reachability: the fake button
        // and GTK4's window-level click gesture; the real + good buttons are clean.
        assert_eq!(g.keyboard_unreachable, 2);
        assert_eq!(g.pointer_only, 2);
        assert!(!g.focus_trap);
    }

    #[test]
    fn flutter_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process Flutter operability agent
        // (sdk/reproit_flutter/.../operability_fixture_test.dart's groundTruth()).
        // This is the VERBATIM EXPLORE:GROUNDTRUTH line `flutter test` emits for
        // the operability fixture: a real ElevatedButton (clean) and a "fake
        // button" (a bare GestureDetector(onTap:) wrapping Text). Flutter's
        // semantics DO give the gesture a synthetic button role (rolePresent:true,
        // gestureKind "tap"), so the gap is NOT no_role; the fake button is the
        // motivating finding because it is operable by pointer yet has no Focus, so
        // it is keyboard-unreachable AND not keyboard-activatable. The marker lives
        // in tests/golden/operability/flutter.json and is RE-CAPTURED by the CI
        // capture-diff job (`flutter test`); see .github/workflows/ci.yml.
        let g = gaps_from_golden("flutter");
        // Flutter exposes the gesture's button role, so there is no no_role gap.
        assert_eq!(g.no_role, 0, "flutter gives the gesture a button role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button has no Focus -> not in the tab order"
        );
        assert_eq!(
            g.pointer_only, 1,
            "fake button is pointer-only (onTap, not keyboard-activatable)"
        );
        assert!(!g.focus_trap);
    }

    #[test]
    fn merge_backfills_route_on_a_known_state() {
        // First run had no route; a later run that reports one backfills it.
        let mut map = AppMap {
            app: "t".into(),
            version: 1,
            states: BTreeMap::new(),
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        };
        merge(
            &mut map,
            &parse_run(r#"EXPLORE:STATE {"sig":"abc","labels":[],"unlabeled":0}"#),
        );
        assert!(map
            .states
            .values()
            .next()
            .unwrap()
            .signature
            .route
            .is_none());
        merge(
            &mut map,
            &parse_run(r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":[],"unlabeled":0}"#),
        );
        assert_eq!(
            map.states
                .values()
                .next()
                .unwrap()
                .signature
                .route
                .as_deref(),
            Some("/home")
        );
    }

    #[test]
    fn read_all_device_logs_unions_every_actor() {
        let dir = std::env::temp_dir().join(format!("reproit-maplogs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("drive-a.log"), "EXPLORE:STATE a-line").unwrap();
        std::fs::write(dir.join("drive-b.log"), "EXPLORE:STATE b-line").unwrap();
        // A non-device file must be ignored.
        std::fs::write(dir.join("other.log"), "ignore me").unwrap();
        let joined = read_all_device_logs(&dir).unwrap();
        assert!(joined.contains("a-line"), "device a's log is included");
        assert!(joined.contains("b-line"), "device b's log is included");
        assert!(
            !joined.contains("ignore me"),
            "non-device logs are excluded"
        );
        // Sorted by name: a before b.
        assert!(joined.find("a-line").unwrap() < joined.find("b-line").unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
