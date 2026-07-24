//! Render-state invariants: what the settled screen shows.

use crate::adapters::config::InvariantsCfg;
use crate::domain::invariants::finding::finding;
use crate::domain::invariants::Observations;
use serde_json::{json, Value};

pub(super) fn evaluate_render_state_invariants(
    obs: &Observations,
    cfg: &InvariantsCfg,
) -> Vec<Value> {
    let mut out = Vec::new();

    // no-broken-render: every observed state must render NO broken-content
    // artifact (a label coerced from an object/undefined/null/NaN, or an
    // unrendered template). The web runner detects this from the DOM/labels and
    // emits EXPLORE:CONTENTBUG per state. This is the built-in version of the
    // user-declarable labelsAbsent custom invariant, so a render bug is caught
    // WITHOUT the developer first declaring it. Deterministic: pure DOM scan, no
    // pixels or timing, so it re-confirms on replay. Empty for runners/states
    // that render no broken content, so a clean app reports nothing.
    if cfg.no_broken_render {
        for (sig, items) in &obs.obs.content_bugs {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(key, reason, text)| format!("{key} ({reason}): {text:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-broken-render",
                "CONTENTBUG",
                format!(
                    "state {sig} renders {} broken-content label(s): {detail} (a \
                     stringify/template bug leaked a raw artifact like [object \
                     Object]/undefined/null/NaN/{{...}} to the screen)",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    // Zero-contrast invisible content: glyph runs whose resolved foreground
    // equals their resolved background in a selection/emphasis context, so
    // required content renders invisible. Colorimetric equality on the
    // attributes the app emitted; deterministic, re-confirms on replay.
    if cfg.no_zero_contrast {
        for (sig, items) in &obs.obs.zero_contrast {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(key, text, color)| format!("{key}: {text:?} in {color}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-zero-contrast",
                "ZEROCONTRAST",
                format!(
                    "state {sig} renders {} invisible zero-contrast run(s): {detail} \
                     (foreground exactly equals the styled background, so selected or \
                     emphasized content is unreadable)",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    // Dead input: a runner-injected input provably vanished where an effect
    // is structurally required. The runner observed the whole event pipeline
    // (no event, no delta, no preventDefault), so the probe is an equality
    // check and re-confirms on replay.
    if cfg.no_dead_input {
        for (sig, items) in &obs.obs.dead_inputs {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(key, input, context)| format!("{key} ({context}): {input}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-dead-input",
                "DEADINPUT",
                format!(
                    "state {sig} swallows {} injected input(s): {detail} (the input \
                     produced no event, no value or scroll delta, and no handler \
                     claimed it, so the input pipeline is broken)",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    if cfg.no_overflow {
        for (sig, checks) in &obs.obs.overflow_checks {
            for check in checks.iter().filter(|check| {
                check.outcome == crate::domain::overflow::OverflowOutcome::Violation
            }) {
                let spill_x = check.spill_x_centipx as f64 / 100.0;
                let spill_y = check.spill_y_centipx as f64 / 100.0;
                let mut found = finding(
                    "no-layout-overflow",
                    "OVERFLOW",
                    format!(
                        "state {sig} renders {} outside its declared container {} by \
                         {spill_x:.2}px horizontally and {spill_y:.2}px vertically",
                        check.subject_key, check.container_key
                    ),
                    Some(sig),
                );
                found["selector"] = json!(check.subject_key);
                found["fingerprint"] = json!(check.fingerprint);
                found["overflow"] = json!({
                    "subjectKey": check.subject_key,
                    "containerKey": check.container_key,
                    "reason": check.reason,
                    "spillX": spill_x,
                    "spillY": spill_y,
                });
                out.push(found);
            }
        }
    }

    // no-blank-screen: an empty state already corroborated by independent
    // authority at ingestion (for example, a first-party exception on the same
    // URL). Structural visual emptiness never reaches this collection.
    if cfg.no_blank_screen {
        for (sig, items) in &obs.obs.blank_screens {
            let Some((_key, w, h)) = items.first() else {
                continue;
            };
            out.push(finding(
                "no-blank-screen",
                "BLANKSCREEN",
                format!(
                    "state {sig} renders a blank screen: zero visible text nodes and zero \
                     tappable controls in a {w}x{h} viewport; the white-screen-of-death, nothing \
                     mounted for this route"
                ),
                Some(sig),
            ));
        }
    }

    // app-invariant: the app's own predicates, registered via the SDK
    // (`ReproIt.invariant("id", fn)`) and evaluated by the SDK on each
    // state-settle under the fuzzer. Any violation the SDK reported
    // (`EXPLORE:INVARIANT`) is the app's ground truth, so it is
    // false-positive-free. The invariant id leads the message (before any
    // parenthetical) so scan_detail keeps it on truncation.
    if cfg.no_invariant_violation {
        for (sig, items) in &obs.obs.app_invariants {
            for (id, message) in items {
                let msg = if message.is_empty() {
                    format!("app invariant \"{id}\" failed in state {sig}")
                } else {
                    format!("app invariant \"{id}\" failed in state {sig}: {message}")
                };
                out.push(finding("app-invariant", "INVARIANT", msg, Some(sig)));
            }
        }
    }

    // no-listener-leak: event listeners and/or DOM nodes that grow MONOTONICALLY
    // across repeated visits to the same route (enter route, leave, re-enter).
    // The web/electron runners' opt-in revisit probe (REPROIT_LISTENERLEAK=1)
    // drives N re-entries with the addEventListener/removeEventListener counters
    // it wrapped at page init plus getElementsByTagName('*').length, and only
    // emits a metric that climbed on EVERY revisit above a slope floor -- so a
    // stable app (flat after a one-time warmup) never reaches here. A monotonic
    // unbounded climb is a real leak: the route mounts listeners/nodes it never
    // releases on unmount. Sequence/repeat-dependent (needs the revisit loop), so
    // it is a fuzz/soak signal, not a state-present one. The route + climb lead
    // the message before any parenthetical (scan_detail truncates at the first
    // " (").
    if cfg.no_listener_leak {
        for (route, (visits, items)) in &obs.obs.listener_leaks {
            for (kind, first, last) in items {
                let unit = if kind == "nodes" {
                    "DOM nodes"
                } else {
                    "event listeners"
                };
                out.push(finding(
                    "no-listener-leak",
                    "LISTENERLEAK",
                    format!(
                        "route {route} leaks {unit}: {first} climbing to {last} across {visits} \
                         revisits; each visit adds {unit} that unmount never releases, an \
                         unbounded growth that ends in an out-of-memory crash"
                    ),
                    Some(route),
                ));
            }
        }
    }

    out
}
