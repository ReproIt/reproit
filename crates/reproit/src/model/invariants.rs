//! INVARIANTS / PROPERTIES oracle (property-based testing).
//!
//! The earlier oracles (uncaught exception, jank threshold,
//! operability semantics) were ad-hoc checks scattered through `modes/fuzz.rs`.
//! This module generalizes them into NAMED invariants evaluated over a single
//! run's observations (the parsed `EXPLORE:STATE`/`EXPLORE:EDGE` records, plus
//! the exception + perf findings that the existing oracles already produce).
//!
//! Three scopes, all pure functions over a run's observations:
//!   - State invariants (node predicates): `no-jank` (sim), plus custom
//!     label-presence/absence regex.
//!   - Edge   invariants: `no-exception` (the existing exception oracle, named).
//!   - Graph  invariants: `no-occluded-control`, plus
//!     `no-leak` (reuse the soak/memory teardown signal when present). The
//!     general graph-sink oracle was removed as crawler-budget FP-prone; its
//!     sink predicate survives only for permission-walk.
//!
//! Every violation is returned in the SAME shape `all_findings` already
//! produces (`{kind, message, frames}`, plus an `invariant` id), so the
//! downstream find -> shrink -> reproduce -> report pipeline is unchanged.
//!
//! Tier honesty: graph / label / exception invariants run on the HEADLESS tier
//! (default). `no-jank` needs real frame timing and is SIM-ONLY; `no-leak`
//! relies on a memory/teardown signal that only the live runtime surfaces, so
//! it is best-effort headless (it fires on a teardown exception block, which
//! the headless explorer DOES emit) and authoritative under `--sim`.

use crate::config::{InvariantScope, InvariantsCfg};
use crate::map::RunObs;
use serde_json::{json, Value};

/// Everything the invariant set needs to evaluate one run. Built by the caller
/// from the per-seed log slice (+ the sim manifest, when on the sim tier).
pub struct Observations {
    /// Parsed `EXPLORE:STATE`/`EXPLORE:EDGE` records for this run.
    pub obs: RunObs,
    /// App exception findings already parsed (`exceptions_in_log` /
    /// `app_exceptions`): the `no-exception` edge oracle reuses these verbatim.
    pub exceptions: Vec<Value>,
    /// Per-state max jank percent, keyed by state sig, when the sim tier
    /// attributed frame timing per state. Empty on the headless tier
    /// (`no-jank` then reports nothing and is noted sim-only).
    pub jank_by_sig: std::collections::BTreeMap<String, f64>,
    /// True when a leaked-resource / teardown signal was observed (a teardown
    /// exception block headless, or a soak memory-growth signal under --sim).
    pub leak_signal: Option<String>,
    /// Whether this run is on the simulator tier (enables `no-jank`).
    pub sim: bool,
}

/// Render a jank/hang magnitude with its unit: `>= 16ms` for the millisecond tier,
/// `>= 14 keypresses` / `>= 30 pct` for non-ms tiers, so a finding never implies
/// wall-clock time for a count or percentage (the TUI's bucket is ignored
/// keystrokes, an RSS-only tier's could be janky-frame percent).
fn metric(bucket: i64, unit: &str) -> String {
    if unit == "ms" {
        format!(">= {bucket}ms")
    } else {
        format!(">= {bucket} {unit}")
    }
}

/// A single invariant finding, shaped like every other finding so the existing
/// report/shrink path consumes it unchanged.
pub fn finding(invariant: &str, kind: &str, message: String, sig: Option<&str>) -> Value {
    json!({
        "kind": kind,
        "invariant": invariant,
        "message": message,
        "sig": sig,
        "frames": [],
    })
}

/// Like `finding`, but marks it ADVISORY: a real signal that is NOT deterministic
/// across environments (a raw-pixel or otherwise environment-relative measure),
/// so it is reported for information but never counted as a verdict-bearing,
/// replayable repro. This keeps reproit's "reproduces on any machine" promise
/// honest: only deterministic findings become repros; pixel signals inform.
pub fn advisory_finding(invariant: &str, kind: &str, message: String, sig: Option<&str>) -> Value {
    let mut f = finding(invariant, kind, message, sig);
    f["advisory"] = Value::Bool(true);
    f
}

/// Evaluate the full invariant set (built-ins gated by config + any custom
/// invariants) over one run's observations. Returns all violations.
pub fn evaluate(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
    let mut out = Vec::new();

    // ---- Edge invariants -------------------------------------------------
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
                    "transition {from} --{action}--> showed a transient frame {:.0}% different from both the start and the settled result (advisory: a raw-pixel flash signal, not deterministic across machines)",
                    peak * 100.0
                ),
                Some(from),
            ));
        }
    }

    // ---- State invariants ------------------------------------------------
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
                    "state {sig} renders {} broken-content label(s): {detail} (a stringify/template bug leaked a raw artifact like [object Object]/undefined/null/NaN/{{...}} to the screen)",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

    // no-blank-screen: a reached state that renders NOTHING -- zero visible text
    // nodes and zero tappable controls in a non-empty viewport. The classic
    // white-screen-of-death: an SPA whose mount threw before render, so the
    // server answered 200, the DOM holds a bare root div, and the user sees
    // white. The web runner scans this only after its settle wait and only when
    // document.body exists with a non-zero box, so a page still loading never
    // fires. Structural DOM emptiness (no pixels), so it re-confirms on replay.
    // Empty for runners/states that render content.
    if cfg.no_blank_screen {
        for (sig, items) in &obs.obs.blank_screens {
            let Some((_key, w, h)) = items.first() else {
                continue;
            };
            out.push(finding(
                "no-blank-screen",
                "BLANKSCREEN",
                format!(
                    "state {sig} renders a blank screen: zero visible text nodes and zero tappable controls in a {w}x{h} viewport; the white-screen-of-death, nothing mounted for this route"
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
                        "route {route} leaks {unit}: {first} climbing to {last} across {visits} revisits; each visit adds {unit} that unmount never releases, an unbounded growth that ends in an out-of-memory crash"
                    ),
                    Some(route),
                ));
            }
        }
    }

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
                    "state {sig} control {key} overlaps the {edge} safe-area inset by {by}px{more}: it sits under the notch/status bar/home indicator, so it is obscured or hard to tap"
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
                    "denying the {perm} permission dead-ends at state {sig}: no outgoing action edge (the app strands the user on a permission screen with no way forward){}",
                    screen_hint(&label_set(&obs.obs, sig))
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
                    "state {sig} has {} broken critical asset(s): {detail}; a visible image/encoding asset or required stylesheet/application script failed",
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
                    "state {sig} breaks at 200% zoom with {} reflow violation(s): {detail}; WCAG 1.4.10 requires content to reflow without two-dimensional scrolling and keep controls usable",
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
                    "state {sig} shows different content at {} scroll position(s) after scrolling away and back: {detail}; a list-recycling bug swapped content at a pinned offset",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

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
                    "transition {from} --{action}--> forced {} on the main thread (layout thrashing from repeated forced synchronous reflow; the count is machine-invariant, so this jank reproduces on any runner)",
                    metric(*bucket, unit)
                )
            } else {
                format!(
                    "transition {from} --{action}--> blocked the main thread {} (a dropped-frame jank stall; the handler ran a long synchronous task)",
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
                    "transition {from} --{action}--> froze the main thread {} with no progress (a synchronous hang: the app stopped responding for the duration)",
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
                    "the {role} choice '{outlier}' behaves differently from its siblings: selecting it shifts the global page layout by {mag}px while the other choices do not (an odd-one-out option)"
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
            //  - link check (from == sig): the finding sits on the SOURCE screen
            //    that carries the dead <a>; the screen itself is healthy.
            //  - visited (otherwise): the walk actually landed on the dead
            //    document, so the finding's own screen IS the dead route.
            // /cdn-cgi/l/email-protection is a Cloudflare email-obfuscation URL;
            // on a host not behind Cloudflare nothing decodes it, so it is a
            // real dead link for users, but the fix is specific (restore a plain
            // mailto:), so say that instead of the generic dead-route line.
            // NO parentheses in these messages: scan_detail (modes/fuzz.rs)
            // truncates at the first " (" and would eat the remedy.
            let cf_email = route.starts_with("/cdn-cgi/l/email-protection");
            let message = if from.as_deref() == Some(sig.as_str()) {
                if cf_email {
                    format!(
                        "dead link on this screen: {route} returns HTTP {status}; it is a Cloudflare email-protection URL and this host is not behind Cloudflare, so nothing decodes it; replace the link with a plain mailto:"
                    )
                } else {
                    format!(
                        "dead link on this screen: following the link to {route} returns HTTP {status}; this screen itself loads fine, the link target is what is broken"
                    )
                }
            } else if cf_email {
                format!(
                    "navigated to {route} and got HTTP {status}; it is a Cloudflare email-protection URL and this host is not behind Cloudflare, so nothing decodes it; replace the link with a plain mailto:"
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

    // ---- Graph invariants ------------------------------------------------
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
                    "state {from} double-submits: tapping {action} twice within 150ms fired {method} {url} {count} times; the handler has no guard against rapid double activation, so a double click submits twice"
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
                    "state {from} drops keyboard focus: {action} leaves focus on document.body although the control still exists; a keyboard user loses their place after the interaction"
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
        for (sig, items) in &obs.obs.occlusions {
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
                    "state {sig} has {} occluded control(s): {detail} (a foreign element covers the control's center, so a click hits the overlay instead of the control)",
                    items.len()
                ),
                Some(sig),
            ));
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
                    "state {sig} has {} client-side security issue(s): {detail} (a cross-origin target=_blank without rel=noopener, or an HTTPS page loading http content)",
                    items.len()
                ),
                Some(sig),
            ));
        }
    }

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
                    "state {sig} does not survive rotation: after rotating the screen and rotating back, the app rebuilt a different structure than before, so content or state was lost across the orientation change (expected structure {expected}, got {got})"
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
                    "state {sig} does not survive backgrounding: sending the app to the background and restoring it dropped the user on a different screen or lost state, instead of returning to the same screen (expected structure {expected}, got {got})"
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
                    "state {sig} keeps the soft keyboard open with no text field focused: the keyboard covers content the user never asked for and cannot dismiss by leaving the field"
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
                    "state {sig} keeps a wakelock held after you navigate away: {detail} still held off the screen that needed it, draining the battery by keeping the CPU/screen awake"
                ),
                Some(sig),
            ));
        }
    }

    // ---- Custom invariants ----------------------------------------------
    for c in &cfg.custom {
        out.extend(eval_custom(obs, c));
    }

    out
}

/// States that are dead ends in this run's observed graph: a state that was
/// observed AND has at least one outgoing edge recorded OR is the start, but
/// whose ONLY outgoing edges are `back`. A state with no outgoing edge at all
/// is a dead end iff it is not the start (the start with no edges just means an
/// empty walk). We treat "no non-back exit" as the dead-end condition, which is
/// exactly PLANTED-BUG 6 (the Advanced screen: reachable, but its only exit is
/// system back).
fn permission_traps(obs: &RunObs) -> Vec<String> {
    // Routes (URL path / framework anchor) that have a forward exit from SOME
    // state on them. On a dynamic single-page site, one logical page churns into
    // several structural snapshots (animation, lazy render) that share a route;
    // the snapshot where the walk's budget ran out has no recorded exit and would
    // look like a sink. If a same-route sibling does have a forward exit, it is
    // the same page and the walk could leave it, so the exit-less snapshot is an
    // artifact, not a dead end. A genuinely trapped screen has its own route and
    // is unaffected. Empty when no runner reports routes (TUI/desktop), so the
    // predicate is unchanged there.
    // route -> the label sets of states with a forward exit, from THIS seed's
    // edges plus the aggregate-map fold-in. A sink on such a route is excused only
    // when its labels are a SUBSET of one of these escapable siblings -- a
    // same-or-reduced render of that page (animation churn). A structurally
    // DISTINCT screen sharing the URL (a section toggle with no route change)
    // shows labels the escapable page lacks, so it is not a subset and stays
    // flagged. Empty when no runner reports routes (TUI/desktop), so the predicate
    // is unchanged there.
    let mut route_exit_labels: std::collections::BTreeMap<
        String,
        Vec<std::collections::BTreeSet<String>>,
    > = std::collections::BTreeMap::new();
    for (from, action, to) in &obs.edges {
        if action != "back" && to != from {
            if let Some(route) = obs.routes.get(from) {
                let labels: std::collections::BTreeSet<String> =
                    label_set(obs, from).into_iter().collect();
                route_exit_labels
                    .entry(route.clone())
                    .or_default()
                    .push(labels);
            }
        }
    }
    for (route, sets) in &obs.escapable_route_labels {
        route_exit_labels
            .entry(route.clone())
            .or_default()
            .extend(sets.iter().cloned());
    }
    // route -> label sets of states that offered tappables on SOME snapshot. A
    // zero-tappable snapshot of the same route (header nav scrolled offscreen, a
    // partial render) is not a proven sink IF it is a same-or-reduced render of a
    // tappable-bearing sibling (its labels are a subset). A distinct content-only
    // screen sharing the URL (an "Advanced" pane with no controls) shows labels no
    // tappable sibling has, so it is not excused here.
    let mut routes_with_tappables: std::collections::BTreeMap<
        String,
        Vec<std::collections::BTreeSet<String>>,
    > = std::collections::BTreeMap::new();
    for (sig, &n) in &obs.tappables {
        if n > 0 {
            if let Some(route) = obs.routes.get(sig) {
                let labels: std::collections::BTreeSet<String> =
                    label_set(obs, sig).into_iter().collect();
                routes_with_tappables
                    .entry(route.clone())
                    .or_default()
                    .push(labels);
            }
        }
    }

    let mut out = Vec::new();
    for sig in obs.states.keys() {
        let is_start = obs.start.as_deref() == Some(sig.as_str());
        // Reachable as a destination of some edge, or the start state.
        let reachable = is_start || obs.edges.iter().any(|(_, _, to)| to == sig);
        if !reachable {
            continue;
        }
        // A START state the walk never acted from is an empty/unproductive walk,
        // not a proven sink (this fn's contract, and the common shape of a web
        // seed that churned without recording an exit). Only the start gets this
        // pass: a NON-start state reached with no exit IS a genuine sink (the
        // Advanced-screen planted bug), so it stays flagged.
        let acted_from = obs.edges.iter().any(|(from, _, _)| from == sig);
        if is_start && !acted_from {
            continue;
        }
        let has_forward_exit = obs
            .edges
            .iter()
            .any(|(from, action, to)| from == sig && action != "back" && to != sig);
        if has_forward_exit {
            continue;
        }
        // Same page has a forward exit -> this is a transient snapshot of an
        // escapable page, not a real sink. Two sources: a same-route sibling in
        // THIS seed's sparse graph, and the AGGREGATE map's escapable routes
        // folded in by the caller (covers the common case where one seed visited
        // the page only as its budget terminus).
        if let Some(route) = obs.routes.get(sig) {
            if let Some(sibling_sets) = route_exit_labels.get(route) {
                let sink_labels: std::collections::BTreeSet<String> =
                    label_set(obs, sig).into_iter().collect();
                // Suppress only a same-or-reduced render of an escapable sibling:
                // the sink shows nothing the escapable page does not already show.
                // A distinct screen at the same URL carries labels no escapable
                // sibling has, so it is not a subset and is correctly flagged.
                if sibling_sets.iter().any(|s| sink_labels.is_subset(s)) {
                    continue;
                }
            }
        }
        // Unexplored terminus, not a proven sink: the state OFFERED tappable
        // elements the walk never tapped (more tappables than recorded tap
        // actions from it). A real dead end either offers no forward action or
        // has all its actions exhausted with no exit; a leaf page reached as the
        // budget terminus (e.g. a blog article whose header nav was deduped after
        // being tried elsewhere) still has untapped nav and is not a trap.
        // tappables=0 (no element data, as in unit fixtures) never triggers this,
        // so a genuine no-action sink stays flagged.
        let offered = obs.tappables.get(sig).copied().unwrap_or(0);
        if offered > 0 {
            // Count any FORWARD action tried from this state, not just `tap:`. The
            // forward-action verb differs by platform -- web/native a11y tap and
            // type, the TUI presses keys (`key:Down`/`key:Enter`) -- so keying off
            // `tap:` alone made every TUI state look like all its offered elements
            // were untried (suppression always fired, real TUI sinks never flagged).
            // `!= "back"` is the platform-neutral "the walk tried something here".
            let tried = obs
                .edges
                .iter()
                .filter(|(from, action, _)| from == sig && *action != "back")
                .count();
            if offered > tried {
                continue;
            }
        } else if let Some(route) = obs.routes.get(sig) {
            // This snapshot saw zero tappables. If a same-route sibling that DID
            // offer tappables is a superset of this one's labels, this is a
            // transient/partial render of that page, not a sink. A distinct
            // content-only screen at the same URL has labels no tappable sibling
            // carries, so it stays flagged.
            if let Some(sibling_sets) = routes_with_tappables.get(route) {
                let sink_labels: std::collections::BTreeSet<String> =
                    label_set(obs, sig).into_iter().collect();
                if sibling_sets.iter().any(|s| sink_labels.is_subset(s)) {
                    continue;
                }
            }
        }
        out.push(sig.clone());
    }
    out
}

/// Re-evaluation outcome for a single recorded graph-invariant violation,
/// replayed by `check`. Distinguishes "the invariant tripped again" (a real
/// regression) from "it held" (the fix worked) from "the replay never reached
/// the violating context" (re-record). Maps 1:1 onto the per-run verdict
/// `check` aggregates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphRecheck {
    /// The recorded state is present and the finding still violates its predicate.
    StillViolating,
    /// The recorded state is present but the finding predicate no longer fails.
    Fixed,
    /// The recorded state never appeared in the replay graph: the path to the
    /// finding's context is gone, so the invariant could not be re-evaluated.
    NotReached,
}

/// Re-confirm an older flicker finding over a replay log, mirroring
/// the recorded violating state sig (`trigger.sig`, the
/// transition's FROM state) is re-evaluated against the replay's
/// presented-frame `EXPLORE:FLICKER` records. DOM identity churn alone is not
/// visual evidence and intentionally cannot re-confirm a finding.
///   - the replay shows a transient frame divergence FROM that sig -> StillViolating
///   - the sig is reached but no transition from it churned -> Fixed (held)
///   - the sig never appears in the replay graph -> NotReached (re-record)
pub fn recheck_rerender_flicker(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    let flickers = obs.paint_flickers.keys().any(|(from, _)| from == sig);
    if flickers {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY flicker signal (used by `check` for a
/// flicker repro that recorded no specific violating sig).
pub fn any_rerender_flicker(obs: &RunObs) -> bool {
    !obs.paint_flickers.is_empty()
}

/// Re-confirm a `no-broken-render` (content-bug) finding over a replay log,
/// mirroring `recheck_overflow`: the recorded violating state sig is re-evaluated
/// against the replay's `EXPLORE:CONTENTBUG` records.
///   - the replay still renders broken content at that sig -> StillViolating
///   - the sig is reached but renders no broken content -> Fixed (the fix held)
///   - the sig never appears in the replay graph -> NotReached (re-record).
pub fn recheck_content_bug(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    if obs
        .content_bugs
        .get(sig)
        .is_some_and(|items| !items.is_empty())
    {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY broken-content signal (used by `check` for a
/// content-bug repro that recorded no specific violating sig).
pub fn any_content_bug(obs: &RunObs) -> bool {
    obs.content_bugs.values().any(|items| !items.is_empty())
}

/// Re-confirm a `no-jank` (web jank) finding over a replay log. A jank stall is
/// keyed by (from, action), so re-evaluate whether ANY transition FROM the
/// recorded sig still janks.
///   - a transition from that sig still janks -> StillViolating
///   - the sig is reached but no transition from it janks -> Fixed
///   - the sig never appears in the replay graph -> NotReached.
pub fn recheck_jank(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    if obs.janks.keys().any(|(from, _)| from == sig) {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY jank signal (used by `check` for a jank
/// repro that recorded no specific violating sig).
pub fn any_jank(obs: &RunObs) -> bool {
    !obs.janks.is_empty()
}

/// Re-confirm a `no-hang` (freeze) finding over a replay log, mirroring
/// `recheck_jank` against the `EXPLORE:HANG` records.
pub fn recheck_hang(obs: &RunObs, sig: &str) -> GraphRecheck {
    if !obs.states.contains_key(sig) {
        return GraphRecheck::NotReached;
    }
    if obs.hangs.keys().any(|(from, _)| from == sig) {
        GraphRecheck::StillViolating
    } else {
        GraphRecheck::Fixed
    }
}

/// Whether the replay graph has ANY hang signal (used by `check` for a hang
/// repro that recorded no specific violating sig).
pub fn any_hang(obs: &RunObs) -> bool {
    !obs.hangs.is_empty()
}

fn label_set(obs: &RunObs, sig: &str) -> Vec<String> {
    obs.states.get(sig).cloned().unwrap_or_default()
}

fn screen_hint(labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!(
            " [{}]",
            labels
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Evaluate one custom invariant against the run.
fn eval_custom(obs: &Observations, c: &crate::config::CustomInvariant) -> Vec<Value> {
    let mut out = Vec::new();
    match &c.scope {
        InvariantScope::State => {
            for (sig, labels) in &obs.obs.states {
                // labels-match: every state's labels must contain a match.
                if let Some(re) = &c.labels_match {
                    if !labels.iter().any(|l| re.is_match(l)) {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "state {sig} violates {}: no label matches /{}/{}",
                                c.id,
                                re.as_str(),
                                screen_hint(labels)
                            ),
                            Some(sig),
                        ));
                    }
                }
                // labels-absent: no label may match.
                if let Some(re) = &c.labels_absent {
                    if let Some(hit) = labels.iter().find(|l| re.is_match(l)) {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "state {sig} violates {}: label {hit:?} matches forbidden /{}/",
                                c.id,
                                re.as_str()
                            ),
                            Some(sig),
                        ));
                    }
                }
            }
        }
        InvariantScope::Edge => {
            // Custom edge invariant: forbid an action (by regex) anywhere, e.g.
            // "no destructive tap reachable". Start simple: a forbidden-action
            // regex flags any edge whose action string matches.
            if let Some(re) = &c.action_absent {
                for (from, action, to) in &obs.obs.edges {
                    if re.is_match(action) {
                        out.push(finding(
                            &c.id,
                            "INVARIANT",
                            format!(
                                "edge {from} --{action}--> {to} violates {}: forbidden action /{}/",
                                c.id,
                                re.as_str()
                            ),
                            Some(from),
                        ));
                    }
                }
            }
        }
        InvariantScope::Graph => {
            // Custom graph invariant: a label that MUST be reachable.
            if let Some(re) = &c.must_reach {
                let reached = obs
                    .obs
                    .states
                    .values()
                    .any(|labels| labels.iter().any(|l| re.is_match(l)));
                if !reached {
                    out.push(finding(
                        &c.id,
                        "INVARIANT",
                        format!(
                            "invariant {} violated: no observed state has a label matching required /{}/",
                            c.id,
                            re.as_str()
                        ),
                        None,
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn advisory_finding_is_flagged_but_still_classifies() {
        // paint-flicker is a raw-pixel signal: reported (advisory) but never a
        // verdict-bearing repro. It must carry the advisory flag yet still map to
        // its oracle so the report can group it.
        let f = advisory_finding("paint-flicker", "FLICKER", "flash".into(), Some("s"));
        assert_eq!(f.get("advisory").and_then(Value::as_bool), Some(true));
        assert_eq!(
            f.get("invariant").and_then(Value::as_str),
            Some("paint-flicker")
        );
        assert_eq!(
            crate::crosscut::classify(&f),
            crate::crosscut::Oracle::Flicker
        );
    }

    #[test]
    fn stuck_keyboard_fires_per_sig_and_respects_gate() {
        let mut o = obs_with(&[("s1", &["Detail"])], &[], Some("s1"));
        o.obs.stuck_keyboards.insert("s1".to_string());
        let f = evaluate(&o, &InvariantsCfg::default());
        let hit = f
            .iter()
            .find(|x| x["invariant"] == "no-stuck-keyboard")
            .expect("stuck-keyboard finding for s1");
        assert_eq!(hit["kind"], "STUCKKEYBOARD");
        assert_eq!(hit["sig"], "s1");
        assert_eq!(
            crate::crosscut::classify(hit),
            crate::crosscut::Oracle::StuckKeyboard
        );
        // The message keeps essentials before any parenthesis (scan detail
        // truncates at the first " (").
        let msg = hit["message"].as_str().unwrap();
        assert!(msg.contains("soft keyboard open with no text field focused"));
        // Gated off: no finding.
        let cfg = InvariantsCfg {
            no_stuck_keyboard: false,
            ..Default::default()
        };
        assert!(!evaluate(&o, &cfg)
            .iter()
            .any(|x| x["invariant"] == "no-stuck-keyboard"));
        // A clean run (no marker) stays silent.
        let clean = obs_with(&[("s1", &["Detail"])], &[], Some("s1"));
        assert!(!evaluate(&clean, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-stuck-keyboard"));
    }

    #[test]
    fn wakelock_leak_fires_per_sig_and_respects_gate() {
        let mut o = obs_with(&[("video", &["Player"])], &[], Some("video"));
        o.obs.wakelock_leaks.insert(
            "video".to_string(),
            vec![
                ("com.app:VideoPlayback".to_string(), "wakelock".to_string()),
                ("KEEP_SCREEN_ON".to_string(), "keep-screen-on".to_string()),
            ],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let hit = f
            .iter()
            .find(|x| x["invariant"] == "no-wakelock-leak")
            .expect("wakelock finding for video");
        assert_eq!(hit["kind"], "WAKELOCK");
        assert_eq!(hit["sig"], "video");
        assert_eq!(
            crate::crosscut::classify(hit),
            crate::crosscut::Oracle::WakeLock
        );
        // The message keeps the essentials (the leaked tag) before any parenthesis
        // (scan detail truncates at the first " (").
        let msg = hit["message"].as_str().unwrap();
        let head = msg.split(" (").next().unwrap();
        assert!(head.contains("com.app:VideoPlayback"));
        assert!(head.contains("navigate away"));
        // Gated off: no finding.
        let cfg = InvariantsCfg {
            no_wakelock_leak: false,
            ..Default::default()
        };
        assert!(!evaluate(&o, &cfg)
            .iter()
            .any(|x| x["invariant"] == "no-wakelock-leak"));
        // A clean run (no marker) stays silent.
        let clean = obs_with(&[("video", &["Player"])], &[], Some("video"));
        assert!(!evaluate(&clean, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-wakelock-leak"));
    }

    fn obs_with(
        states: &[(&str, &[&str])],
        edges: &[(&str, &str, &str)],
        start: Option<&str>,
    ) -> Observations {
        let mut s = BTreeMap::new();
        for (sig, labels) in states {
            s.insert(
                sig.to_string(),
                labels.iter().map(|x| x.to_string()).collect(),
            );
        }
        Observations {
            obs: RunObs {
                states: s,
                routes: Default::default(),
                tappables: Default::default(),
                elements: Default::default(),
                texts: Default::default(),
                occlusions: Default::default(),
                security: Default::default(),
                blank_screens: Default::default(),
                broken_assets: Default::default(),
                zoom_reflows: Default::default(),
                scroll_round_trips: Default::default(),
                rotation_losses: Default::default(),
                background_losses: Default::default(),
                stuck_keyboards: Default::default(),
                edges: edges
                    .iter()
                    .map(|(f, a, t)| (f.to_string(), a.to_string(), t.to_string()))
                    .collect(),
                start: start.map(String::from),
                escapable_route_labels: Default::default(),
                gaps: Default::default(),
                rerenders: Default::default(),
                paint_flickers: Default::default(),
                content_bugs: Default::default(),
                janks: Default::default(),
                duplicate_submits: Default::default(),
                focus_losses: Default::default(),
                hangs: Default::default(),
                choice_bugs: Default::default(),
                broken_routes: Default::default(),
                app_invariants: Default::default(),
                listener_leaks: Default::default(),
                wakelock_leaks: Default::default(),
                safe_areas: Default::default(),
                permission_screens: Default::default(),
            },
            exceptions: vec![],
            jank_by_sig: BTreeMap::new(),
            leak_signal: None,
            sim: false,
        }
    }

    fn kinds(findings: &[Value]) -> Vec<String> {
        findings
            .iter()
            .map(|f| {
                f.get("invariant")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn app_invariant_violation_becomes_a_finding() {
        // An app-registered invariant the SDK reported as failed in a state
        // becomes an `app-invariant` finding (kind INVARIANT), naming the state
        // and carrying the SDK's message. Disabling the flag silences it, and a
        // clean run produces none.
        let mut o = obs_with(&[("s1", &["Cart"])], &[], Some("s1"));
        o.obs.app_invariants.insert(
            "s1".to_string(),
            vec![(
                "cart total never negative".to_string(),
                "total was -5".to_string(),
            )],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f
            .iter()
            .find(|x| x["invariant"] == "app-invariant")
            .expect("app-invariant finding");
        assert_eq!(v["kind"], "INVARIANT");
        assert_eq!(v["sig"], "s1");
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("cart total never negative"), "got {msg}");
        assert!(msg.contains("total was -5"), "got {msg}");

        let cfg = InvariantsCfg {
            no_invariant_violation: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"app-invariant".to_string()));

        let clean = obs_with(&[("s1", &["Cart"])], &[], Some("s1"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"app-invariant".to_string()));
    }

    #[test]
    fn dom_identity_churn_is_not_a_flicker() {
        // Replacing unchanged DOM anchors is an implementation detail. Without a
        // transient presented frame, it must remain silent.
        let mut o = obs_with(&[("s1", &["My App"])], &[], Some("s1"));
        o.obs.rerenders.insert(
            ("s1".to_string(), "tap:key:id:bad".to_string()),
            vec!["id:hdr".to_string(), "id:nav".to_string()],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(!kinds(&f).contains(&"rerender-flicker".to_string()));
    }

    #[test]
    fn no_duplicate_submit_flags_a_double_fired_request() {
        // The double-dispatch probe found a pay button that fired the same POST
        // twice: the finding carries the from-sig, action, method, url, and
        // count (all before any parenthesis, so scan detail keeps them).
        let mut o = obs_with(&[("s1", &["Checkout"])], &[], Some("s1"));
        o.obs.duplicate_submits.insert(
            ("s1".to_string(), "tap:key:id:pay".to_string()),
            (
                "POST".to_string(),
                "https://app.example/api/orders".to_string(),
                2,
            ),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-duplicate-submit".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-duplicate-submit")
            .unwrap();
        assert_eq!(v["kind"], "DUPSUBMIT");
        let msg = v["message"].as_str().unwrap();
        for needle in [
            "s1",
            "tap:key:id:pay",
            "POST",
            "https://app.example/api/orders",
            "2 times",
        ] {
            assert!(msg.contains(needle), "message misses {needle}: {msg}");
        }
        // The finding classifies to its own oracle category.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::DuplicateSubmit
        );
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_duplicate_submit: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-duplicate-submit".to_string()));
        // A run with no DUPSUBMIT records (probe off or every handler guarded)
        // stays silent.
        let clean = obs_with(&[("s1", &["Checkout"])], &[], Some("s1"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-duplicate-submit".to_string()));
    }

    #[test]
    fn no_focus_loss_flags_a_dropped_focus_transition() {
        // A tap that left keyboard focus on <body> while the control survived:
        // the finding names the from-sig and the action (essentials before any
        // parenthesis, so scan detail keeps them).
        let mut o = obs_with(&[("s1", &["Todo"])], &[], Some("s1"));
        o.obs
            .focus_losses
            .insert(("s1".to_string(), "tap:key:id:add".to_string()));
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-focus-loss".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-focus-loss")
            .unwrap();
        assert_eq!(v["kind"], "FOCUSLOSS");
        let msg = v["message"].as_str().unwrap();
        for needle in ["s1", "tap:key:id:add", "document.body"] {
            assert!(msg.contains(needle), "message misses {needle}: {msg}");
        }
        // The finding classifies to its own oracle category.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::FocusLoss
        );
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_focus_loss: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-focus-loss".to_string()));
        // A run with no FOCUSLOSS records stays silent.
        let clean = obs_with(&[("s1", &["Todo"])], &[], Some("s1"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-focus-loss".to_string()));
    }

    #[test]
    fn recheck_rerender_flicker_distinguishes_back_held_unreached() {
        // DOM churn alone does not re-confirm a visible flicker.
        let mut o = obs_with(&[("s1", &["x"])], &[], Some("s1"));
        o.obs.rerenders.insert(
            ("s1".to_string(), "tap:key:id:bad".to_string()),
            vec!["id:hdr".to_string()],
        );
        assert_eq!(recheck_rerender_flicker(&o.obs, "s1"), GraphRecheck::Fixed);
        // A transient presented-frame divergence does re-confirm it.
        o.obs
            .paint_flickers
            .insert(("s1".to_string(), "tap:key:id:bad".to_string()), 0.42);
        assert_eq!(
            recheck_rerender_flicker(&o.obs, "s1"),
            GraphRecheck::StillViolating
        );
        // Fixed: the sig is observed but nothing churns from it (the fix held).
        let held = obs_with(&[("s1", &["x"])], &[], Some("s1"));
        assert_eq!(
            recheck_rerender_flicker(&held.obs, "s1"),
            GraphRecheck::Fixed
        );
        // NotReached: the sig never appeared in the replay graph.
        assert_eq!(
            recheck_rerender_flicker(&held.obs, "other"),
            GraphRecheck::NotReached
        );
    }

    #[test]
    fn no_broken_render_flags_a_state_with_a_broken_label() {
        // A state rendering [object Object] fires; a clean state stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("acct", &["Account"])],
            &[("home", "tap:Go", "acct")],
            Some("home"),
        );
        o.obs.content_bugs.insert(
            "acct".to_string(),
            vec![(
                "id:acct-name".to_string(),
                "object-object".to_string(),
                "Account: [object Object]".to_string(),
            )],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-broken-render".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-broken-render")
            .unwrap();
        assert_eq!(v["sig"], "acct");
        assert_eq!(v["kind"], "CONTENTBUG");
        assert!(v["message"].as_str().unwrap().contains("object Object"));
        // The clean `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-broken-render" && x["sig"] == "home"));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_broken_render: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-broken-render".to_string()));
    }

    #[test]
    fn rotation_and_background_loss_fire_and_respect_flags() {
        // A state that regressed its structure across a rotation round-trip fires
        // no-rotation-loss; one that regressed across background/restore fires
        // no-background-loss. A clean run is silent; each flag gates its finding.
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs
            .rotation_losses
            .insert("home".to_string(), ("abc".to_string(), "def".to_string()));
        o.obs
            .background_losses
            .insert("home".to_string(), ("abc".to_string(), "xyz".to_string()));
        let f = evaluate(&o, &InvariantsCfg::default());
        let rot = f
            .iter()
            .find(|x| x["invariant"] == "no-rotation-loss")
            .expect("rotation finding");
        assert_eq!(rot["kind"], "ROTATION");
        assert_eq!(rot["sig"], "home");
        assert_eq!(
            crate::crosscut::classify(rot),
            crate::crosscut::Oracle::Rotation
        );
        // Essentials before any parenthesis: the message states the loss first.
        assert!(rot["message"]
            .as_str()
            .unwrap()
            .contains("does not survive rotation"));
        let bg = f
            .iter()
            .find(|x| x["invariant"] == "no-background-loss")
            .expect("background finding");
        assert_eq!(bg["kind"], "BGRESTORE");
        assert_eq!(
            crate::crosscut::classify(bg),
            crate::crosscut::Oracle::BackgroundRestore
        );
        // A clean run reports neither.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        let cf = kinds(&evaluate(&clean, &InvariantsCfg::default()));
        assert!(!cf.contains(&"no-rotation-loss".to_string()));
        assert!(!cf.contains(&"no-background-loss".to_string()));
        // Each flag suppresses its own finding independently.
        let cfg = InvariantsCfg {
            no_rotation_loss: false,
            no_background_loss: false,
            ..Default::default()
        };
        let gated = kinds(&evaluate(&o, &cfg));
        assert!(!gated.contains(&"no-rotation-loss".to_string()));
        assert!(!gated.contains(&"no-background-loss".to_string()));
    }

    #[test]
    fn no_blank_screen_flags_an_empty_state() {
        // A state that rendered nothing (zero visible text nodes, zero
        // tappables, non-empty viewport) fires; a clean state stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("dead", &[])],
            &[("home", "tap:Go", "dead")],
            Some("home"),
        );
        o.obs.blank_screens.insert(
            "dead".to_string(),
            vec![("tag:body".to_string(), 1280, 720)],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-blank-screen".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-blank-screen")
            .unwrap();
        assert_eq!(v["sig"], "dead");
        assert_eq!(v["kind"], "BLANKSCREEN");
        // Essentials before any parenthesis: the message names the viewport.
        assert!(v["message"].as_str().unwrap().contains("1280x720"));
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::BlankScreen
        );
        // The content-bearing `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-blank-screen" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-blank-screen".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_blank_screen: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-blank-screen".to_string()));
    }

    #[test]
    fn no_safe_area_flags_a_control_in_an_inset() {
        // A control whose hit rect overlaps a device inset fires; a screen with
        // no control in an inset stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("scan", &["Done"])],
            &[("home", "tap:Go", "scan")],
            Some("home"),
        );
        o.obs.safe_areas.insert(
            "scan".to_string(),
            vec![("key:done".to_string(), "top".to_string(), 18)],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-safe-area-collision".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-safe-area-collision")
            .unwrap();
        assert_eq!(v["sig"], "scan");
        assert_eq!(v["kind"], "SAFEAREA");
        // Essentials before any parenthesis: the control, edge, and depth.
        let msg = v["message"].as_str().unwrap();
        let head = msg.split(" (").next().unwrap();
        assert!(head.contains("key:done"));
        assert!(head.contains("top"));
        assert!(head.contains("18px"));
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::SafeArea
        );
        // The clean `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-safe-area-collision" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-safe-area-collision".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_safe_area: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-safe-area-collision".to_string()));
    }

    #[test]
    fn no_permission_dead_end_flags_a_denied_permission_sink() {
        // Under a denial sweep, a post-denial screen that is ALSO a graph dead
        // end fires and names the permission; a post-denial screen that has a
        // forward exit does NOT fire (it is not a dead end), and a dead end that
        // was NOT marked as post-denial is ignored.
        // `perm` (post-denial sink) is a genuine sink; `flow` has a forward exit.
        let mut o = obs_with(
            &[
                ("home", &["Scan"]),
                ("perm", &["Enable Camera"]),
                ("flow", &["Manual"]),
                ("ok", &["Home"]),
            ],
            &[
                ("home", "tap:Scan", "perm"),
                ("home", "tap:Manual", "flow"),
                ("flow", "tap:Manual", "ok"),
            ],
            Some("home"),
        );
        o.obs
            .permission_screens
            .insert("perm".to_string(), "camera".to_string());
        o.obs
            .permission_screens
            .insert("flow".to_string(), "camera".to_string());
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-permission-dead-end")
            .expect("permission dead-end fires on the post-denial sink");
        assert_eq!(v["sig"], "perm");
        assert_eq!(v["kind"], "PERMISSIONWALK");
        // Essentials before any parenthesis: the permission and the state.
        let head = v["message"].as_str().unwrap().split(" (").next().unwrap();
        assert!(head.contains("camera"));
        assert!(head.contains("perm"));
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::PermissionWalk
        );
        // `flow` has a forward exit, so the permission oracle does NOT flag it.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-permission-dead-end" && x["sig"] == "flow"));
        // Outside a denial sweep (no marked screens) the oracle is silent, even
        // when the graph has the same dead end.
        let mut clean = obs_with(
            &[("home", &["Scan"]), ("perm", &["Enable Camera"])],
            &[("home", "tap:Scan", "perm")],
            Some("home"),
        );
        clean.obs.permission_screens.clear();
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-permission-dead-end".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_permission_dead_end: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-permission-dead-end".to_string()));
    }

    #[test]
    fn no_listener_leak_flags_a_monotonic_climb_per_route() {
        // A route whose listeners/nodes climb across revisits fires; a stable
        // route stays silent. The runner only emits a monotonic climb, so the
        // Rust side simply surfaces every reported metric.
        let mut o = obs_with(&[("home", &["Home"])], &[], Some("home"));
        o.obs.listener_leaks.insert(
            "/detail".to_string(),
            (
                5,
                vec![
                    ("listeners".to_string(), 8, 40),
                    ("nodes".to_string(), 120, 180),
                ],
            ),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let leak: Vec<_> = f
            .iter()
            .filter(|x| x["invariant"] == "no-listener-leak")
            .collect();
        // One finding per leaking metric (listeners + nodes).
        assert_eq!(leak.len(), 2, "got {:?}", kinds(&f));
        assert_eq!(leak[0]["kind"], "LISTENERLEAK");
        assert_eq!(leak[0]["sig"], "/detail");
        // Route + climb lead the message, before any " (".
        let msg = leak[0]["message"].as_str().unwrap();
        assert!(msg.contains("/detail") && msg.contains("40"), "got {msg}");
        // Classifies to the Leak oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(leak[0]),
            crate::crosscut::Oracle::Leak
        );
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Home"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-listener-leak".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_listener_leak: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-listener-leak".to_string()));
    }

    #[test]
    fn no_reflow_break_flags_a_route_that_breaks_at_zoom() {
        // A route that grows a horizontal scrollbar or collapses a tappable at
        // 200% zoom fires; a cleanly reflowing route stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("table", &["Report"])],
            &[("home", "tap:Go", "table")],
            Some("home"),
        );
        o.obs.zoom_reflows.insert(
            "table".to_string(),
            vec![
                ("tag:html".to_string(), "hscroll".to_string(), 560),
                ("key:id:save".to_string(), "collapsed".to_string(), 0),
            ],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-reflow-break".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-reflow-break")
            .unwrap();
        assert_eq!(v["sig"], "table");
        assert_eq!(v["kind"], "ZOOMREFLOW");
        // Essentials before any parenthesis: count + per-item break detail.
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("2 reflow violation(s)"), "message: {msg}");
        assert!(
            msg.contains("tag:html scrolls horizontally by 560px"),
            "message: {msg}"
        );
        assert!(
            msg.contains("key:id:save collapses to 0px"),
            "message: {msg}"
        );
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::ZoomReflow
        );
        // The cleanly reflowing `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-reflow-break" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-reflow-break".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_zoom_reflow: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-reflow-break".to_string()));
    }

    #[test]
    fn no_scroll_recycle_flags_content_that_differs_after_round_trip() {
        // A list whose content at a pinned offset differs after scrolling away
        // and back fires; a stable list stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("feed", &["Feed"])],
            &[("home", "tap:Go", "feed")],
            Some("home"),
        );
        o.obs.scroll_round_trips.insert(
            "feed".to_string(),
            vec![(
                "y=0".to_string(),
                "Alpha|Bravo".to_string(),
                "Charlie|Delta".to_string(),
            )],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-scroll-recycle".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-scroll-recycle")
            .unwrap();
        assert_eq!(v["sig"], "feed");
        assert_eq!(v["kind"], "SCROLLROUNDTRIP");
        // Essentials before any parenthesis: count + before/after content.
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("1 scroll position(s)"), "message: {msg}");
        assert!(
            msg.contains("at y=0 \"Alpha|Bravo\" became \"Charlie|Delta\""),
            "message: {msg}"
        );
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::ScrollRoundTrip
        );
        // The `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-scroll-recycle" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-scroll-recycle".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_scroll_round_trip: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-scroll-recycle".to_string()));
    }

    #[test]
    fn no_broken_asset_flags_dead_subresources() {
        // A state with visible and critical network asset failures fires once,
        // carrying the reasons in the detail; a clean state stays silent.
        let mut o = obs_with(
            &[("home", &["Go"]), ("shop", &["Shop"])],
            &[("home", "tap:Go", "shop")],
            Some("home"),
        );
        o.obs.broken_assets.insert(
            "shop".to_string(),
            vec![
                (
                    "key:id:hero".to_string(),
                    "img".to_string(),
                    "missing.png".to_string(),
                ),
                (
                    "tag:link".to_string(),
                    "stylesheet-http".to_string(),
                    "https://app.test/app.css status=404 content-type=text/css".to_string(),
                ),
                (
                    "key:id:desc".to_string(),
                    "tofu".to_string(),
                    "glitch \u{FFFD}".to_string(),
                ),
            ],
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        assert!(
            kinds(&f).contains(&"no-broken-asset".to_string()),
            "got {:?}",
            kinds(&f)
        );
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-broken-asset")
            .unwrap();
        assert_eq!(v["sig"], "shop");
        assert_eq!(v["kind"], "BROKENASSET");
        // Essentials before any parenthesis: count + per-item reason detail.
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("3 broken critical asset(s)"), "message: {msg}");
        assert!(msg.contains("[img] missing.png"), "message: {msg}");
        assert!(msg.contains("[stylesheet-http]"), "message: {msg}");
        // The finding classifies to its own oracle, never falling back to crash.
        assert_eq!(
            crate::crosscut::classify(v),
            crate::crosscut::Oracle::BrokenAsset
        );
        // The clean `home` state is not flagged.
        assert!(!f
            .iter()
            .any(|x| x["invariant"] == "no-broken-asset" && x["sig"] == "home"));
        // A clean run reports nothing.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!kinds(&evaluate(&clean, &InvariantsCfg::default()))
            .contains(&"no-broken-asset".to_string()));
        // Disabling the invariant suppresses it.
        let cfg = InvariantsCfg {
            no_broken_asset: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-broken-asset".to_string()));
    }

    #[test]
    fn recheck_content_bug_distinguishes_held_unreached() {
        let mut o = obs_with(&[("s1", &["x"])], &[], Some("s1"));
        o.obs.content_bugs.insert(
            "s1".to_string(),
            vec![("id:x".to_string(), "null".to_string(), "null".to_string())],
        );
        assert_eq!(
            recheck_content_bug(&o.obs, "s1"),
            GraphRecheck::StillViolating
        );
        let held = obs_with(&[("s1", &["x"])], &[], Some("s1"));
        assert_eq!(recheck_content_bug(&held.obs, "s1"), GraphRecheck::Fixed);
        assert_eq!(
            recheck_content_bug(&held.obs, "other"),
            GraphRecheck::NotReached
        );
    }

    #[test]
    fn no_choice_anomaly_flags_an_outlier_choice() {
        // A multi-choice component reported one option that shifted the global
        // layout while its siblings did not. The differential outlier fires; a
        // run with no choice-bugs stays silent.
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs.choice_bugs.push((
            "home".to_string(),
            "tab".to_string(),
            "Go".to_string(),
            "role:tab#3".to_string(),
            720,
        ));
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-choice-anomaly")
            .unwrap();
        assert_eq!(v["sig"], "home");
        assert!(v["message"].as_str().unwrap().contains("Go"));
        // Empty -> no finding.
        let clean = obs_with(&[("home", &["Go"])], &[], Some("home"));
        assert!(!evaluate(&clean, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-choice-anomaly"));
        // Toggle off suppresses it.
        let cfg = InvariantsCfg {
            no_choice_anomaly: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-choice-anomaly".to_string()));
    }

    #[test]
    fn no_broken_route_flags_a_4xx_state() {
        // A visited route whose document responded >= 400 is a dead route the app
        // linked to. It fires once per broken route; a clean run stays silent.
        let mut o = obs_with(&[("dl", &["Page not found"])], &[], Some("dl"));
        o.obs.broken_routes.push((
            "dl".to_string(),
            "/download".to_string(),
            404,
            Some("home".to_string()),
        ));
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f
            .iter()
            .find(|x| x["invariant"] == "no-broken-route")
            .unwrap();
        assert_eq!(v["sig"], "dl");
        assert!(v["message"].as_str().unwrap().contains("/download"));
        assert!(v["message"].as_str().unwrap().contains("404"));
        // Visited shape (from != sig): the flagged screen IS the dead document.
        assert!(v["message"]
            .as_str()
            .unwrap()
            .contains("this screen's document"));
        // Link-check shape (from == sig): the finding sits on the healthy SOURCE
        // screen, so the copy must say the LINK target is what is broken.
        let mut o2 = obs_with(&[("home", &["Classes"])], &[], Some("home"));
        o2.obs.broken_routes.push((
            "home".to_string(),
            "/gone".to_string(),
            404,
            Some("home".to_string()),
        ));
        let f2 = evaluate(&o2, &InvariantsCfg::default());
        let v2 = f2
            .iter()
            .find(|x| x["invariant"] == "no-broken-route")
            .unwrap();
        let m2 = v2["message"].as_str().unwrap();
        assert!(m2.contains("dead link on this screen"));
        assert!(m2.contains("/gone"));
        assert!(m2.contains("loads fine"));
        // Cloudflare email-protection links get the specific remedy, not the
        // generic dead-route line.
        let mut o3 = obs_with(&[("home", &["Contact"])], &[], Some("home"));
        o3.obs.broken_routes.push((
            "home".to_string(),
            "/cdn-cgi/l/email-protection".to_string(),
            404,
            Some("home".to_string()),
        ));
        let f3 = evaluate(&o3, &InvariantsCfg::default());
        let m3 = f3
            .iter()
            .find(|x| x["invariant"] == "no-broken-route")
            .unwrap()["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(m3.contains("mailto:"));
        assert!(m3.contains("Cloudflare email-protection"));
        // Empty -> no finding.
        let clean = obs_with(&[("dl", &["x"])], &[], Some("dl"));
        assert!(!evaluate(&clean, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-broken-route"));
        // Toggle off suppresses it.
        let cfg = InvariantsCfg {
            no_broken_route: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-broken-route".to_string()));
    }

    #[test]
    fn no_jank_fires_on_a_web_longtask_stall_without_sim() {
        // The web jank path is NOT gated on sim: a longtask stall on a transition
        // fires headless. A clean walk (no janks) stays silent.
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs.janks.insert(
            ("home".to_string(), "tap:key:testid:recompute".to_string()),
            (200, "ms".to_string()),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-jank").unwrap();
        assert_eq!(v["kind"], "PERF");
        assert_eq!(v["sig"], "home");
        assert!(v["message"].as_str().unwrap().contains(">= 200ms"));
        // `recheck_jank` re-confirms by FROM-sig.
        assert_eq!(recheck_jank(&o.obs, "home"), GraphRecheck::StillViolating);
        // Disabling no-jank suppresses it.
        let cfg = InvariantsCfg {
            no_jank: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-jank".to_string()));
    }

    #[test]
    fn no_hang_fires_on_a_web_freeze() {
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs.hangs.insert(
            ("home".to_string(), "tap:key:testid:export".to_string()),
            (2000, "ms".to_string()),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-hang").unwrap();
        assert_eq!(v["kind"], "HANG");
        assert_eq!(v["sig"], "home");
        assert!(v["message"].as_str().unwrap().contains("froze"));
        assert!(v["message"].as_str().unwrap().contains(">= 2000ms"));
        assert_eq!(recheck_hang(&o.obs, "home"), GraphRecheck::StillViolating);
        let cfg = InvariantsCfg {
            no_hang: false,
            ..Default::default()
        };
        assert!(!kinds(&evaluate(&o, &cfg)).contains(&"no-hang".to_string()));
    }

    #[test]
    fn hang_message_renders_a_non_ms_unit_without_claiming_milliseconds() {
        // The TUI hang bucket is a count of ignored keypresses, not wall-clock ms;
        // the message must say so ("14 keypresses"), not "14ms".
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.obs.hangs.insert(
            ("home".to_string(), "key:Enter".to_string()),
            (14, "keypresses".to_string()),
        );
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-hang").unwrap();
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains(">= 14 keypresses"), "got: {msg}");
        assert!(!msg.contains("14ms"), "must not claim ms: {msg}");
    }

    #[test]
    fn permission_trap_predicate_flags_a_reached_sink() {
        // home -> advanced; advanced has NO outgoing edge: a sink (PLANTED-BUG 6).
        let o = obs_with(
            &[
                ("home", &["Go"]),
                ("advanced", &["Advanced", "Verbose logging"]),
            ],
            &[("home", "tap:Advanced", "advanced")],
            Some("home"),
        );
        let de = permission_traps(&o.obs);
        assert!(de.iter().any(|s| s == "advanced"));
        // home is not a dead end (it has a forward exit).
        assert!(!de.iter().any(|s| s == "home"));
    }

    #[test]
    fn back_only_exit_is_still_a_dead_end() {
        // advanced has only a `back` edge out: still a dead end (no forward exit).
        let o = obs_with(
            &[("home", &["Go"]), ("advanced", &["Advanced"])],
            &[
                ("home", "tap:Advanced", "advanced"),
                ("advanced", "back", "home"),
            ],
            Some("home"),
        );
        assert!(permission_traps(&o.obs).iter().any(|s| s == "advanced"));
    }

    #[test]
    fn single_url_distinct_screen_sink_is_a_dead_end() {
        // A single-URL app (JS section toggle, no hash/path change): the dashboard
        // and a content-only "Advanced" pane share route "/". The dashboard has a
        // forward exit and tappables; Advanced is a genuine sink (reached by an
        // action, no exit, no controls). Because every state shares the one route,
        // route-only suppression would wrongly excuse Advanced via the dashboard's
        // exit/tappables. Its labels are NOT a subset of the dashboard's, so the
        // label-aware predicate keeps it flagged. (Regression: corpus
        // dead-end-advanced went silent under route-only suppression.)
        let mut o = obs_with(
            &[
                (
                    "dashboard",
                    &["Dashboard", "Open advanced settings", "Refresh queue"],
                ),
                ("advanced", &["Advanced", "Nothing to configure yet."]),
            ],
            &[("dashboard", "tap:advanced", "advanced")],
            Some("dashboard"),
        );
        for s in ["dashboard", "advanced"] {
            o.obs.routes.insert(s.to_string(), "/".to_string());
        }
        assert!(
            permission_traps(&o.obs).iter().any(|s| s == "advanced"),
            "a distinct content-only screen sharing the URL is a real dead end"
        );
    }

    #[test]
    fn same_route_snapshots_are_not_dead_ends() {
        // A dynamic single-page site: one route "/" churns into three structural
        // snapshots as it animates. The walk ends at s2 (budget exhausted), which
        // has no recorded exit, but its same-route siblings s0/s1 DO, so s2 is an
        // animation artifact, not a sink. (Regression: the archastro.ai false
        // positive.)
        let mut o = obs_with(
            &[("s0", &["Home"]), ("s1", &["Home"]), ("s2", &["Home"])],
            &[("s0", "tap:link", "s1"), ("s1", "tap:link", "s2")],
            Some("s0"),
        );
        for s in ["s0", "s1", "s2"] {
            o.obs.routes.insert(s.to_string(), "/".to_string());
        }
        assert!(
            permission_traps(&o.obs).is_empty(),
            "no snapshot of an escapable single-page route should be a dead end"
        );
    }

    #[test]
    fn lone_start_state_with_no_edges_is_not_a_dead_end() {
        // The actual archastro.ai seed shape: the walk observed only the start
        // state and recorded no edge (it churned without a clean transition). An
        // unproductive walk is not a proven sink, so the landing page must not be
        // flagged. (A non-start reached sink still is: see no_dead_end_flags_a_sink_node.)
        let o = obs_with(&[("home", &["Home"])], &[], Some("home"));
        assert!(permission_traps(&o.obs).is_empty());
    }

    #[test]
    fn unexplored_leaf_with_untapped_nav_is_not_a_dead_end() {
        // A leaf reached as the budget terminus that still offers tappable nav the
        // walk never tapped (header links deduped after being tried elsewhere) is
        // not a trap. Regression: the cloud.google.com /blog/<article> dead-end FP.
        let mut o = obs_with(
            &[("home", &["Home"]), ("article", &["Cloud", "Blog"])],
            &[("home", "tap:role:link#0", "article")],
            Some("home"),
        );
        o.obs.tappables.insert("article".into(), 4); // offered 4 nav links, tapped 0
        assert!(!permission_traps(&o.obs).iter().any(|s| s == "article"));
    }

    #[test]
    fn exhausted_sink_with_all_tappables_tried_is_still_a_dead_end() {
        // The walk DID tap the screen's action and it self-looped (no forward
        // exit). Tappables exhausted -> a genuine sink, still flagged.
        let mut o = obs_with(
            &[("home", &["Home"]), ("trap", &["Stuck"])],
            &[
                ("home", "tap:role:link#0", "trap"),
                ("trap", "tap:role:button#0", "trap"),
            ],
            Some("home"),
        );
        o.obs.tappables.insert("trap".into(), 1); // offered 1, tapped 1 -> exhausted
        assert!(permission_traps(&o.obs).iter().any(|s| s == "trap"));
    }

    #[test]
    fn tui_exhausted_sink_counts_key_actions_as_tried() {
        // A TUI sink (forward actions are `key:*`, not `tap:`) that offered 2
        // elements and tried both via key presses, self-looping with no forward
        // exit, IS a genuine dead end. The old `tap:`-only count read the key
        // presses as untried (offered > tapped), so it suppressed every TUI sink
        // and the oracle never fired on the TUI despite being marked covered.
        let mut o = obs_with(
            &[("home", &["Home"]), ("trap", &["Stuck"])],
            &[
                ("home", "key:Enter", "trap"),
                ("trap", "key:Down", "trap"),
                ("trap", "key:Enter", "trap"),
            ],
            Some("home"),
        );
        o.obs.tappables.insert("trap".into(), 2); // offered 2, tried 2 keys -> exhausted
        let de = permission_traps(&o.obs);
        assert!(
            de.iter().any(|s| s == "trap"),
            "a TUI sink with all key actions tried must be a dead end: {de:?}"
        );
    }

    #[test]
    fn distinct_route_sink_is_still_a_dead_end() {
        // home (/) -> trap (/trap); trap has no exit AND its own route, so the
        // same-route suppression does not apply: still a real dead end.
        let mut o = obs_with(
            &[("home", &["Go"]), ("trap", &["Stuck"])],
            &[("home", "tap:Go", "trap")],
            Some("home"),
        );
        o.obs.routes.insert("home".into(), "/".into());
        o.obs.routes.insert("trap".into(), "/trap".into());
        assert!(permission_traps(&o.obs).iter().any(|s| s == "trap"));
    }

    #[test]
    fn no_exception_wraps_the_existing_exception_finding() {
        let mut o = obs_with(&[("home", &["Go"])], &[], Some("home"));
        o.exceptions = vec![json!({
            "kind": "EXCEPTION CAUGHT BY WIDGETS LIBRARY",
            "message": "A leaked AnimationController was found",
            "frames": ["package:bugzoo/main.dart:210:5"],
        })];
        let f = evaluate(&o, &InvariantsCfg::default());
        let v = f.iter().find(|x| x["invariant"] == "no-exception").unwrap();
        assert_eq!(v["kind"], "EXCEPTION CAUGHT BY WIDGETS LIBRARY");
        // Frames are preserved so the report still points at code.
        assert!(v["frames"][0].as_str().unwrap().contains("main.dart:210"));
    }

    #[test]
    fn no_jank_is_sim_only() {
        let mut o = obs_with(&[("feed", &["Feed"])], &[], Some("feed"));
        o.jank_by_sig.insert("feed".to_string(), 80.0);
        // Headless: jank reported but tier is not sim -> no finding.
        assert!(!evaluate(&o, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-jank"));
        // Sim: same data now fires.
        o.sim = true;
        assert!(evaluate(&o, &InvariantsCfg::default())
            .iter()
            .any(|x| x["invariant"] == "no-jank"));
    }

    #[test]
    fn custom_label_regex() {
        use crate::config::CustomInvariant;
        let cfg = InvariantsCfg {
            custom: vec![CustomInvariant {
                id: "settings-has-save".to_string(),
                scope: InvariantScope::State,
                labels_match: Some(regex::Regex::new("(?i)save").unwrap()),
                ..Default::default()
            }],
            ..Default::default()
        };
        // A state with no "Save" label violates the custom invariant.
        let o = obs_with(
            &[("settings", &["Profile", "Logout"])],
            &[],
            Some("settings"),
        );
        let f = evaluate(&o, &cfg);
        assert!(f.iter().any(|x| x["invariant"] == "settings-has-save"));
    }
}
