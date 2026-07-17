//! The accessibility view: reproit's "two graphs told apart" diff, narrowed to
//! the accessibility axis. Graph 1 (ground-truth operable, what a pointer user
//! can actually do) minus graph 2 (the accessibility/keyboard-operable subset)
//! = the operability gaps already recorded per screen in the app map. This
//! module reads those gaps and renders them as a focused, GROUNDED report (WCAG
//! 2.1.1 / 4.1.2 + focus traps): for humans (`reproit debug map accessibility`)
//! and for agents (the `reproit_accessibility` MCP tool, via `--json`).
//!
//! No analysis happens here. The diff is computed deterministically by the
//! engine when the map is built (`map::gaps_from_groundtruth`; see
//! docs/operability-graph.md). This is a pure view that CLOSES THE LOOP for a
//! fixer: each gap carries the selector that failed and which dimension(s),
//! plus a static source location (file:line, via `attribute`) to fix it and the
//! action path to reach its screen. find -> locate -> fix -> reproit_check.

use crate::cli::context::Ctx;
use crate::model::appmap::{Action, AppMap};
use crate::model::attribute;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

/// Render the accessibility diff. `root` (when known) is the project root we
/// attribute selectors into for file:line; `state` filters to one screen
/// (signature id or human name); `kind` filters to one dimension
/// (`pointer_only` | `keyboard_unreachable` | `no_role` | `focus_trap`).
pub(crate) fn report(
    map: &AppMap,
    root: Option<&Path>,
    state: Option<&str>,
    kind: Option<&str>,
    markdown: bool,
    ctx: &Ctx,
) {
    let want_kind = |k: &str| kind.is_none_or(|f| f == k);
    // focus_trap is a screen-level flag, not a per-element item.
    let include_focus_trap = kind.is_none_or(|f| f == "focus_trap");

    let mut screens: Vec<serde_json::Value> = Vec::new();
    // Summary totals dedup by SELECTOR across states: a gap control on a screen
    // that churns into several structural signatures (a status line appearing, a
    // value-state) must count ONCE, not once per snapshot -- else 3 planted gaps
    // read as 6. Per-screen counts below stay per-state; only the rolled-up
    // summary is deduped. focus_trap (screen-level, no selector) dedups by route.
    let mut po_sel: std::collections::BTreeSet<String> = Default::default();
    let mut ku_sel: std::collections::BTreeSet<String> = Default::default();
    let mut nr_sel: std::collections::BTreeSet<String> = Default::default();
    let mut ft_keys: std::collections::BTreeSet<String> = Default::default();

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
        for it in &g.items {
            for k in &it.kinds {
                if !want_kind(k) {
                    continue;
                }
                match k.as_str() {
                    "pointer_only" => {
                        po_sel.insert(it.selector.clone());
                    }
                    "keyboard_unreachable" => {
                        ku_sel.insert(it.selector.clone());
                    }
                    "no_role" => {
                        nr_sel.insert(it.selector.clone());
                    }
                    _ => {}
                }
            }
        }
        if focus_trap {
            ft_keys.insert(st.signature.route.clone().unwrap_or_else(|| sig.clone()));
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

    // Deduped summary totals (unique controls / focus-trapped routes).
    let (t_po, t_ku, t_nr, t_ft) = (
        po_sel.len() as u32,
        ku_sel.len() as u32,
        nr_sel.len() as u32,
        ft_keys.len() as u32,
    );

    // Markdown: an exportable, WCAG-cited report (redirect to a file to hand to
    // a reviewer). Printed unconditionally; it IS the requested output.
    if markdown {
        println!(
            "{}",
            markdown_report(&map.app, &screens, t_po, t_ku, t_nr, t_ft)
        );
        return;
    }

    if ctx.json {
        ctx.emit(&json!({
            "command": "debug map accessibility",
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
        ctx.say("(the current backend did not expose operability evidence for this app.)");
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
        "\n  totals: pointer_only={t_po} keyboard_unreachable={t_ku} no_role={t_nr} \
         focus_traps={t_ft}"
    ));
}

/// The set of gaps in a map as `(state, selector, kind)` tuples, for diffing
/// two builds. A screen-level focus trap is keyed with an empty selector.
fn gap_set(map: &AppMap) -> BTreeSet<(String, String, String)> {
    let mut s = BTreeSet::new();
    for (sig, st) in &map.states {
        for it in &st.operability_gaps.items {
            for k in &it.kinds {
                s.insert((sig.clone(), it.selector.clone(), k.clone()));
            }
        }
        if st.operability_gaps.focus_trap {
            s.insert((sig.clone(), String::new(), "focus_trap".to_string()));
        }
    }
    s
}

/// Compare a `baseline` map against the `current` one and report the gaps that
/// are NEW (a regression) and the ones resolved. Returns true if any new gap
/// was introduced, so a caller can fail CI. This is the build-vs-build gate:
/// same engine, two maps.
pub(crate) fn regression(baseline: &AppMap, current: &AppMap, ctx: &Ctx) -> bool {
    let base = gap_set(baseline);
    let cur = gap_set(current);
    let new: Vec<&(String, String, String)> = cur.difference(&base).collect();
    let resolved: Vec<&(String, String, String)> = base.difference(&cur).collect();
    let regressed = !new.is_empty();

    if ctx.json {
        let to_json = |v: &[&(String, String, String)]| {
            v.iter()
                .map(|(s, sel, k)| json!({ "state": s, "selector": sel, "kind": k }))
                .collect::<Vec<_>>()
        };
        ctx.emit(&json!({
            "command": "debug map accessibility (baseline diff)",
            "regressed": regressed,
            "new_gaps": to_json(&new),
            "resolved_gaps": to_json(&resolved),
        }));
        return regressed;
    }

    if !regressed {
        ctx.say(format!(
            "accessibility: no new gaps vs baseline ({} resolved).",
            resolved.len()
        ));
    } else {
        ctx.say(format!(
            "accessibility REGRESSION: {} new gap(s) vs baseline:",
            new.len()
        ));
        for (s, sel, k) in &new {
            let target = if sel.is_empty() {
                s.as_str()
            } else {
                sel.as_str()
            };
            ctx.say(format!("  + {k}  {target}  [{s}]"));
        }
        if !resolved.is_empty() {
            ctx.say(format!("  ({} gap(s) resolved)", resolved.len()));
        }
    }
    regressed
}

/// The WCAG success criterion a gap dimension maps to (for the report).
fn wcag_label(kind: &str) -> &'static str {
    match kind {
        "pointer_only" | "keyboard_unreachable" => "2.1.1 Keyboard",
        "no_role" => "4.1.2 Name, Role, Value",
        "focus_trap" => "2.1.2 No Keyboard Trap",
        _ => "",
    }
}

/// Render the diff as an exportable, WCAG-cited Markdown report. `screens` is
/// the per-screen JSON built in `report`.
fn markdown_report(
    app: &str,
    screens: &[serde_json::Value],
    t_po: u32,
    t_ku: u32,
    t_nr: u32,
    t_ft: u32,
) -> String {
    let app = if app.is_empty() { "this app" } else { app };
    let mut out = String::new();
    out.push_str(&format!("# Accessibility report: {app}\n\n"));
    out.push_str(
        "Controls a pointer user can operate but the keyboard or assistive tech cannot, found by \
         driving the app and comparing the two (deterministic; see docs/operability-graph.md).\n\n",
    );
    out.push_str("## Summary\n\n");
    out.push_str("| Issue | WCAG | Count |\n| --- | --- | --- |\n");
    out.push_str(&format!(
        "| Operable by mouse, not keyboard | {} | {t_po} |\n",
        wcag_label("pointer_only")
    ));
    out.push_str(&format!(
        "| Not reachable in the tab order | {} | {t_ku} |\n",
        wcag_label("keyboard_unreachable")
    ));
    out.push_str(&format!(
        "| No role/name exposed to assistive tech | {} | {t_nr} |\n",
        wcag_label("no_role")
    ));
    out.push_str(&format!(
        "| Keyboard focus trap | {} | {t_ft} |\n\n",
        wcag_label("focus_trap")
    ));
    out.push_str(&format!("Screens with gaps: {}\n", screens.len()));

    for s in screens {
        let name = s["name"].as_str().unwrap_or("");
        let sig = s["state"].as_str().unwrap_or("");
        let head = if name.is_empty() { sig } else { name };
        out.push_str(&format!("\n## {head}  `{sig}`\n\n"));
        if let Some(route) = s["route"].as_str() {
            out.push_str(&format!("Route: `{route}`\n\n"));
        }
        if s["focus_trap"].as_bool().unwrap_or(false) {
            out.push_str("A keyboard focus trap was observed on this screen (WCAG 2.1.2).\n\n");
        }
        if let Some(arr) = s["items"].as_array() {
            if !arr.is_empty() {
                out.push_str("| Control | Fails | WCAG | Source |\n| --- | --- | --- | --- |\n");
                for it in arr {
                    let sel = it["selector"].as_str().unwrap_or("");
                    let kinds: Vec<&str> = it["kinds"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|k| k.as_str()).collect())
                        .unwrap_or_default();
                    let wcag = kinds.first().map(|k| wcag_label(k)).unwrap_or("");
                    let src = it["source"]
                        .as_array()
                        .and_then(|a| a.first())
                        .map(|loc| {
                            let f = loc["file"].as_str().unwrap_or("");
                            let l = loc["line"].as_u64().unwrap_or(0);
                            if f.is_empty() {
                                String::new()
                            } else {
                                format!("`{f}:{l}`")
                            }
                        })
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "| `{sel}` | {} | {wcag} | {src} |\n",
                        kinds.join(", ")
                    ));
                }
            }
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wcag_labels_map_each_dimension() {
        assert_eq!(wcag_label("pointer_only"), "2.1.1 Keyboard");
        assert_eq!(wcag_label("keyboard_unreachable"), "2.1.1 Keyboard");
        assert_eq!(wcag_label("no_role"), "4.1.2 Name, Role, Value");
        assert_eq!(wcag_label("focus_trap"), "2.1.2 No Keyboard Trap");
    }

    #[test]
    fn gap_set_diff_finds_new_and_resolved() {
        // Two builds: baseline has a gap on row-a; current fixed row-a and grew a
        // new gap on row-b. The diff should flag exactly the new one.
        let mk = |sel: &str| -> AppMap {
            serde_json::from_value(json!({
                "app": "x", "version": 1,
                "states": { "s1": {
                    "description": "d",
                    "signature": { "screenshot_phash": null, "semantics_hash": "h", "route": null },
                    "operability_gaps": {
                        "pointer_only": 1,
                        "keyboard_unreachable": 0,
                        "no_role": 0,
                        "focus_trap": false,
                        "items": [{ "selector": sel, "kinds": ["pointer_only"] }]
                    }
                }},
                "transitions": [], "invariants": []
            }))
            .unwrap()
        };
        let base = gap_set(&mk("row-a"));
        let cur = gap_set(&mk("row-b"));
        let new: Vec<_> = cur.difference(&base).collect();
        let resolved: Vec<_> = base.difference(&cur).collect();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].1, "row-b");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].1, "row-a");
        // No diff against itself.
        assert!(gap_set(&mk("row-a")).difference(&base).next().is_none());
    }

    #[test]
    fn markdown_report_has_summary_and_grounded_rows() {
        let screens = vec![json!({
            "state": "s1",
            "name": "Refund queue",
            "route": "/refund",
            "focus_trap": true,
            "items": [
                { "selector": "key:testid:row", "kinds": ["pointer_only"],
                  "source": [{ "file": "index.html", "line": 8 }] }
            ]
        })];
        let md = markdown_report("shop", &screens, 1, 0, 0, 1);
        assert!(md.contains("# Accessibility report: shop"));
        assert!(md.contains("2.1.1 Keyboard"));
        assert!(md.contains("`key:testid:row`"));
        assert!(md.contains("`index.html:8`"));
        assert!(md.to_lowercase().contains("focus trap"));
    }
}
