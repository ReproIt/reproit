//! Operability-state invariants: whether the user can act on the screen.

use crate::adapters::config::InvariantsCfg;
use crate::domain::invariants::finding::finding;
use crate::domain::invariants::graph::{label_set, permission_traps, screen_hint};
use crate::domain::invariants::Observations;
use serde_json::Value;

pub(super) fn evaluate_operability_state_invariants(
    obs: &Observations,
    cfg: &InvariantsCfg,
) -> Vec<Value> {
    let mut out = Vec::new();

    // no-safe-area-collision: an interactive control whose hit rect intersects a
    // device safe-area inset -- the status bar / notch / Dynamic Island (top),
    // the home indicator (bottom), or a landscape notch / rounded corner (left or
    // right) -- so the control is partly hidden under system chrome or a display
    // cutout and is hard to hit. Ground truth is the platform inset geometry
    // (Flutter MediaQuery.viewPadding / Appium safe-area insets) versus the
    // control's own hit rect, a pure structural measurement, so it re-confirms on
    // replay. Native mobile explorers emit EXPLORE:SAFEAREA per state; empty for
    // runners/states with no control in an inset, so a clean screen reports
    // nothing. The control + edge lead the message (before any parenthetical) so
    // scan_detail keeps them on truncation.
    if cfg.no_safe_area {
        for (sig, items) in &obs.obs.safe_areas {
            let Some((key, edge, by)) = items.first() else {
                continue;
            };
            let more = if items.len() > 1 {
                format!(" and {} more control(s)", items.len() - 1)
            } else {
                String::new()
            };
            out.push(finding(
                "no-safe-area-collision",
                "SAFEAREA",
                format!(
                    "state {sig} control {key} overlaps the {edge} safe-area inset by \
                     {by}px{more}: it sits under the notch/status bar/home indicator, so it is \
                     obscured or hard to tap"
                ),
                Some(sig),
            ));
        }
    }

    // no-permission-dead-end: under a runtime-permission DENIAL sweep, a screen
    // the app reached AFTER the denial is a genuine graph dead end -- a stuck
    // "please enable X" screen with no working way forward. This COMPOSES with the
    // sink predicate, then keeps only
    // the sinks that the permission-denial sweep marked as post-denial screens
    // (EXPLORE:PERMISSIONWALK), attributing the trap to the exact denied
    // permission. A marked screen that DOES have a forward exit is never flagged
    // (it is not a dead end); terminal states declared in config are exempt. The
    // permission + state lead the message (before any parenthetical) so
    // scan_detail keeps them on truncation. Gated on its own toggle so a team can
    // silence it independently.
    if cfg.no_permission_dead_end && !obs.obs.permission_screens.is_empty() {
        let sinks: std::collections::BTreeSet<String> =
            permission_traps(&obs.obs).into_iter().collect();
        for (sig, perm) in &obs.obs.permission_screens {
            if !sinks.contains(sig) {
                continue;
            }
            if cfg.terminal_states_match(sig, label_set(&obs.obs, sig)) {
                continue;
            }
            out.push(finding(
                "no-permission-dead-end",
                "PERMISSIONWALK",
                format!(
                    "denying the {perm} permission dead-ends at state {sig}: no outgoing action \
                     edge (the app strands the user on a permission screen with no way forward){}",
                    screen_hint(label_set(&obs.obs, sig))
                ),
                Some(sig),
            ));
        }
    }

    // no-broken-asset: dead critical subresources in a state -- a visibly broken
    // image, rendered tofu, or a failed/MIME-blocked same-origin stylesheet or
    // application script. These are DOM facts correlated with browser-confirmed
    // resource outcomes, without timing thresholds, so they re-confirm on replay.
    // The web runner emits EXPLORE:BROKENASSET per state; empty for healthy
    // assets, so a clean app reports nothing.
    if cfg.no_broken_asset {
        for (sig, items) in &obs.obs.broken_assets {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(key, reason, detail)| format!("{key} [{reason}] {detail}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-broken-asset",
                "BROKENASSET",
                format!(
                    "state {sig} has {} broken critical asset(s): {detail}; a visible \
                     image/encoding asset or required stylesheet/application script failed",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    // no-reflow-break: a route that breaks at 200% zoom (WCAG 1.4.10 Reflow,
    // EAA-mandatory). The web runner re-renders each newly-reached route at
    // half the viewport's CSS size (the reflow-equivalent of 200% zoom) and
    // emits EXPLORE:ZOOMREFLOW when the content then requires two-dimensional
    // scrolling (a horizontal scrollbar on a vertically-scrolling document) or
    // a previously visible tappable's hit rect collapses below 1px. Pure
    // layout measurement at a fixed zoomed viewport, so it re-confirms on
    // replay. Empty for runners/routes that reflow cleanly. Keep any remedy
    // out of parentheses: scan detail truncates at the first " (".
    if cfg.no_zoom_reflow {
        for (sig, items) in &obs.obs.zoom_reflows {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(key, kind, by)| match kind.as_str() {
                    "hscroll" => format!("{key} scrolls horizontally by {by}px"),
                    _ => format!("{key} collapses to {by}px"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-reflow-break",
                "ZOOMREFLOW",
                format!(
                    "state {sig} breaks at 200% zoom with {} reflow violation(s): {detail}; WCAG \
                     1.4.10 requires content to reflow without two-dimensional scrolling and keep \
                     controls usable",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    // no-scroll-recycle: in a scrollable list, the content at a pinned offset is
    // NOT identical after scrolling away and back -- a list-recycling /
    // virtualization bug rebound a different row to the same position. The
    // explorers that can scroll (Flutter, web, Appium) scroll the list away,
    // scroll it back, and emit EXPLORE:SCROLLROUNDTRIP when the normalized
    // content at a pinned offset changed (legitimately dynamic value-state is
    // normalized out upstream). A structural content comparison, so it
    // re-confirms on replay. Empty for stable lists / runners that cannot
    // scroll. Keep any remedy out of parentheses: scan detail truncates at the
    // first " (".
    if cfg.no_scroll_round_trip {
        for (sig, items) in &obs.obs.scroll_round_trips {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(pos, before, after)| format!("at {pos} \"{before}\" became \"{after}\""))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-scroll-recycle",
                "SCROLLROUNDTRIP",
                format!(
                    "state {sig} shows different content at {} scroll position(s) after scrolling \
                     away and back: {detail}; a list-recycling bug swapped content at a pinned \
                     offset",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    out
}
