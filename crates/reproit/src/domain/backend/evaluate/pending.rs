//! Pending-obligation summaries for interactive inspection.
//!
//! `pending_obligations` is a pure, bounded companion to `evaluate`: over the
//! same accumulated event prefix it names, in plain language, the predicates
//! that are mid-accumulation (a write awaiting its read-back, an idempotency
//! pair half-matched, ...). It never produces a verdict and never changes
//! `evaluate`'s behavior; inspection uses it so a human can see which oracle
//! state each step advanced.

use super::*;

const MAX_PENDING: usize = 32;

/// One oracle predicate that has consumed evidence but not yet resolved to a
/// violation, a satisfaction, or an abstention over the current prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingObligation {
    /// The evaluate-family check the obligation belongs to (`data-loss`, ...).
    pub oracle: &'static str,
    /// Plain-language description of the accumulated state and what evidence
    /// would complete the check. Never contains payload values.
    pub summary: String,
}

struct Paired<'a> {
    operation: &'a str,
    input: &'a Value,
    start_sequence: u64,
    return_sequence: u64,
    success: bool,
    actor: Option<&'a str>,
    tenant: Option<&'a str>,
}

/// Start/Return pairs in stream order, plus the set of open (unreturned)
/// operations.
fn pair_invocations<'a>(events: &'a [BackendEvent]) -> (Vec<Paired<'a>>, Vec<&'a str>) {
    let mut starts: BTreeMap<(&str, &str), &BackendEvent> = BTreeMap::new();
    let mut paired = Vec::new();
    for event in events {
        let key = (event.trace_id.as_str(), event.span_id.as_str());
        match &event.event {
            BackendEventKind::Start { .. } => {
                starts.insert(key, event);
            }
            BackendEventKind::Return { success, .. } => {
                if let Some(start) = starts.remove(&key) {
                    if let BackendEventKind::Start { input } = &start.event {
                        paired.push(Paired {
                            operation: start.operation.as_str(),
                            input,
                            start_sequence: start.sequence,
                            return_sequence: event.sequence,
                            success: *success,
                            actor: start.actor.as_deref(),
                            tenant: start.tenant.as_deref(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let open = starts
        .into_values()
        .map(|event| event.operation.as_str())
        .collect();
    (paired, open)
}

pub fn pending_obligations(
    config: &BackendConfig,
    events: &[BackendEvent],
) -> Vec<PendingObligation> {
    if !config.enabled {
        return Vec::new();
    }
    let contracts = select_contracts(config);
    let (paired, open) = pair_invocations(events);
    let mut out = Vec::new();
    for operation in open {
        out.push(PendingObligation {
            oracle: "in-flight",
            summary: format!("operation `{operation}` started and has not returned yet"),
        });
    }
    idempotency_pending(config, &contracts, events, &mut out);
    data_loss_pending(&contracts, &paired, &mut out);
    lifecycle_pending(config, &paired, &mut out);
    concurrency_pending(config, &contracts, &paired, &mut out);
    out.truncate(MAX_PENDING);
    out
}

/// One successful call with an idempotency key on an idempotent operation is
/// half of the pair the idempotency check needs.
fn idempotency_pending(
    config: &BackendConfig,
    contracts: &BTreeMap<&str, &OperationContract>,
    events: &[BackendEvent],
    out: &mut Vec<PendingObligation>,
) {
    let mut groups = BTreeMap::<(&str, &str, Option<&str>, Option<&str>), usize>::new();
    let mut successes: BTreeMap<(&str, &str), bool> = BTreeMap::new();
    for event in events {
        if let BackendEventKind::Return { success, .. } = &event.event {
            successes.insert((event.trace_id.as_str(), event.span_id.as_str()), *success);
        }
    }
    for event in events {
        let BackendEventKind::Start { .. } = &event.event else {
            continue;
        };
        let Some(key) = event.idempotency_key.as_deref() else {
            continue;
        };
        let authored = config.invariants.iter().any(|invariant| {
            matches!(
                invariant,
                BackendInvariant::Idempotent { operation } if operation == &event.operation
            )
        });
        let declared = contracts
            .get(event.operation.as_str())
            .is_some_and(|contract| contract.idempotent);
        let returned_ok = successes
            .get(&(event.trace_id.as_str(), event.span_id.as_str()))
            .copied()
            .unwrap_or(false);
        if (authored || declared) && returned_ok {
            *groups
                .entry((
                    event.operation.as_str(),
                    key,
                    event.actor.as_deref(),
                    event.tenant.as_deref(),
                ))
                .or_default() += 1;
        }
    }
    for ((operation, key, _, _), count) in groups {
        if count == 1 {
            let key: String = key.chars().take(32).collect();
            out.push(PendingObligation {
                oracle: "idempotency",
                summary: format!(
                    "idempotency pair half-matched: one successful `{operation}` with key \
                     `{key}` observed; a repeat with the same key must reproduce the same \
                     persistent final effect"
                ),
            });
        }
    }
}

/// The data-loss quad minus its closing read: baseline reads plus an accepted
/// mutation sharing identity with them means the next identical read decides.
fn data_loss_pending(
    contracts: &BTreeMap<&str, &OperationContract>,
    paired: &[Paired<'_>],
    out: &mut Vec<PendingObligation>,
) {
    let read_only = |invocation: &Paired<'_>| -> bool {
        contracts
            .get(invocation.operation)
            .is_some_and(|contract| contract.read_only)
    };
    for m in (0..paired.len()).rev() {
        let mutation = &paired[m];
        if read_only(mutation) || !mutation.success || !mutation.input.is_object() {
            continue;
        }
        let Some(rb) = (0..m)
            .rev()
            .find(|&i| paired[i].success && read_only(&paired[i]))
        else {
            continue;
        };
        if (rb + 1..m).any(|i| !read_only(&paired[i])) {
            continue;
        }
        let Some(ra) = (0..rb).rev().find(|&i| {
            paired[i].success
                && paired[i].operation == paired[rb].operation
                && paired[i].input == paired[rb].input
        }) else {
            continue;
        };
        if (ra + 1..rb).any(|i| !read_only(&paired[i])) {
            continue;
        }
        let read_back_seen = (m + 1..paired.len()).any(|i| {
            paired[i].success
                && paired[i].operation == paired[rb].operation
                && paired[i].input == paired[rb].input
        });
        if read_back_seen {
            continue;
        }
        out.push(PendingObligation {
            oracle: "data-loss",
            summary: format!(
                "write awaiting read-back: `{}` accepted a mutation after a `{}` baseline; \
                 the next `{}` with the same input decides the data-loss check",
                mutation.operation, paired[rb].operation, paired[rb].operation
            ),
        });
        return;
    }
}

/// A successful lifecycle create with no later successful read leaves the
/// resource-lifecycle check waiting on its read-back.
fn lifecycle_pending(
    config: &BackendConfig,
    paired: &[Paired<'_>],
    out: &mut Vec<PendingObligation>,
) {
    for resource in &config.resources {
        let Some(create_index) = paired
            .iter()
            .rposition(|call| call.success && call.operation == resource.create.operation)
        else {
            continue;
        };
        let read_after = paired[create_index + 1..]
            .iter()
            .any(|call| call.success && call.operation == resource.read.operation);
        if !read_after {
            out.push(PendingObligation {
                oracle: "resource-lifecycle",
                summary: format!(
                    "resource `{}` created via `{}`; its `{}` read-back is pending",
                    resource.name, resource.create.operation, resource.read.operation
                ),
            });
        }
    }
}

/// A concurrent-update proof with calls observed but no overlapping pair by
/// distinct actors yet is an open multi-actor group.
fn concurrency_pending(
    config: &BackendConfig,
    contracts: &BTreeMap<&str, &OperationContract>,
    paired: &[Paired<'_>],
    out: &mut Vec<PendingObligation>,
) {
    for proof in &config.proofs {
        let BackendProofContract::ConcurrentUpdate {
            operation,
            consistency,
            ..
        } = proof
        else {
            continue;
        };
        if *consistency != ResourceConsistency::Strong
            || !contracts.contains_key(operation.as_str())
        {
            continue;
        }
        let calls: Vec<&Paired<'_>> = paired
            .iter()
            .filter(|call| {
                call.success
                    && call.operation == operation.as_str()
                    && call.actor.is_some()
                    && call.tenant.is_some()
            })
            .collect();
        if calls.is_empty() {
            continue;
        }
        let paired_up = calls.iter().enumerate().any(|(index, left)| {
            calls.iter().skip(index + 1).any(|right| {
                left.actor != right.actor
                    && left.start_sequence < right.return_sequence
                    && right.start_sequence < left.return_sequence
            })
        });
        if !paired_up {
            out.push(PendingObligation {
                oracle: "concurrent-update",
                summary: format!(
                    "concurrent-update group open: {} call(s) to `{operation}` observed; an \
                     overlapping call by a different actor completes the check",
                    calls.len()
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(sequence: u64, span: &str, operation: &str, kind: Value) -> BackendEvent {
        let mut base = json!({
            "sequence": sequence,
            "traceId": "t",
            "spanId": span,
            "operation": operation,
        });
        base.as_object_mut()
            .expect("object")
            .extend(kind.as_object().expect("object").clone());
        serde_json::from_value(base).expect("test event")
    }

    fn contract(id: &str, read_only: bool, idempotent: bool) -> OperationContract {
        OperationContract {
            id: id.into(),
            authority: Authority::Declared,
            input: Some(ValueDomain::Any),
            output: None,
            outputs_by_status: BTreeMap::new(),
            success_statuses: Vec::new(),
            read_only,
            idempotent,
            idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
            tenant_isolated: false,
            promised_effects: Vec::new(),
        }
    }

    fn config(operations: Vec<OperationContract>) -> BackendConfig {
        BackendConfig {
            enabled: true,
            operations,
            ..BackendConfig::default()
        }
    }

    #[test]
    fn open_invocation_and_half_matched_idempotency_pair_are_pending() {
        let config = config(vec![contract("pay", false, true)]);
        let mut start = event(
            1,
            "a",
            "pay",
            json!({"kind": "start", "input": {"amount": 5}}),
        );
        start.idempotency_key = Some("key-1".into());
        let done = event(
            2,
            "a",
            "pay",
            json!({"kind": "return", "output": {}, "status": 200, "success": true}),
        );
        let open = event(3, "b", "pay", json!({"kind": "start", "input": {}}));
        let pending = pending_obligations(&config, &[start, done, open]);
        assert!(pending
            .iter()
            .any(|item| item.oracle == "in-flight" && item.summary.contains("`pay`")));
        assert!(pending
            .iter()
            .any(|item| item.oracle == "idempotency" && item.summary.contains("key-1")));
    }

    #[test]
    fn accepted_write_after_a_read_baseline_awaits_its_read_back() {
        let config = config(vec![
            contract("getNote", true, false),
            contract("patchNote", false, false),
        ]);
        let read = |sequence, span| {
            [
                event(
                    sequence,
                    span,
                    "getNote",
                    json!({"kind": "start", "input": {"id": "n1"}}),
                ),
                event(
                    sequence + 1,
                    span,
                    "getNote",
                    json!({"kind": "return", "output": {"title": "a"}, "success": true}),
                ),
            ]
        };
        let mut events: Vec<BackendEvent> = Vec::new();
        events.extend(read(1, "r1"));
        events.extend(read(3, "r2"));
        events.push(event(
            5,
            "w",
            "patchNote",
            json!({"kind": "start", "input": {"id": "n1", "title": "b"}}),
        ));
        events.push(event(
            6,
            "w",
            "patchNote",
            json!({"kind": "return", "output": {}, "success": true}),
        ));
        let pending = pending_obligations(&config, &events);
        assert!(
            pending
                .iter()
                .any(|item| item.oracle == "data-loss" && item.summary.contains("read-back")),
            "pending: {pending:?}"
        );
        // The closing read resolves the obligation.
        events.extend(read(7, "r3"));
        let resolved = pending_obligations(&config, &events);
        assert!(!resolved.iter().any(|item| item.oracle == "data-loss"));
    }

    #[test]
    fn concurrent_update_group_stays_open_until_a_second_actor_overlaps() {
        let mut config = config(vec![contract("updateDoc", false, false)]);
        config.proofs = vec![BackendProofContract::ConcurrentUpdate {
            operation: "updateDoc".into(),
            identity_input_path: "$.id".into(),
            snapshot_input_path: "$.id".into(),
            consistency: ResourceConsistency::Strong,
            policy: ConcurrencyPolicy::OptimisticVersion {
                resource: "docs".into(),
                version_input_path: "$.version".into(),
                conflict_statuses: vec![409],
            },
        }];
        let call = |sequence: u64, span: &str, actor: &str| {
            let mut start = event(
                sequence,
                span,
                "updateDoc",
                json!({"kind": "start", "input": {"id": "d1", "version": 1}}),
            );
            start.actor = Some(actor.into());
            start.tenant = Some("acme".into());
            let done = event(
                sequence + 3,
                span,
                "updateDoc",
                json!({"kind": "return", "output": {}, "status": 200, "success": true}),
            );
            [start, done]
        };
        let solo: Vec<BackendEvent> = call(1, "a", "alice").into();
        let pending = pending_obligations(&config, &solo);
        assert!(pending
            .iter()
            .any(|item| item.oracle == "concurrent-update"));

        let mut both: Vec<BackendEvent> = call(1, "a", "alice").into();
        both.extend(call(2, "b", "bob")); // sequences 2..5 overlap 1..4
        let resolved = pending_obligations(&config, &both);
        assert!(!resolved
            .iter()
            .any(|item| item.oracle == "concurrent-update"));
    }
}
