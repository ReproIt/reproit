//! INVARIANTS / PROPERTIES oracle (property-based testing).
//!
//! The earlier oracles (uncaught exception, jank threshold,
//! operability semantics) were ad-hoc checks scattered through
//! `modes/fuzz/mod.rs`. This module generalizes them into NAMED invariants
//! evaluated over a single run's observations (the parsed
//! `EXPLORE:STATE`/`EXPLORE:EDGE` records, plus the exception + perf findings
//! that the existing oracles already produce).
//!
//! Three scopes, all pure functions over a run's observations:
//!   - State invariants (node predicates): `no-jank` (sim), plus custom
//!     label-presence/absence regex.
//!   - Edge   invariants: `no-exception` (the existing exception oracle,
//!     named).
//!   - Graph  invariants: `no-occluded-control`, plus `no-leak` (reuse the
//!     soak/memory teardown signal when present). The general graph-sink oracle
//!     was removed as crawler-budget FP-prone; its sink predicate survives only
//!     for permission-walk.
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

#[cfg(test)]
use crate::adapters::config::{InvariantScope, InvariantsCfg};
use crate::domain::map::RunObs;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;

mod custom;
mod evaluate;
mod finding;
mod graph;
mod recheck;

pub use evaluate::evaluate;
#[allow(unused_imports)] // Preserve the existing finding-constructor façade for callers/tests.
pub use finding::{advisory_finding, finding};
#[cfg(test)]
use graph::permission_traps;
pub use recheck::{
    any_content_bug, any_detached_indicator, any_hang, any_jank, any_rerender_flicker,
    recheck_accessibility_state, recheck_content_bug, recheck_detached_indicator, recheck_hang,
    recheck_jank, recheck_overflow, recheck_rerender_flicker, GraphRecheck,
};

#[cfg(feature = "perf-bench")]
pub(crate) fn benchmark_permission_traps(obs: &RunObs) -> usize {
    graph::permission_traps(obs).len()
}

/// Everything the invariant set needs to evaluate one run. Built by the caller
/// from the per-seed log slice (+ the sim manifest, when on the sim tier).
pub struct Observations {
    /// Parsed `EXPLORE:STATE`/`EXPLORE:EDGE` records for this run.
    pub obs: RunObs,
    /// App exception findings already parsed (`ParsedRun` / `app_exceptions`):
    /// the `no-exception` edge oracle reuses these verbatim.
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

#[cfg(test)]
mod tests;
