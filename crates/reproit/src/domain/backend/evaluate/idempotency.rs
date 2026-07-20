use super::*;

pub(super) fn evaluate_idempotency(
    config: &BackendConfig,
    contracts: &BTreeMap<&str, &OperationContract>,
    invocations: &BTreeMap<(String, String), Invocation<'_>>,
    violations: &mut Vec<BackendViolation>,
) {
    let mut groups =
        BTreeMap::<(String, String, Option<String>, Option<String>), Vec<&Invocation<'_>>>::new();
    for invocation in invocations.values() {
        let Some(start) = invocation.start else {
            continue;
        };
        let Some(key) = start.idempotency_key.as_ref() else {
            continue;
        };
        let authored = config.invariants.iter().any(|invariant| {
            matches!(
                invariant,
                BackendInvariant::Idempotent { operation } if operation == &start.operation
            )
        });
        if authored
            || contracts
                .get(start.operation.as_str())
                .is_some_and(|contract| contract.idempotent)
        {
            groups
                .entry((
                    start.operation.clone(),
                    key.clone(),
                    start.actor.clone(),
                    start.tenant.clone(),
                ))
                .or_default()
                .push(invocation);
        }
    }
    for ((operation, _, _, _), group) in groups {
        let Some(contract) = contracts.get(operation.as_str()) else {
            continue;
        };
        let mut successful = group
            .into_iter()
            .filter(|invocation| {
                invocation
                    .returned
                    .as_ref()
                    .is_some_and(|r| contract.is_success(r))
            })
            .collect::<Vec<_>>();
        successful.sort_by_key(|invocation| {
            invocation
                .start
                .map(|start| start.sequence)
                .unwrap_or(u64::MAX)
        });
        if successful.len() < 2 {
            continue;
        }
        if idempotency_group_outcome(contract, &successful) == IdempotencyOutcome::Violation {
            let event = successful[1]
                .returned
                .as_ref()
                .expect("filtered return")
                .event;
            violations.push(violation(
                contract,
                event,
                "idempotency",
                "repeating the same idempotency key introduced a different persistent final effect"
                    .into(),
            ));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdempotencyOutcome {
    Violation,
    Satisfied,
    Abstain,
}

fn idempotency_group_outcome(
    contract: &OperationContract,
    successful: &[&Invocation<'_>],
) -> IdempotencyOutcome {
    if successful.len() < 2 {
        return IdempotencyOutcome::Abstain;
    }
    let inputs = successful
        .iter()
        .filter_map(|invocation| invocation.start)
        .map(|start| match &start.event {
            BackendEventKind::Start { input } => canonical_json(input),
            _ => String::new(),
        })
        .collect::<BTreeSet<_>>();
    if inputs.len() != 1 {
        // Reusing a key for different requests is caller behavior, not proof the
        // operation violated idempotency for an identical request.
        return IdempotencyOutcome::Abstain;
    }

    if contract.idempotency_response_replay == IdempotencyResponseReplay::Exact {
        let responses = successful
            .iter()
            .filter_map(|invocation| invocation.returned.as_ref())
            .map(|returned| {
                format!(
                    "{}:{}",
                    returned.status.unwrap_or_default(),
                    canonical_json(returned.output)
                )
            })
            .collect::<BTreeSet<_>>();
        if responses.len() > 1 {
            return IdempotencyOutcome::Violation;
        }
    }

    // Generic RFC idempotency is judged only over application-authored durable
    // resources. Incidental logs, counters, calls, and emitted events cannot
    // define the operation's intended final effect.
    let intended = contract
        .promised_effects
        .iter()
        .filter(|effect| matches!(effect.kind, EffectKind::Write | EffectKind::Delete))
        .filter_map(|effect| effect.resource.as_deref())
        .collect::<BTreeSet<_>>();
    if contract.authority != Authority::Declared || intended.is_empty() {
        return IdempotencyOutcome::Abstain;
    }
    if successful.iter().any(|invocation| {
        !invocation
            .returned
            .as_ref()
            .is_some_and(|returned| returned.effects_complete)
    }) {
        return IdempotencyOutcome::Abstain;
    }

    let Some(baseline) = persistent_final_effects(
        &successful[0].effects,
        &intended,
        successful[0]
            .start
            .and_then(|start| start.tenant.as_deref()),
    ) else {
        return IdempotencyOutcome::Abstain;
    };
    if baseline.is_empty() {
        return IdempotencyOutcome::Abstain;
    }
    for invocation in successful.iter().skip(1) {
        let Some(retry) = persistent_final_effects(
            &invocation.effects,
            &intended,
            invocation.start.and_then(|start| start.tenant.as_deref()),
        ) else {
            return IdempotencyOutcome::Abstain;
        };
        if retry
            .iter()
            .any(|(identity, value)| baseline.get(identity) != Some(value))
        {
            return IdempotencyOutcome::Violation;
        }
    }
    IdempotencyOutcome::Satisfied
}
