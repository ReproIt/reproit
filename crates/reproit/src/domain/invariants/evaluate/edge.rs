//! Edge invariants: per-action exception/jank/hang violations.

use crate::adapters::config::InvariantsCfg;
use crate::domain::invariants::finding::advisory_finding;
use crate::domain::invariants::Observations;
use serde_json::{json, Value};

pub(super) fn evaluate_edge_invariants(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
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
