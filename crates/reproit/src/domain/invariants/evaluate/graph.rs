//! Graph invariants: violations over the explored screen graph.

use crate::adapters::config::InvariantsCfg;
use crate::domain::invariants::finding::finding;
use crate::domain::invariants::Observations;
use serde_json::{json, Value};

pub(super) fn evaluate_graph_invariants(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
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
