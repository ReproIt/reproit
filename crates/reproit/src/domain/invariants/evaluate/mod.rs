//! Ordered evaluation of built-in and custom invariant families. Each family
//! lives in its own file; a new oracle's evaluation lands in exactly one of
//! them (or a new family file when none fits).

mod edge;
mod graph;
mod lifecycle;
mod operability;
mod render;
mod transition;

use super::custom::eval_custom;
use super::Observations;
use crate::adapters::config::InvariantsCfg;
use serde_json::Value;

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
    let mut out = edge::evaluate_edge_invariants(obs, cfg);
    out.extend(render::evaluate_render_state_invariants(obs, cfg));
    out.extend(operability::evaluate_operability_state_invariants(obs, cfg));
    out.extend(transition::evaluate_transition_invariants(obs, cfg));
    out.extend(graph::evaluate_graph_invariants(obs, cfg));
    out.extend(lifecycle::evaluate_lifecycle_invariants(obs, cfg));
    for custom in &cfg.custom {
        out.extend(eval_custom(obs, custom));
    }
    out
}
