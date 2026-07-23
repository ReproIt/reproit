//! DATA-LOSS oracle (silent write loss / sibling corruption), pure over the
//! event sequence so discovery and replay share one code path.
//!
//! Pattern (the round-trip quad, constructed by the headless prober but
//! recognized structurally here): read R_a and read R_b of the SAME operation
//! with the SAME input (the volatility baseline), then exactly one successful
//! mutation M sharing a scalar identity value with the reads, then read R_c
//! (same operation and input as R_b). Zero-FP guards:
//!
//!   - VOLATILE fields (any top-level field that differed between R_a and
//!     R_b with no write in between: updatedAt, etags, counters) are excluded
//!     from every check. Self-baseline, never name heuristics.
//!   - SILENT WRITE LOSS fires only when the mutation was ACCEPTED (2xx), the
//!     requested value differed from the stored one, and the read-back value
//!     is BYTE-UNCHANGED. A server that canonicalizes (trim, case) returns a
//!     third value and abstains.
//!   - SIBLING CORRUPTION fires only for a non-volatile field the mutation
//!     did not touch whose value changed across M. With exactly one mutation
//!     between the baseline and the check read, nothing else can explain it.
//!   - Reasons carry FIELD NAMES only, never values (redaction by
//!     construction, and a value-free reason keeps the fingerprint stable).

use super::super::{BackendEvent, BackendEventKind, BackendViolation};
use super::violation;
use crate::domain::backend::operation::OperationContract;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_VIOLATIONS_PER_QUAD: usize = 5;

struct Invocation<'a> {
    event: &'a BackendEvent,
    operation: &'a str,
    input: &'a Value,
    output: &'a Value,
    success: bool,
}

/// Pair Start/Return events into ordered invocations.
fn invocations<'a>(events: &'a [BackendEvent]) -> Vec<Invocation<'a>> {
    let mut starts: BTreeMap<(&str, &str, u32), &BackendEvent> = BTreeMap::new();
    let mut out = Vec::new();
    for event in events {
        let key = (
            event.trace_id.as_str(),
            event.span_id.as_str(),
            event.action_index,
        );
        match &event.event {
            BackendEventKind::Start { .. } => {
                starts.insert(key, event);
            }
            BackendEventKind::Return {
                output, success, ..
            } => {
                if let Some(start) = starts.remove(&key) {
                    if let BackendEventKind::Start { input } = &start.event {
                        out.push(Invocation {
                            event: start,
                            operation: start.operation.as_str(),
                            input,
                            output,
                            success: *success,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Every scalar leaf value in a JSON tree (the identity-sharing test).
fn scalar_leaves(value: &Value, out: &mut Vec<Value>) {
    match value {
        Value::Object(map) => map.values().for_each(|v| scalar_leaves(v, out)),
        Value::Array(items) => items.iter().for_each(|v| scalar_leaves(v, out)),
        Value::Null => {}
        scalar => out.push(scalar.clone()),
    }
}

fn shares_identity(a: &Value, b: &Value) -> bool {
    let (mut la, mut lb) = (Vec::new(), Vec::new());
    scalar_leaves(a, &mut la);
    scalar_leaves(b, &mut lb);
    la.iter().any(|v| {
        !matches!(v, Value::Bool(_)) && v.as_str().is_none_or(|s| !s.is_empty()) && lb.contains(v)
    })
}

pub(super) fn evaluate_data_loss(
    contracts: &BTreeMap<&str, &OperationContract>,
    events: &[BackendEvent],
    violations: &mut Vec<BackendViolation>,
) {
    let invs = invocations(events);
    let read_only = |inv: &Invocation| -> bool {
        contracts
            .get(inv.operation)
            .is_some_and(|contract| contract.read_only)
    };
    for m in 0..invs.len() {
        let mutation = &invs[m];
        if read_only(mutation) || !mutation.success || !mutation.input.is_object() {
            continue;
        }
        // R_b: the nearest successful read before M with no mutation between.
        let Some(rb) = (0..m)
            .rev()
            .find(|&i| invs[i].success && read_only(&invs[i]))
        else {
            continue;
        };
        if (rb + 1..m).any(|i| !read_only(&invs[i])) {
            continue;
        }
        // R_a: an earlier identical read (same operation + input) as baseline,
        // with no mutation between R_a and R_b.
        let Some(ra) = (0..rb).rev().find(|&i| {
            invs[i].success
                && invs[i].operation == invs[rb].operation
                && invs[i].input == invs[rb].input
        }) else {
            continue;
        };
        if (ra + 1..rb).any(|i| !read_only(&invs[i])) {
            continue;
        }
        // R_c: the next identical read after M, with M the only mutation
        // between R_b and R_c.
        let Some(rc) = (m + 1..invs.len()).find(|&i| {
            invs[i].success
                && invs[i].operation == invs[rb].operation
                && invs[i].input == invs[rb].input
        }) else {
            continue;
        };
        if (m + 1..rc).any(|i| !read_only(&invs[i])) {
            continue;
        }
        if !shares_identity(invs[rb].input, mutation.input) {
            continue;
        }
        let (Some(oa), Some(ob), Some(oc)) = (
            invs[ra].output.as_object(),
            invs[rb].output.as_object(),
            invs[rc].output.as_object(),
        ) else {
            continue;
        };
        let contract = match contracts.get(mutation.operation) {
            Some(contract) => *contract,
            None => continue,
        };
        // Headless inputs group parameters as {"path":…, "body":…}; the write
        // payload is the body group when present, the whole object otherwise.
        let body = match mutation.input.get("body").and_then(Value::as_object) {
            Some(body) => body,
            None => mutation.input.as_object().expect("checked is_object"),
        };
        // Identity params (the id the reads also carry) are not writes.
        let mut read_input_leaves = Vec::new();
        scalar_leaves(invs[rb].input, &mut read_input_leaves);
        let patched: Vec<&String> = body
            .iter()
            .filter(|(key, value)| ob.contains_key(*key) && !read_input_leaves.contains(value))
            .map(|(key, _)| key)
            .collect();
        if patched.is_empty() {
            continue;
        }
        let volatile = |key: &str| oa.get(key) != ob.get(key);
        let mut emitted = 0usize;
        for field in &patched {
            let requested = &body[*field];
            let stored = &ob[*field];
            if requested.is_object() || requested.is_array() {
                continue;
            }
            if !volatile(field)
                && requested != stored
                && oc.get(*field) == Some(stored)
                && emitted < MAX_VIOLATIONS_PER_QUAD
            {
                emitted += 1;
                violations.push(violation(
                    contract,
                    mutation.event,
                    "data-loss",
                    format!(
                        "accepted write to `{field}` read back unchanged: the \
                         mutation returned success but the value was silently \
                         dropped"
                    ),
                ));
            }
        }
        for (key, before) in ob {
            if patched.contains(&key) || volatile(key) {
                continue;
            }
            if oc.get(key) != Some(before) && emitted < MAX_VIOLATIONS_PER_QUAD {
                emitted += 1;
                violations.push(violation(
                    contract,
                    mutation.event,
                    "data-loss",
                    format!(
                        "mutation of {patched:?} changed the unrelated field \
                         `{key}`: sibling state was corrupted or lost"
                    ),
                ));
            }
        }
    }
}
