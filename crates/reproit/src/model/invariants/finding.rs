//! Construction of invariant findings in the shared report shape.

use serde_json::{json, Value};

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

/// Like `finding`, but marks it ADVISORY: a real signal that is NOT
/// deterministic across environments (a raw-pixel or otherwise
/// environment-relative measure), so it is reported for information but never
/// counted as a verdict-bearing, replayable repro. This keeps reproit's
/// "reproduces on any machine" promise honest: only deterministic findings
/// become repros; pixel signals inform.
pub fn advisory_finding(invariant: &str, kind: &str, message: String, sig: Option<&str>) -> Value {
    let mut f = finding(invariant, kind, message, sig);
    f["advisory"] = Value::Bool(true);
    f
}
