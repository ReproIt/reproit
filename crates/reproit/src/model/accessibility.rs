//! The accessibility view: reproit's "two graphs told apart" diff, narrowed to
//! the accessibility axis. Graph 1 (ground-truth operable, what a pointer user
//! can actually do) minus graph 2 (the accessibility/keyboard-operable subset)
//! = the operability gaps already recorded per screen in the app map. This
//! module reads those gaps and renders them as a focused, GROUNDED report (WCAG
//! 2.1.1 / 4.1.2 + focus traps): for humans (`reproit map accessibility`) and
//! for agents (the `reproit_accessibility` MCP tool, via `--json`).
//!
//! No analysis happens here. The diff is computed deterministically by the
//! engine when the map is built (`map::gaps_from_groundtruth`; see
//! docs/operability-graph.md). This is a pure view that CLOSES THE LOOP for a
//! fixer: each gap carries the selector that failed and which dimension(s), plus
//! a static source location (file:line, via `attribute`) to fix it and the
//! action path to reach its screen. find -> locate -> fix -> reproit_check.

use crate::appmap::{Action, AppMap};
use crate::attribute;
use crate::Ctx;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

/// Render the accessibility diff. `root` (when known) is the project root we
/// attribute selectors into for file:line; `state` filters to one screen
/// (signature id or human name); `kind` filters to one dimension (`pointer_only`
/// | `keyboard_unreachable` | `no_role` | `focus_trap`).
pub(crate) fn report(
    map: &AppMap,
    root: Option<&Path>,
    state: Option<&str>,
    kind: Option<&str>,
    ctx: &Ctx,
) {
    let want_kind = |k: &str| kind.is_none_or(|f| f == k);
    // focus_trap is a screen-level flag, not a per-element item.
    let include_focus_trap = kind.is_none_or(|f| f == "focus_trap");

    let mut screens: Vec<serde_json::Value> = Vec::new();
    let (mut t_po, mut t_ku, mut t_nr, mut t_ft) = (0u32, 0u32, 0u32, 0u32);

    for (sig, st) in &map.states {
        if let Some(want) = state {
            if sig != want && st.description != want {
                continue;
            }
        }
        let g = &st.operability_gaps;
        // Per-element detail, filtered to the requested kind (an element can fail
        // more than one dimension, so we keep only the matching ones), and each
        // attributed back to source so the agent knows where to fix it.
        let items: Vec<serde_json::Value> = g
            .items
            .iter()
            .filter_map(|it| {
                let kinds: Vec<&String> = it.kinds.iter().filter(|k| want_kind(k)).collect();
                if kinds.is_empty() {
                    return None;
                }
                let source = root
                    .map(|r| attribute_json(r, &it.selector))
                    .unwrap_or_default();
                Some(json!({ "selector": it.selector, "kinds": kinds, "source": source }))
            })
            .collect();
        let focus_trap = g.focus_trap && include_focus_trap;
        if items.is_empty() && !focus_trap {
            continue;
        }
        t_po += g.pointer_only;
        t_ku += g.keyboard_unreachable;
        t_nr += g.no_role;
        if g.focus_trap {
            t_ft += 1;
        }
        // How to reach this screen: the route (if the app exposes one) plus a
        // best-effort action path over the map's transitions. Either may be
        // absent on a sparse map; we never fabricate one.
        let path = path_to(map, sig);
        screens.push(json!({
            "state": sig,
            "name": st.description,
            "route": st.signature.route,
            "repro_path": path,
            "focus_trap": focus_trap,
            "pointer_only": g.pointer_only,
            "keyboard_unreachable": g.keyboard_unreachable,
            "no_role": g.no_role,
            "items": items,
        }));
    }

    if ctx.json {
        ctx.emit(&json!({
            "command": "map accessibility",
            "app": map.app,
            "summary": {
                "screens_with_gaps": screens.len(),
                "pointer_only": t_po,
                "keyboard_unreachable": t_ku,
                "no_role": t_nr,
                "focus_traps": t_ft,
            },
            "screens": screens,
        }));
        return;
    }

    // Human view.
    if screens.is_empty() {
        ctx.say("accessibility: no operability gaps recorded (graph 1 == graph 2).");
        ctx.say("(build the map with the a11y oracle first: `reproit map`.)");
        return;
    }
    ctx.say(format!(
        "accessibility diff for {} ({} screen(s) with gaps)",
        map.app,
        screens.len()
    ));
    for s in &screens {
        let name = s["name"].as_str().unwrap_or("");
        let sig = s["state"].as_str().unwrap_or("");
        let head = if name.is_empty() { sig } else { name };
        ctx.say(format!("\n  {head} [{sig}]"));
        if let Some(route) = s["route"].as_str() {
            ctx.say(format!("    route: {route}"));
        }
        if let Some(arr) = s["repro_path"].as_array() {
            if !arr.is_empty() {
                let steps: Vec<&str> = arr.iter().filter_map(|a| a.as_str()).collect();
                ctx.say(format!("    reach: {}", steps.join("  ->  ")));
            }
        }
        if s["focus_trap"].as_bool().unwrap_or(false) {
            ctx.say("    focus_trap: a keyboard focus trap was observed on this screen");
        }
        if let Some(arr) = s["items"].as_array() {
            for it in arr {
                let sel = it["selector"].as_str().unwrap_or("");
                let kinds: Vec<&str> = it["kinds"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                ctx.say(format!("    {sel}  ->  {}", kinds.join(", ")));
                if let Some(loc) = it["source"].as_array().and_then(|a| a.first()) {
                    let f = loc["file"].as_str().unwrap_or("");
                    let l = loc["line"].as_u64().unwrap_or(0);
                    if !f.is_empty() {
                        ctx.say(format!("        at {f}:{l}"));
                    }
                }
            }
        }
    }
    ctx.say(format!(
        "\n  totals: pointer_only={t_po} keyboard_unreachable={t_ku} no_role={t_nr} focus_traps={t_ft}"
    ));
}

/// Attribute one selector to source, returned as ranked JSON locations (best
/// first, capped). Best-effort: an empty list means we could not place it.
fn attribute_json(root: &Path, selector: &str) -> Vec<serde_json::Value> {
    attribute::attribute(root, selector)
        .into_iter()
        .take(3)
        .map(|loc| json!({ "file": loc.file, "line": loc.line, "snippet": loc.snippet }))
        .collect()
}

/// Shortest action path from the map's entry to `target`, over the recorded
/// transitions. Entry = a state that is never a transition target (else the
/// first state). Returns an empty vec when target is the entry or unreachable;
/// we never invent a path. Action descriptors use reproit's selector grammar
/// (`tap:<finder>`, etc.) so they read like a repro.
fn path_to(map: &AppMap, target: &str) -> Vec<String> {
    let targets: BTreeSet<&str> = map.transitions.iter().map(|t| t.to.as_str()).collect();
    let entry = map
        .states
        .keys()
        .find(|k| !targets.contains(k.as_str()))
        .or_else(|| map.states.keys().next());
    let Some(entry) = entry.map(String::as_str) else {
        return vec![];
    };
    if entry == target {
        return vec![];
    }
    let mut seen: BTreeSet<&str> = BTreeSet::from([entry]);
    let mut prev: BTreeMap<&str, (&str, String)> = BTreeMap::new();
    let mut q: VecDeque<&str> = VecDeque::from([entry]);
    while let Some(cur) = q.pop_front() {
        for t in map.transitions.iter().filter(|t| t.from == cur) {
            let to = t.to.as_str();
            if seen.insert(to) {
                prev.insert(to, (cur, action_desc(&t.action)));
                if to == target {
                    let mut acts = Vec::new();
                    let mut node = target;
                    while node != entry {
                        let (from, act) = &prev[node];
                        acts.push(act.clone());
                        node = from;
                    }
                    acts.reverse();
                    return acts;
                }
                q.push_back(to);
            }
        }
    }
    vec![]
}

fn action_desc(a: &Action) -> String {
    match a {
        Action::Tap { finder } => format!("tap:{finder}"),
        Action::Type { finder, .. } => format!("type:{finder}"),
        Action::Scroll { finder, dy } => format!("scroll:{finder}:{dy}"),
        Action::Back => "back".into(),
        Action::System { event } => event.clone(),
    }
}
