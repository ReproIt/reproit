//! Transition invariants: violations across a single action.

use super::metric;
use crate::adapters::config::InvariantsCfg;
use crate::domain::invariants::finding::finding;
use crate::domain::invariants::Observations;
use serde_json::Value;

pub(super) fn evaluate_transition_invariants(
    obs: &Observations,
    cfg: &InvariantsCfg,
) -> Vec<Value> {
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
