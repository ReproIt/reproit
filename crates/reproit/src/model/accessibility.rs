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
//! docs/operability-graph.md). This is a pure view over the stored result, so a
//! claim it makes is always reproducible: every gap carries the selector that
//! failed and which dimension(s), addressable by reproit's selector grammar.

use crate::appmap::AppMap;
use crate::Ctx;
use serde_json::json;

/// Render the accessibility diff. `state` filters to one screen (matched against
/// its signature id or its human name); `kind` filters to one gap dimension
/// (`pointer_only` | `keyboard_unreachable` | `no_role` | `focus_trap`).
pub(crate) fn report(map: &AppMap, state: Option<&str>, kind: Option<&str>, ctx: &Ctx) {
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
        // Per-element detail, filtered to the requested kind (an element can
        // fail more than one dimension, so we keep only the matching ones).
        let items: Vec<serde_json::Value> = g
            .items
            .iter()
            .filter_map(|it| {
                let kinds: Vec<&String> = it.kinds.iter().filter(|k| want_kind(k)).collect();
                if kinds.is_empty() {
                    None
                } else {
                    Some(json!({ "selector": it.selector, "kinds": kinds }))
                }
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
        screens.push(json!({
            "state": sig,
            "name": st.description,
            "route": st.signature.route,
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
            }
        }
    }
    ctx.say(format!(
        "\n  totals: pointer_only={t_po} keyboard_unreachable={t_ku} no_role={t_nr} focus_traps={t_ft}"
    ));
}
