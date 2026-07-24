//! Ordered evaluation of built-in and custom invariant families.

use super::custom::eval_custom;
use super::finding::{advisory_finding, finding};
use super::graph::{label_set, permission_traps, screen_hint};
use super::Observations;
use crate::adapters::config::InvariantsCfg;
use serde_json::{json, Value};

/// Render a jank/hang magnitude with its unit: `>= 16ms` for the millisecond
/// tier, `>= 14 keypresses` / `>= 30 pct` for non-ms tiers, so a finding never
/// implies wall-clock time for a count or percentage (the TUI's bucket is
/// ignored keystrokes, an RSS-only tier's could be janky-frame percent).
fn metric(bucket: i64, unit: &str) -> String {
    if unit == "ms" {
        format!(">= {bucket}ms")
    } else {
        format!(">= {bucket} {unit}")
    }
}

/// Evaluate the full invariant set (built-ins gated by config + any custom
/// invariants) over one run's observations. Returns all violations.
pub fn evaluate(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
    let mut out = evaluate_edge_invariants(obs, cfg);
    out.extend(evaluate_render_state_invariants(obs, cfg));
    out.extend(evaluate_operability_state_invariants(obs, cfg));
    out.extend(evaluate_transition_invariants(obs, cfg));
    out.extend(evaluate_graph_invariants(obs, cfg));
    out.extend(evaluate_lifecycle_invariants(obs, cfg));
    for custom in &cfg.custom {
        out.extend(eval_custom(obs, custom));
    }
    out
}

fn evaluate_edge_invariants(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
    let mut out = Vec::new();

    // no-exception: the existing exception oracle, now a named edge invariant.
    if cfg.no_exception {
        for ex in &obs.exceptions {
            let kind = ex
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("EXCEPTION");
            let msg = ex.get("message").and_then(Value::as_str).unwrap_or("");
            // Preserve the original record (frames!) but tag it as the named
            // invariant so the report can attribute it.
            let mut rec = ex.clone();
            rec["invariant"] = json!("no-exception");
            if rec
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .is_empty()
            {
                rec["message"] = json!(format!("uncaught app exception: {kind}"));
            }
            let _ = msg;
            out.push(rec);
        }
    }

    // DOM node replacement is implementation churn, not evidence of a presented
    // visual defect. EXPLORE:RERENDER remains parseable as diagnostic telemetry,
    // but must never become a flicker finding. A framework can replace unchanged
    // chrome between two presented frames without the user seeing it.
    if cfg.rerender_flicker {
        // paint-flicker: the gated Tier-2 pixel signal (EXPLORE:FLICKER). A frame
        // that diverged from both endpoints mid-transition then settled. Same
        // flicker oracle/toggle; timing-sensitive, so it is only emitted when the
        // runner ran with REPROIT_FLICKER_PIXELS and re-confirmed across repeats.
        for ((from, action), peak) in &obs.obs.paint_flickers {
            // ADVISORY: this is a raw-PIXEL signal (a screencast frame diff), which
            // is not deterministic across GPUs/fonts/DPR, so it is reported but
            // never becomes a verdict-bearing repro.
            out.push(advisory_finding(
                "paint-flicker",
                "FLICKER",
                format!(
                    "transition {from} --{action}--> showed a transient frame {:.0}% different \
                     from both the start and the settled result (advisory: a raw-pixel flash \
                     signal, not deterministic across machines)",
                    peak * 100.0
                ),
                Some(from),
            ));
        }
    }

    out
}

fn evaluate_render_state_invariants(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
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

fn evaluate_operability_state_invariants(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
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

fn evaluate_transition_invariants(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
    let mut out = Vec::new();

    // no-jank: a main-thread JANK stall on a transition. Two independent sources,
    // both gated by this one toggle so `--only jank` / `--no jank` cover them:
    //   - SIM tier: per-state frame-timing jank over budget (jank_by_sig). Headless
    //     has a fake clock, so jank_by_sig is empty there.
    //   - WEB tier: a Long Tasks stall on a transition (obs.janks). Deterministic
    //     (keyed off the browser's longtask trace, bucketed so timing jitter can't
    //     flip the verdict), so it re-confirms on replay. Empty unless an action
    //     blocked the main thread past the jank floor.
    if cfg.no_jank {
        if obs.sim {
            for (sig, jank) in &obs.jank_by_sig {
                if *jank > cfg.jank_pct_max {
                    out.push(finding(
                        "no-jank",
                        "PERF",
                        format!(
                            "state {sig} jank {jank:.1}% exceeds budget {:.0}% (sim tier)",
                            cfg.jank_pct_max
                        ),
                        Some(sig),
                    ));
                }
            }
        }
        for ((from, action), (bucket, unit)) in &obs.obs.janks {
            // "layouts" is the deterministic, machine-invariant signal (forced
            // synchronous reflow count), so it gets a thrash-specific message that
            // states the reproducibility; every other unit is the wall-clock stall.
            let message = if unit == "layouts" {
                format!(
                    "transition {from} --{action}--> forced {} on the main thread (layout \
                     thrashing from repeated forced synchronous reflow; the count is \
                     machine-invariant, so this jank reproduces on any runner)",
                    metric(*bucket, unit)
                )
            } else {
                format!(
                    "transition {from} --{action}--> blocked the main thread {} (a dropped-frame \
                     jank stall; the handler ran a long synchronous task)",
                    metric(*bucket, unit)
                )
            };
            out.push(finding("no-jank", "PERF", message, Some(from)));
        }
    }

    // no-hang: a main-thread FREEZE on a transition (the app stopped making
    // progress). The web runner's watchdog reports an action whose synchronous
    // handler blocked the main thread past the hang floor (a far higher bucket
    // than jank), from the same Long Tasks trace, so it is deterministic and
    // re-confirms on replay. Empty unless an action froze the UI.
    if cfg.no_hang {
        for ((from, action), (bucket, unit)) in &obs.obs.hangs {
            out.push(finding(
                "no-hang",
                "HANG",
                format!(
                    "transition {from} --{action}--> froze the main thread {} with no progress (a \
                     synchronous hang: the app stopped responding for the duration)",
                    metric(*bucket, unit)
                ),
                Some(from),
            ));
        }
    }

    // no-choice-anomaly: a multi-choice component (language tabs, a radio group)
    // where every option has a similar effect EXCEPT one outlier that shifts the
    // global layout. Differential, not an absolute threshold: the web runner
    // exhaustively selects each choice, measures its effect on the page OUTSIDE
    // the component, and reports only the choice whose effect deviates far from
    // its siblings. Empty unless a component has an odd-one-out option.
    if cfg.no_choice_anomaly {
        for (from, role, outlier, _sel, mag) in &obs.obs.choice_bugs {
            out.push(finding(
                "no-choice-anomaly",
                "CHOICE",
                format!(
                    "the {role} choice '{outlier}' behaves differently from its siblings: \
                     selecting it shifts the global page layout by {mag}px while the other \
                     choices do not (an odd-one-out option)"
                ),
                Some(from),
            ));
        }
    }

    // no-broken-route: the app links to a URL whose document responded with a
    // GENUINELY-GONE status -- 404 or 410 ONLY, GET-confirmed by the runner. The
    // runner excludes 401/403 (auth gates), 429 (rate limit), 3xx (redirect),
    // 405/501 (method semantics -- a CDN answering HEAD 501 while GET is 200 was a
    // false positive), and 5xx (a transient server error is not a broken LINK).
    // Keyed off the HTTP status, so it is structural and locale-invariant. Empty
    // unless a visited route came back genuinely gone.
    if cfg.no_broken_route {
        for (sig, route, status, from) in &obs.obs.broken_routes {
            // Two attribution shapes, and the copy must make the difference
            // unmistakable (a "route X is dead" line under screen Y reads as
            // "Y is broken", and Y loads fine):
            //  - link check (from == sig): the finding sits on the SOURCE screen that
            //    carries the dead <a>; the screen itself is healthy.
            //  - visited (otherwise): the walk actually landed on the dead document, so the
            //    finding's own screen IS the dead route.
            // /cdn-cgi/l/email-protection is a Cloudflare email-obfuscation URL.
            // It needs the decoder script and is not a usable route by itself,
            // so a 404 is actionable but has a more specific remedy.
            // NO parentheses in these messages: scan_detail (modes/fuzz.rs)
            // truncates at the first " (" and would eat the remedy.
            let cf_email = route.starts_with("/cdn-cgi/l/email-protection");
            let message = if from.as_deref() == Some(sig.as_str()) {
                if cf_email {
                    format!(
                        "dead link on this screen: {route} returns HTTP {status}; it is a \
                         Cloudflare email-protection URL whose decoder did not handle the click; \
                         restore the decoder or replace the link with a plain mailto:"
                    )
                } else {
                    format!(
                        "dead link on this screen: following the link to {route} returns HTTP \
                         {status}; this screen itself loads fine, the link target is what is \
                         broken"
                    )
                }
            } else if cf_email {
                format!(
                    "navigated to {route} and got HTTP {status}; it is a Cloudflare \
                     email-protection URL whose decoder did not handle the click; restore the \
                     decoder or replace the link with a plain mailto:"
                )
            } else {
                format!("this screen's document {route} returned HTTP {status} [dead route]")
            };
            out.push(finding(
                "no-broken-route",
                "BROKENROUTE",
                message,
                Some(sig),
            ));
        }
    }

    // no-leak: a leaked-resource / teardown signal. Headless surfaces a
    // teardown exception block (already in `exceptions` -> no-exception); this
    // adds a dedicated finding when a non-exception memory signal is present
    // (e.g. soak memory growth under --sim), so it is not double-counted.
    if cfg.no_leak {
        if let Some(detail) = &obs.leak_signal {
            out.push(finding(
                "no-leak",
                "LEAK",
                format!("resource leak signal: {detail}"),
                None,
            ));
        }
    }

    out
}

fn evaluate_graph_invariants(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
    let mut out = Vec::new();

    // The general dead-end oracle (a non-terminal sink with no outgoing edge)
    // was removed: it depended on the crawler having exhausted every action on a
    // screen, so a budget-limited crawl produced false dead ends. The FP-safe,
    // environment-anchored variant survives as permission-walk (a dead end
    // reached specifically after a permission denial). The `dead_ends()`
    // predicate is retained only for that permission-denial oracle.

    // no-duplicate-submit: a submit-like control that fired the SAME first-party
    // non-GET request twice when tapped twice in rapid succession -- the handler
    // has no double-activation guard, so an impatient double click places the
    // order/payment/post twice. From the web runner's double-dispatch probe,
    // which is OPT-IN per run (REPROIT_DUPSUBMIT=1: double-firing real submits
    // changes exploration semantics, so a normal walk never does it). The probe
    // skips a control whose first click navigated (the navigation legitimately
    // swallows the second click), so a record here is a real double fire.
    if cfg.no_duplicate_submit {
        for ((from, action), (method, url, count)) in &obs.obs.duplicate_submits {
            out.push(finding(
                "no-duplicate-submit",
                "DUPSUBMIT",
                format!(
                    "state {from} double-submits: tapping {action} twice within 150ms fired \
                     {method} {url} {count} times; the handler has no guard against rapid double \
                     activation, so a double click submits twice"
                ),
                None,
            ));
        }
    }

    // no-focus-loss: a non-navigating tap after which document.activeElement is
    // back on <body> while the tapped control still exists -- the interaction's
    // re-render dropped keyboard focus, so a keyboard user loses their place.
    // The runner suppresses the false positives upstream (a dialog/popover
    // opening or closing, a control removed by its own re-render, and
    // link/anchor taps never fire), so a record here is a real focus drop.
    // Deterministic: pure focus/DOM facts, no pixels or timing.
    if cfg.no_focus_loss {
        for (from, action) in &obs.obs.focus_losses {
            out.push(finding(
                "no-focus-loss",
                "FOCUSLOSS",
                format!(
                    "state {from} drops keyboard focus: {action} leaves focus on document.body \
                     although the control still exists; a keyboard user loses their place after \
                     the interaction"
                ),
                None,
            ));
        }
    }

    // no-occluded-control: an interactive element presented as usable but whose
    // center is covered by a FOREIGN element (an invisible leftover backdrop, a
    // z-index accident, a sticky header) -- a click there hits the overlay, not
    // the control. Pure hit-test (elementFromPoint), deterministic given a fixed
    // viewport, so it re-confirms on replay. The web runner emits EXPLORE:OCCLUSION
    // per state (background behind an open modal is excluded upstream, so a legit
    // modal is not a false positive).
    if cfg.no_occluded_control {
        // Dedup by the occluded control-SET, not the state signature. A stateful
        // single-DOM app (a demo mockup cycling language/steps, an SPA route)
        // re-presents the same buried controls under many signatures; reporting
        // one finding per signature is noise for one underlying defect. Collapse
        // identical control-sets to the FIRST signature that showed them so the
        // finding id stays stable, and emit once per distinct set.
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (sig, items) in &obs.obs.occlusions {
            let mut set: Vec<String> = items.iter().map(|(t, c)| format!("{t}\u{1f}{c}")).collect();
            set.sort();
            set.dedup();
            let key = set.join("\u{1e}");
            if !seen.insert(key) {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(t, c)| format!("{t} under {c}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-occluded-control",
                "OCCLUSION",
                format!(
                    "state {sig} has {} occluded control(s): {detail} (a foreign element covers \
                     the control's center, so a click hits the overlay instead of the control)",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    // no-detached-indicator: the application explicitly declared an indicator,
    // its owner, and their container. The web adapter emits only after the
    // relationship resolved uniquely, all nodes were visible and stable, and
    // the same violation survived two settled samples. Missing or ambiguous
    // declarations are ABSTAIN upstream and never become findings.
    if cfg.no_detached_indicator {
        for (sig, items) in &obs.obs.relations {
            for item in items {
                if item.kind != "indicator-anchor" {
                    continue;
                }
                let gap = item.gap_centipx as f64 / 100.0;
                let detail = if item.violation == "escaped-container" {
                    format!(
                        "{} escaped its declared container {}",
                        item.dependent_key, item.container_key
                    )
                } else {
                    format!(
                        "{} is {gap:.2}px from its owner {}, beyond the declared {}px maximum",
                        item.dependent_key, item.owner_key, item.max_gap
                    )
                };
                let mut found = finding(
                    "no-detached-indicator",
                    "DETACHEDINDICATOR",
                    format!(
                        "state {sig} has a detached indicator: {detail}; explicit relationship {} \
                         -> {}",
                        item.dependent_key, item.owner_key
                    ),
                    Some(sig),
                );
                // Stable structural identity for dedupe, shrink, evidence, and
                // future per-relation replay selection. Raw geometry remains
                // diagnostic and is deliberately excluded.
                found["relationship"] = json!({
                    "kind": item.kind,
                    "dependentKey": item.dependent_key,
                    "ownerKey": item.owner_key,
                    "containerKey": item.container_key,
                    "violation": item.violation,
                });
                found["selector"] = json!(item.dependent_key);
                out.push(found);
            }
        }
    }

    // no-accessibility-state-mismatch: compare the native control's live DOM
    // property with Chromium's computed accessibility state for that exact
    // backend DOM node. The runner has already required two identical settled
    // samples. Only explicit VIOLATION checks become findings; SATISFIED and
    // ABSTAIN remain replay evidence.
    if cfg.no_accessibility_state_mismatch {
        for (sig, checks) in &obs.obs.accessibility_state_checks {
            for check in checks.iter().filter(|check| check.outcome == "VIOLATION") {
                let actual = check.actual.as_deref().unwrap_or("unavailable");
                let mut found = finding(
                    "no-accessibility-state-mismatch",
                    "A11YSTATE",
                    format!(
                        "state {sig} exposes {}={} for {} but the native control state is {}; \
                         the same semantic-state contradiction held in two settled samples",
                        check.property, actual, check.identity, check.expected
                    ),
                    Some(sig),
                );
                found["selector"] = json!(check.identity);
                found["fingerprint"] = json!(check.fingerprint);
                found["accessibilityState"] = json!({
                    "identity": check.identity,
                    "property": check.property,
                    "expected": check.expected,
                    "actual": actual,
                    "reason": check.reason,
                    "fingerprint": check.fingerprint,
                });
                out.push(found);
            }
        }
    }

    // no-insecure-markup: client-side security-hygiene smells (a cross-origin
    // target=_blank without rel=noopener -> reverse tabnabbing; an HTTPS page with
    // an http: form action or http: subresource -> mixed content). Pure DOM/URL
    // predicates from the web runner (EXPLORE:SECURITY), deterministic and
    // false-positive-free. Gated with the broken-route web-hygiene toggle.
    if cfg.no_broken_route {
        for (sig, items) in &obs.obs.security {
            let detail = items
                .iter()
                .take(3)
                .map(|(kind, target)| format!("{kind}: {target}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-insecure-markup",
                "SECURITY",
                format!(
                    "state {sig} has {} client-side security issue(s): {detail} (a cross-origin \
                     target=_blank without rel=noopener, or an HTTPS page loading http content)",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    out
}

fn evaluate_lifecycle_invariants(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
    let mut out = Vec::new();

    // no-rotation-loss: a lifecycle-metamorphic violation across a device
    // rotation. The explorer rotated the surface (portrait <-> landscape /
    // split-screen), reflowed, then rotated BACK to the original orientation,
    // and the screen did not rebuild the same structure -- content or state was
    // lost across the rotation lifecycle and never came back. From
    // EXPLORE:ROTATION; round-trip identity (same orientation in and out) with
    // value-state excluded, so it is deterministic and false-positive-free.
    // Essentials before any parenthesis: scan detail truncates at the first " (".
    if cfg.no_rotation_loss {
        for (sig, (expected, got)) in &obs.obs.rotation_losses {
            out.push(finding(
                "no-rotation-loss",
                "ROTATION",
                format!(
                    "state {sig} does not survive rotation: after rotating the screen and \
                     rotating back, the app rebuilt a different structure than before, so content \
                     or state was lost across the orientation change (expected structure \
                     {expected}, got {got})"
                ),
                Some(sig),
            ));
        }
    }

    // no-background-loss: a lifecycle-metamorphic violation across the app
    // background -> foreground cycle. The explorer sent the app to the
    // background (paused/hidden) then restored it (resumed/visible), and it came
    // back to a DIFFERENT screen or lost its state. From EXPLORE:BGRESTORE; no
    // size change and value-state excluded, so it is deterministic and
    // false-positive-free. Essentials before any parenthesis (scan detail
    // truncates at the first " (").
    if cfg.no_background_loss {
        for (sig, (expected, got)) in &obs.obs.background_losses {
            out.push(finding(
                "no-background-loss",
                "BGRESTORE",
                format!(
                    "state {sig} does not survive backgrounding: sending the app to the \
                     background and restoring it dropped the user on a different screen or lost \
                     state, instead of returning to the same screen (expected structure \
                     {expected}, got {got})"
                ),
                Some(sig),
            ));
        }
    }

    // no-stuck-keyboard: the soft keyboard must never be visible without a
    // focused text input. From EXPLORE:STUCKKEYBOARD, emitted only on violation
    // by the native mobile explorers off platform ground truth (IME visibility
    // + focus tree), so it is deterministic and false-positive-free. Keep any
    // remedy out of parentheses: scan detail truncates at the first " (".
    if cfg.no_stuck_keyboard {
        for sig in &obs.obs.stuck_keyboards {
            out.push(finding(
                "no-stuck-keyboard",
                "STUCKKEYBOARD",
                format!(
                    "state {sig} keeps the soft keyboard open with no text field focused: the \
                     keyboard covers content the user never asked for and cannot dismiss by \
                     leaving the field"
                ),
                Some(sig),
            ));
        }
    }

    // no-wakelock-leak: a wakelock (or a window FLAG_KEEP_SCREEN_ON) acquired on a
    // screen must be released when the user leaves it. From EXPLORE:WAKELOCK,
    // emitted only on a violation by the Android/Appium explorer off platform
    // ground truth (dumpsys power app-owned wake locks + the focused window's
    // keep-screen-on flag, compared before vs after leaving the screen). The
    // runner excludes app-global/baseline and released locks and attributes each
    // leak to its origin screen once, so it is deterministic and
    // false-positive-free. Keep essentials before any parenthesis: scan detail
    // truncates at the first " (".
    if cfg.no_wakelock_leak {
        for (sig, items) in &obs.obs.wakelock_leaks {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(tag, kind)| format!("{tag} [{kind}]"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-wakelock-leak",
                "WAKELOCK",
                format!(
                    "state {sig} keeps a wakelock held after you navigate away: {detail} still \
                     held off the screen that needed it, draining the battery by keeping the \
                     CPU/screen awake"
                ),
                Some(sig),
            ));
        }
    }

    out
}
