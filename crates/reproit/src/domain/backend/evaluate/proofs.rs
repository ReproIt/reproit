use super::*;

#[derive(Debug, Clone, PartialEq)]
enum ProofOutcome<'a> {
    Violation {
        event: &'a BackendEvent,
        oracle: &'static str,
        reason: String,
    },
    Satisfied,
    Abstain,
}

pub(super) fn evaluate_proof_contracts<'a>(
    config: &'a BackendConfig,
    contracts: &BTreeMap<&str, &'a OperationContract>,
    invocations: &BTreeMap<(String, String), Invocation<'a>>,
    violations: &mut Vec<BackendViolation>,
) {
    for proof in &config.proofs {
        let outcome = match proof {
            BackendProofContract::AuthorizationMatrix { .. } => {
                authorization_outcome(proof, contracts, invocations)
            }
            BackendProofContract::TransactionAtomicity { .. } => {
                atomicity_outcome(proof, contracts, invocations)
            }
            BackendProofContract::ConcurrentUpdate { .. } => {
                concurrency_outcome(proof, contracts, invocations)
            }
            BackendProofContract::ResourceRoundTrip { .. } => {
                round_trip_outcome(proof, contracts, invocations)
            }
            BackendProofContract::CodecRoundTrip { .. } => {
                codec_round_trip_outcome(proof, contracts, invocations)
            }
        };
        let ProofOutcome::Violation {
            event,
            oracle,
            reason,
        } = outcome
        else {
            continue;
        };
        let Some(contract) = contracts.get(event.operation.as_str()) else {
            continue;
        };
        violations.push(proof_violation(contract, proof, event, oracle, reason));
    }
}

fn codec_round_trip_outcome<'a>(
    proof: &BackendProofContract,
    contracts: &BTreeMap<&str, &'a OperationContract>,
    invocations: &'a BTreeMap<(String, String), Invocation<'a>>,
) -> ProofOutcome<'a> {
    let BackendProofContract::CodecRoundTrip {
        operation,
        projections,
    } = proof
    else {
        return ProofOutcome::Abstain;
    };
    let Some(contract) = contracts.get(operation.as_str()) else {
        return ProofOutcome::Abstain;
    };
    if contract.authority != Authority::Declared || projections.is_empty() {
        return ProofOutcome::Abstain;
    }
    let mut observed_complete = false;
    for invocation in proof_invocations(operation, invocations) {
        let (Some(input), Some(returned)) =
            (invocation_input(invocation), invocation.returned.as_ref())
        else {
            continue;
        };
        if !contract.is_success(returned) {
            continue;
        }
        let mut complete = true;
        for projection in projections {
            let (Some(expected), Some(actual)) = (
                json_path(input, &projection.input_path),
                json_path(returned.output, &projection.output_path),
            ) else {
                complete = false;
                break;
            };
            if contains_redacted(expected) || contains_redacted(actual) {
                complete = false;
                break;
            }
            if expected != actual {
                return ProofOutcome::Violation {
                    event: returned.event,
                    oracle: "codec-round-trip",
                    reason: format!(
                        "decoded value at {} contradicted the authored input projection {}",
                        projection.output_path, projection.input_path
                    ),
                };
            }
        }
        observed_complete |= complete;
    }
    if observed_complete {
        ProofOutcome::Satisfied
    } else {
        ProofOutcome::Abstain
    }
}

fn proof_invocations<'a>(
    operation: &str,
    invocations: &'a BTreeMap<(String, String), Invocation<'a>>,
) -> Vec<&'a Invocation<'a>> {
    let mut selected = invocations
        .values()
        .filter(|invocation| {
            invocation
                .start
                .is_some_and(|start| start.operation == operation)
                && invocation.returned.is_some()
        })
        .collect::<Vec<_>>();
    selected.sort_by_key(|invocation| invocation.start.map(|start| start.sequence));
    selected
}

fn invocation_input<'a>(invocation: &'a Invocation<'a>) -> Option<&'a Value> {
    invocation.start.and_then(|start| match &start.event {
        BackendEventKind::Start { input } => Some(input),
        _ => None,
    })
}

fn authored_identity<'a>(invocation: &'a Invocation<'a>, path: &str) -> Option<&'a Value> {
    scalar_at(invocation_input(invocation)?, path)
}

fn authorization_outcome<'a>(
    proof: &BackendProofContract,
    contracts: &BTreeMap<&str, &'a OperationContract>,
    invocations: &'a BTreeMap<(String, String), Invocation<'a>>,
) -> ProofOutcome<'a> {
    let BackendProofContract::AuthorizationMatrix {
        operation,
        identity_input_path,
        snapshot_input_path,
        consistency,
        principals,
        deny,
    } = proof
    else {
        return ProofOutcome::Abstain;
    };
    let Some(contract) = contracts.get(operation.as_str()) else {
        return ProofOutcome::Abstain;
    };
    if *consistency != ResourceConsistency::Strong
        || principals.is_empty()
        || (deny.statuses.is_empty() && deny.redacted_output_paths.is_empty())
    {
        return ProofOutcome::Abstain;
    }
    let calls = proof_invocations(operation, invocations);
    let mut observed_valid = false;
    for allowed_rule in principals
        .iter()
        .filter(|rule| rule.decision == AuthorizationDecision::Allow)
    {
        for denied_rule in principals
            .iter()
            .filter(|rule| rule.decision == AuthorizationDecision::Deny)
        {
            for allowed in calls.iter().filter(|invocation| {
                invocation.start.is_some_and(|start| {
                    start.actor.as_deref() == Some(allowed_rule.actor.as_str())
                        && start.tenant.as_deref() == Some(allowed_rule.tenant.as_str())
                }) && invocation
                    .returned
                    .as_ref()
                    .is_some_and(|returned| contract.is_success(returned))
            }) {
                for denied in calls.iter().filter(|invocation| {
                    invocation.start.is_some_and(|start| {
                        start.actor.as_deref() == Some(denied_rule.actor.as_str())
                            && start.tenant.as_deref() == Some(denied_rule.tenant.as_str())
                    })
                }) {
                    let Some(allowed_identity) = authored_identity(allowed, identity_input_path)
                    else {
                        continue;
                    };
                    let Some(denied_identity) = authored_identity(denied, identity_input_path)
                    else {
                        continue;
                    };
                    let Some(allowed_snapshot) = authored_identity(allowed, snapshot_input_path)
                    else {
                        continue;
                    };
                    let Some(denied_snapshot) = authored_identity(denied, snapshot_input_path)
                    else {
                        continue;
                    };
                    if allowed_identity != denied_identity
                        || allowed_snapshot != denied_snapshot
                        || invocation_input(allowed) != invocation_input(denied)
                    {
                        continue;
                    }
                    let Some(returned) = denied.returned.as_ref() else {
                        continue;
                    };
                    if returned
                        .status
                        .is_some_and(|status| deny.statuses.contains(&status))
                        && !returned.success
                    {
                        observed_valid = true;
                        continue;
                    }
                    if contract.is_success(returned) {
                        if !deny.redacted_output_paths.is_empty()
                            && deny.redacted_output_paths.iter().all(|path| {
                                json_path(returned.output, path).is_none_or(Value::is_null)
                            })
                        {
                            observed_valid = true;
                            continue;
                        }
                        return ProofOutcome::Violation {
                            event: returned.event,
                            oracle: "authorization-matrix",
                            reason: "an authored denied principal received protected resource \
                                     data for the same identity and snapshot"
                                .into(),
                        };
                    }
                }
            }
        }
    }
    if observed_valid {
        ProofOutcome::Satisfied
    } else {
        ProofOutcome::Abstain
    }
}

fn atomicity_outcome<'a>(
    proof: &BackendProofContract,
    contracts: &BTreeMap<&str, &'a OperationContract>,
    invocations: &'a BTreeMap<(String, String), Invocation<'a>>,
) -> ProofOutcome<'a> {
    let BackendProofContract::TransactionAtomicity {
        operation,
        identity_input_path,
        snapshot_input_path,
        consistency,
        failure,
        durable_effects,
    } = proof
    else {
        return ProofOutcome::Abstain;
    };
    let Some(contract) = contracts.get(operation.as_str()) else {
        return ProofOutcome::Abstain;
    };
    if contract.authority != Authority::Declared
        || *consistency != ResourceConsistency::Strong
        || failure.statuses.is_empty()
        || durable_effects.is_empty()
        || durable_effects.iter().any(|effect| {
            !matches!(effect.kind, EffectKind::Write | EffectKind::Delete)
                || effect.resource.is_none()
                || effect.event.is_some()
        })
    {
        return ProofOutcome::Abstain;
    }
    let mut observed_valid = false;
    for invocation in proof_invocations(operation, invocations) {
        if authored_identity(invocation, identity_input_path).is_none()
            || authored_identity(invocation, snapshot_input_path).is_none()
            || invocation_input(invocation).and_then(|input| json_path(input, &failure.input_path))
                != Some(&failure.value)
        {
            continue;
        }
        let Some(returned) = invocation.returned.as_ref() else {
            continue;
        };
        if returned.success
            || !returned
                .status
                .is_some_and(|status| failure.statuses.contains(&status))
            || !returned.effects_complete
        {
            continue;
        }
        match failed_atomicity_effect_outcome(invocation, durable_effects) {
            AtomicityEffectOutcome::Violation(effect) => {
                return ProofOutcome::Violation {
                    event: effect.event,
                    oracle: "transaction-atomicity",
                    reason: "a failed authored operation left a declared durable effect different \
                             from its exact before value"
                        .into(),
                };
            }
            AtomicityEffectOutcome::Satisfied => observed_valid = true,
            AtomicityEffectOutcome::Abstain => continue,
        }
    }
    if observed_valid {
        ProofOutcome::Satisfied
    } else {
        ProofOutcome::Abstain
    }
}

#[derive(Clone, Copy)]
pub(in crate::domain::backend) enum AtomicityEffectOutcome<'a> {
    Violation(&'a EffectEvent<'a>),
    Satisfied,
    Abstain,
}

/// Evaluate the complete local mutation stream of a failed authored operation.
///
/// Absence satisfies the contract because `effectsComplete` proves that the adapter observed
/// every local effect. Once a declared mutation is present, its resource key
/// and exact first `before` value become the baseline. Every later mutation of
/// that resource/key must form a contiguous snapshot chain. A final value equal
/// to the baseline proves rollback; a different final value proves a partial
/// commit. Missing snapshots or a broken chain abstain.
pub(in crate::domain::backend) fn failed_atomicity_effect_outcome<'a>(
    invocation: &'a Invocation<'a>,
    durable_effects: &[EffectPattern],
) -> AtomicityEffectOutcome<'a> {
    let mut declared_targets = BTreeSet::new();
    for pattern in durable_effects {
        let Some(resource) = pattern.resource.as_deref() else {
            return AtomicityEffectOutcome::Abstain;
        };
        declared_targets.insert((pattern.kind, resource));
    }

    let mut groups = BTreeMap::<(&str, &str), Vec<&EffectEvent<'_>>>::new();
    for effect in invocation.effects.iter().filter(|effect| {
        matches!(effect.effect, EffectKind::Write | EffectKind::Delete)
            && effect.resource.is_some_and(|resource| {
                declared_targets
                    .iter()
                    .any(|(_, target)| *target == resource)
            })
    }) {
        let (Some(resource), Some(key)) = (effect.resource, effect.key) else {
            return AtomicityEffectOutcome::Abstain;
        };
        groups.entry((resource, key)).or_default().push(effect);
    }

    let mut proven_effect = None;
    for effects in groups.values_mut() {
        effects.sort_by_key(|effect| effect.event.sequence);
        let Some(first_declared) = effects.iter().position(|effect| {
            let resource = effect.resource.expect("grouped effects have resources");
            declared_targets.contains(&(effect.effect, resource))
        }) else {
            continue;
        };
        let effects = &effects[first_declared..];
        let Some(baseline) = effects[0].before else {
            return AtomicityEffectOutcome::Abstain;
        };
        let mut current = Some(baseline);
        for effect in effects {
            if effect.before != current {
                return AtomicityEffectOutcome::Abstain;
            }
            current = match effect.effect {
                EffectKind::Write => {
                    let Some(after) = effect.after else {
                        return AtomicityEffectOutcome::Abstain;
                    };
                    Some(after)
                }
                EffectKind::Delete => None,
                _ => return AtomicityEffectOutcome::Abstain,
            };
        }
        if current != Some(baseline) {
            proven_effect.get_or_insert(effects[effects.len() - 1]);
        }
    }

    proven_effect.map_or(
        AtomicityEffectOutcome::Satisfied,
        AtomicityEffectOutcome::Violation,
    )
}

fn invocations_overlap(left: &Invocation<'_>, right: &Invocation<'_>) -> bool {
    let (Some(left_start), Some(left_return), Some(right_start), Some(right_return)) = (
        left.start.map(|event| event.sequence),
        left.returned
            .as_ref()
            .map(|returned| returned.event.sequence),
        right.start.map(|event| event.sequence),
        right
            .returned
            .as_ref()
            .map(|returned| returned.event.sequence),
    ) else {
        return false;
    };
    left_start < right_return && right_start < left_return
}

fn matching_write_effect<'a>(
    invocation: &'a Invocation<'a>,
    resource: &str,
) -> Option<&'a EffectEvent<'a>> {
    let tenant = invocation.start?.tenant.as_deref()?;
    let mut effects = invocation.effects.iter().filter(|effect| {
        effect.effect == EffectKind::Write
            && effect.resource == Some(resource)
            && effect.tenant == Some(tenant)
            && effect.key.is_some()
    });
    let effect = effects.next()?;
    effects.next().is_none().then_some(effect)
}

fn concurrency_outcome<'a>(
    proof: &BackendProofContract,
    contracts: &BTreeMap<&str, &'a OperationContract>,
    invocations: &'a BTreeMap<(String, String), Invocation<'a>>,
) -> ProofOutcome<'a> {
    let BackendProofContract::ConcurrentUpdate {
        operation,
        identity_input_path,
        snapshot_input_path,
        consistency,
        policy,
    } = proof
    else {
        return ProofOutcome::Abstain;
    };
    let Some(contract) = contracts.get(operation.as_str()) else {
        return ProofOutcome::Abstain;
    };
    if *consistency != ResourceConsistency::Strong {
        return ProofOutcome::Abstain;
    }
    let calls = proof_invocations(operation, invocations);
    let mut observed_valid = false;
    for (index, left) in calls.iter().enumerate() {
        for right in calls.iter().skip(index + 1) {
            let (Some(left_start), Some(right_start)) = (left.start, right.start) else {
                continue;
            };
            if !invocations_overlap(left, right)
                || left_start.actor.is_none()
                || right_start.actor.is_none()
                || left_start.actor == right_start.actor
                || left_start.tenant.is_none()
                || left_start.tenant != right_start.tenant
                || authored_identity(left, identity_input_path)
                    != authored_identity(right, identity_input_path)
                || authored_identity(left, identity_input_path).is_none()
                || authored_identity(left, snapshot_input_path)
                    != authored_identity(right, snapshot_input_path)
                || authored_identity(left, snapshot_input_path).is_none()
            {
                continue;
            }
            let (Some(left_return), Some(right_return)) =
                (left.returned.as_ref(), right.returned.as_ref())
            else {
                continue;
            };
            if !left_return.effects_complete || !right_return.effects_complete {
                continue;
            }
            match policy {
                ConcurrencyPolicy::OptimisticVersion {
                    resource,
                    version_input_path,
                    conflict_statuses,
                } => {
                    if conflict_statuses.is_empty()
                        || authored_identity(left, version_input_path)
                            != authored_identity(right, version_input_path)
                        || authored_identity(left, version_input_path).is_none()
                    {
                        continue;
                    }
                    let left_success = contract.is_success(left_return);
                    let right_success = contract.is_success(right_return);
                    if left_success && right_success {
                        let (Some(left_effect), Some(right_effect)) = (
                            matching_write_effect(left, resource),
                            matching_write_effect(right, resource),
                        ) else {
                            continue;
                        };
                        if left_effect.key != right_effect.key {
                            continue;
                        }
                        return ProofOutcome::Violation {
                            event: right_return.event,
                            oracle: "concurrent-update",
                            reason: "two overlapping updates with the same authored version both \
                                     committed to the same resource identity"
                                .into(),
                        };
                    }
                    if left_success ^ right_success {
                        let failure = if left_success {
                            right_return
                        } else {
                            left_return
                        };
                        if failure
                            .status
                            .is_some_and(|status| conflict_statuses.contains(&status))
                        {
                            observed_valid = true;
                        }
                    }
                }
                ConcurrencyPolicy::Conserved {
                    resource,
                    delta_input_path,
                    before_path,
                    after_path,
                } => {
                    if !contract.is_success(left_return) || !contract.is_success(right_return) {
                        continue;
                    }
                    let (Some(left_input), Some(right_input)) =
                        (invocation_input(left), invocation_input(right))
                    else {
                        continue;
                    };
                    let (Some(left_delta), Some(right_delta)) = (
                        json_path(left_input, delta_input_path).and_then(Value::as_i64),
                        json_path(right_input, delta_input_path).and_then(Value::as_i64),
                    ) else {
                        continue;
                    };
                    let (Some(left_effect), Some(right_effect)) = (
                        matching_write_effect(left, resource),
                        matching_write_effect(right, resource),
                    ) else {
                        continue;
                    };
                    if left_effect.key != right_effect.key {
                        continue;
                    }
                    let mut ordered = [(left_effect, left_delta), (right_effect, right_delta)];
                    ordered.sort_by_key(|(effect, _)| effect.event.sequence);
                    let Some(mut current) = ordered[0]
                        .0
                        .before
                        .and_then(|value| json_path(value, before_path))
                        .and_then(Value::as_i64)
                    else {
                        continue;
                    };
                    let mut contradicted = false;
                    for (effect, delta) in ordered {
                        let (Some(before), Some(after)) = (
                            effect
                                .before
                                .and_then(|value| json_path(value, before_path))
                                .and_then(Value::as_i64),
                            effect
                                .after
                                .and_then(|value| json_path(value, after_path))
                                .and_then(Value::as_i64),
                        ) else {
                            contradicted = false;
                            current = i64::MIN;
                            break;
                        };
                        let Some(expected) = current.checked_add(delta) else {
                            current = i64::MIN;
                            break;
                        };
                        if before != current || after != expected {
                            contradicted = true;
                            break;
                        }
                        current = after;
                    }
                    if current == i64::MIN {
                        continue;
                    }
                    if contradicted {
                        return ProofOutcome::Violation {
                            event: ordered[1].0.event,
                            oracle: "concurrent-conservation",
                            reason: "overlapping committed updates contradicted the authored \
                                     conservation transition"
                                .into(),
                        };
                    }
                    observed_valid = true;
                }
            }
        }
    }
    if observed_valid {
        ProofOutcome::Satisfied
    } else {
        ProofOutcome::Abstain
    }
}

fn round_trip_outcome<'a>(
    proof: &BackendProofContract,
    contracts: &BTreeMap<&str, &'a OperationContract>,
    invocations: &'a BTreeMap<(String, String), Invocation<'a>>,
) -> ProofOutcome<'a> {
    let BackendProofContract::ResourceRoundTrip {
        write_operation,
        read_operation,
        write_identity_output_path,
        read_identity_input_path,
        write_snapshot_output_path,
        read_snapshot_input_path,
        consistency,
        checks,
    } = proof
    else {
        return ProofOutcome::Abstain;
    };
    let (Some(write_contract), Some(read_contract)) = (
        contracts.get(write_operation.as_str()),
        contracts.get(read_operation.as_str()),
    ) else {
        return ProofOutcome::Abstain;
    };
    if *consistency != ResourceConsistency::Strong || checks.is_empty() {
        return ProofOutcome::Abstain;
    }
    let writes = proof_invocations(write_operation, invocations);
    let reads = proof_invocations(read_operation, invocations);
    let mut observed_valid = false;
    for write in writes.iter().filter(|invocation| {
        invocation.returned.as_ref().is_some_and(|returned| {
            write_contract.is_success(returned) && returned.effects_complete
        })
    }) {
        let Some(write_start) = write.start else {
            continue;
        };
        let Some(write_return) = write.returned.as_ref() else {
            continue;
        };
        if write_start.actor.is_none() || write_start.tenant.is_none() {
            continue;
        }
        let Some(identity) = scalar_at(write_return.output, write_identity_output_path) else {
            continue;
        };
        let Some(snapshot) = scalar_at(write_return.output, write_snapshot_output_path) else {
            continue;
        };
        for read in reads.iter().filter(|invocation| {
            invocation.start.is_some_and(|start| {
                start.actor == write_start.actor && start.tenant == write_start.tenant
            }) && invocation.returned.as_ref().is_some_and(|returned| {
                read_contract.is_success(returned) && returned.effects_complete
            })
        }) {
            if authored_identity(read, read_identity_input_path) != Some(identity)
                || authored_identity(read, read_snapshot_input_path) != Some(snapshot)
            {
                continue;
            }
            let Some(read_return) = read.returned.as_ref() else {
                continue;
            };
            let mut unknown = false;
            for check in checks {
                let mismatch = match check {
                    RoundTripCheck::Exact {
                        write_input_path,
                        read_output_path,
                    }
                    | RoundTripCheck::MediaType {
                        write_input_path,
                        read_output_path,
                    } => {
                        let Some(expected) = invocation_input(write)
                            .and_then(|input| json_path(input, write_input_path))
                        else {
                            unknown = true;
                            break;
                        };
                        let Some(actual) = json_path(read_return.output, read_output_path) else {
                            unknown = true;
                            break;
                        };
                        if contains_redacted(expected) || contains_redacted(actual) {
                            unknown = true;
                            break;
                        }
                        expected != actual
                    }
                    RoundTripCheck::Utf8Sha256 {
                        write_input_path,
                        read_hash_output_path,
                    } => {
                        let Some(content) = invocation_input(write)
                            .and_then(|input| json_path(input, write_input_path))
                            .and_then(Value::as_str)
                        else {
                            unknown = true;
                            break;
                        };
                        let Some(actual) = json_path(read_return.output, read_hash_output_path)
                            .and_then(Value::as_str)
                        else {
                            unknown = true;
                            break;
                        };
                        hash(content.as_bytes()) != actual.to_ascii_lowercase()
                    }
                    RoundTripCheck::ByteSize {
                        write_input_path,
                        read_size_output_path,
                    } => {
                        let Some(content) = invocation_input(write)
                            .and_then(|input| json_path(input, write_input_path))
                        else {
                            unknown = true;
                            break;
                        };
                        let expected = match content {
                            Value::String(value) => value.len() as u64,
                            Value::Array(value) => value.len() as u64,
                            _ => {
                                unknown = true;
                                break;
                            }
                        };
                        let Some(actual) = json_path(read_return.output, read_size_output_path)
                            .and_then(Value::as_u64)
                        else {
                            unknown = true;
                            break;
                        };
                        expected != actual
                    }
                };
                if mismatch {
                    return ProofOutcome::Violation {
                        event: read_return.event,
                        oracle: "resource-round-trip",
                        reason: format!(
                            "the strong read contradicted authored round-trip check {}",
                            round_trip_check_name(check)
                        ),
                    };
                }
            }
            if !unknown {
                observed_valid = true;
            }
        }
    }
    if observed_valid {
        ProofOutcome::Satisfied
    } else {
        ProofOutcome::Abstain
    }
}

fn round_trip_check_name(check: &RoundTripCheck) -> &'static str {
    match check {
        RoundTripCheck::Exact { .. } => "exact-field",
        RoundTripCheck::Utf8Sha256 { .. } => "utf8-sha256",
        RoundTripCheck::ByteSize { .. } => "byte-size",
        RoundTripCheck::MediaType { .. } => "media-type",
    }
}

fn contains_redacted(value: &Value) -> bool {
    redacted_metadata(value).is_some()
        || match value {
            Value::Array(values) => values.iter().any(contains_redacted),
            Value::Object(values) => values.values().any(contains_redacted),
            _ => false,
        }
}

fn proof_violation(
    contract: &OperationContract,
    proof: &BackendProofContract,
    event: &BackendEvent,
    oracle: &str,
    reason: String,
) -> BackendViolation {
    let contract_hash =
        hash(&serde_json::to_vec(&(contract, proof)).expect("proof contract serialization"))[..16]
            .to_string();
    let identity = format!("{}:{contract_hash}:{oracle}:{reason}", contract.id);
    BackendViolation {
        operation: contract.id.clone(),
        contract_hash,
        fingerprint: hash(identity.as_bytes())[..20].to_string(),
        oracle: oracle.into(),
        reason,
        trace_id: event.trace_id.clone(),
        span_id: event.span_id.clone(),
        action_index: event.action_index,
    }
}

pub(super) fn common_query_scalar_input<'a>(
    pages: &[&Invocation<'a>],
    path: &str,
) -> Option<&'a Value> {
    let first = pages.first()?.start.and_then(|start| match &start.event {
        BackendEventKind::Start { input } => scalar_at(input, path),
        _ => None,
    })?;
    pages
        .iter()
        .all(|page| {
            page.start.and_then(|start| match &start.event {
                BackendEventKind::Start { input } => scalar_at(input, path),
                _ => None,
            }) == Some(first)
        })
        .then_some(first)
}

pub(super) fn optional_query_scalar(value: &Value, path: &str) -> Option<String> {
    match json_path(value, path) {
        None | Some(Value::Null) => None,
        Some(value @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) => {
            Some(canonical_json(value))
        }
        _ => None,
    }
}

pub(super) fn paired_transition<'a>(
    input: &Value,
    output: &'a Value,
    from: &str,
) -> Option<&'a str> {
    match (input, output) {
        (Value::String(before), Value::String(after)) if before == from => Some(after),
        (Value::Object(before), Value::Object(after)) => before.iter().find_map(|(key, value)| {
            after
                .get(key)
                .and_then(|next| paired_transition(value, next, from))
        }),
        (Value::Array(before), Value::Array(after)) => before
            .iter()
            .zip(after)
            .find_map(|(value, next)| paired_transition(value, next, from)),
        _ => None,
    }
}

pub(super) fn json_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() || path == "$" {
        return Some(value);
    }
    path.trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .try_fold(value, |current, part| current.get(part))
}

pub(super) fn json_path_values<'a>(value: &'a Value, path: &str) -> Vec<&'a Value> {
    let parts = path
        .trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    fn descend<'a>(value: &'a Value, parts: &[&str], values: &mut Vec<&'a Value>) {
        if parts.is_empty() {
            match value {
                Value::Array(items) => values.extend(items),
                _ => values.push(value),
            }
            return;
        }
        match value {
            Value::Array(items) => {
                for item in items {
                    descend(item, parts, values);
                }
            }
            Value::Object(object) => {
                if let Some(next) = object.get(parts[0]) {
                    descend(next, &parts[1..], values);
                }
            }
            _ => {}
        }
    }
    let mut values = Vec::new();
    descend(value, &parts, &mut values);
    values
}

pub(in crate::domain::backend) fn selection_mismatch(
    domain: &ValueDomain,
    output: &Value,
    selections: &[GraphqlSelection],
) -> Option<String> {
    for selection in selections {
        let schema = normalized_selection_path(&selection.schema_path)?;
        let response = normalized_selection_path(&selection.response_path)?;
        if schema.len() != response.len() {
            continue;
        }
        if let Some(reason) = selected_path_mismatch(
            domain,
            output,
            &schema,
            &response,
            "$output",
            selection.type_condition.as_deref(),
        ) {
            return Some(reason);
        }
    }
    None
}

fn normalized_selection_path(path: &str) -> Option<Vec<(String, bool)>> {
    static NAME: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let name = NAME.get_or_init(|| {
        regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("the GraphQL name regex is valid")
    });
    let mut out = Vec::new();
    for raw in path.split('.') {
        let (raw, array) = raw
            .strip_suffix("[]")
            .map_or((raw, false), |field| (field, true));
        if !name.is_match(raw) {
            return None;
        }
        out.push((raw.to_string(), array));
    }
    (!out.is_empty()).then_some(out)
}

fn selected_path_mismatch(
    domain: &ValueDomain,
    value: &Value,
    schema: &[(String, bool)],
    response: &[(String, bool)],
    path: &str,
    type_condition: Option<&str>,
) -> Option<String> {
    if value.is_null() && domain.mismatch(value, path).is_none() {
        return None;
    }
    if let Some(condition) = type_condition {
        if graphql_abstract_has_variant(domain, condition)
            && value.get("__typename").and_then(Value::as_str) != Some(condition)
        {
            // A conditional fragment only promises fields for the matching
            // concrete object. Missing or different runtime type evidence is
            // not enough to make a hard selected-field claim.
            return None;
        }
    }
    let domain = concrete_domain(domain, value)?;
    if let ValueDomain::Array { items, .. } = domain {
        let values = value.as_array()?;
        return values.iter().enumerate().find_map(|(index, item)| {
            selected_path_mismatch(
                items,
                item,
                schema,
                response,
                &format!("{path}[{index}]"),
                type_condition,
            )
        });
    }
    let ValueDomain::Object { properties, .. } = domain else {
        return None;
    };
    let ((schema_name, schema_array), schema_rest) = schema.split_first()?;
    let ((response_name, response_array), response_rest) = response.split_first()?;
    if schema_array != response_array {
        return None;
    }
    let field_domain = properties.get(schema_name)?;
    let object = value.as_object()?;
    let Some(field_value) = object.get(response_name) else {
        return Some(format!(
            "{path}.{response_name} was selected by the GraphQL operation but is absent"
        ));
    };
    let field_path = format!("{path}.{response_name}");
    if *schema_array {
        let array_domain = concrete_domain(field_domain, field_value)?;
        let ValueDomain::Array { items, .. } = array_domain else {
            return field_domain.mismatch(field_value, &field_path);
        };
        let Some(values) = field_value.as_array() else {
            return field_domain.mismatch(field_value, &field_path);
        };
        if schema_rest.is_empty() {
            return field_domain.mismatch(field_value, &field_path);
        }
        return values.iter().enumerate().find_map(|(index, item)| {
            selected_path_mismatch(
                items,
                item,
                schema_rest,
                response_rest,
                &format!("{field_path}[{index}]"),
                type_condition,
            )
        });
    }
    if schema_rest.is_empty() {
        field_domain.mismatch(field_value, &field_path)
    } else {
        selected_path_mismatch(
            field_domain,
            field_value,
            schema_rest,
            response_rest,
            &field_path,
            type_condition,
        )
    }
}

fn graphql_abstract_has_variant(domain: &ValueDomain, condition: &str) -> bool {
    match domain {
        ValueDomain::OneOf { variants } => variants
            .iter()
            .any(|variant| graphql_abstract_has_variant(variant, condition)),
        ValueDomain::AllOf { variants } => variants
            .iter()
            .any(|variant| graphql_abstract_has_variant(variant, condition)),
        ValueDomain::GraphqlAbstract { variants } => variants.contains_key(condition),
        _ => false,
    }
}

fn concrete_domain<'a>(domain: &'a ValueDomain, value: &Value) -> Option<&'a ValueDomain> {
    match domain {
        ValueDomain::OneOf { variants } => variants
            .iter()
            .find(|variant| {
                !matches!(variant, ValueDomain::Null) && variant.mismatch(value, "$value").is_none()
            })
            .or_else(|| {
                variants
                    .iter()
                    .find(|variant| !matches!(variant, ValueDomain::Null))
            })
            .and_then(|variant| concrete_domain(variant, value)),
        ValueDomain::AllOf { variants } => variants
            .iter()
            .find(|variant| !matches!(variant, ValueDomain::Any))
            .and_then(|variant| concrete_domain(variant, value))
            .or(Some(domain)),
        ValueDomain::GraphqlAbstract { variants } => value
            .get("__typename")
            .and_then(Value::as_str)
            .and_then(|kind| variants.get(kind))
            .and_then(|variant| concrete_domain(variant, value)),
        _ => Some(domain),
    }
}

/// A correct idempotent retry commonly returns the original success without
/// repeating its write or event. Judge promised effects across the complete
/// actor, tenant, operation, and key group so safe retries remain clean.
pub(super) fn idempotent_group_satisfies(
    contract: &OperationContract,
    start: &BackendEvent,
    promised: &EffectPattern,
    invocations: &BTreeMap<(String, String), Invocation<'_>>,
) -> bool {
    if !contract.idempotent || start.idempotency_key.is_none() {
        return false;
    }
    let count = invocations
        .values()
        .filter(|candidate| {
            candidate.start.is_some_and(|other| {
                other.operation == start.operation
                    && other.idempotency_key == start.idempotency_key
                    && other.actor == start.actor
                    && other.tenant == start.tenant
            }) && candidate
                .returned
                .as_ref()
                .is_some_and(|returned| contract.is_success(returned))
        })
        .flat_map(|candidate| candidate.effects.iter())
        .filter(|effect| effect_matches(effect, promised))
        .count();
    count >= promised.at_least
}

pub(super) fn effect_matches(effect: &EffectEvent<'_>, pattern: &EffectPattern) -> bool {
    effect.effect == pattern.kind
        && pattern
            .resource
            .as_deref()
            .is_none_or(|resource| effect.resource == Some(resource))
        && pattern
            .event
            .as_deref()
            .is_none_or(|event| effect.emitted == Some(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codec_result(
        input: Value,
        output: Value,
        authority: Authority,
        projections: Vec<CodecProjection>,
    ) -> (&'static str, Option<String>) {
        let start = BackendEvent {
            sequence: 1,
            trace_id: "trace".into(),
            span_id: "span".into(),
            action_index: 1,
            parent_span_id: None,
            operation: "codec".into(),
            build: None,
            config_contract: None,
            actor: None,
            tenant: None,
            idempotency_key: None,
            selections: Vec::new(),
            event: BackendEventKind::Start { input },
        };
        let returned = BackendEvent {
            sequence: 2,
            event: BackendEventKind::Return {
                output,
                status: Some(200),
                success: true,
                effects_complete: false,
            },
            ..start.clone()
        };
        let returned_output = match &returned.event {
            BackendEventKind::Return { output, .. } => output,
            _ => unreachable!(),
        };
        let contract = OperationContract {
            id: "codec".into(),
            authority,
            input: None,
            output: None,
            outputs_by_status: BTreeMap::new(),
            success_statuses: vec![200],
            read_only: true,
            idempotent: true,
            idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
            tenant_isolated: false,
            promised_effects: Vec::new(),
        };
        let contracts = BTreeMap::from([("codec", &contract)]);
        let invocations = BTreeMap::from([(
            ("trace".into(), "span".into()),
            Invocation {
                start: Some(&start),
                returned: Some(ReturnEvent {
                    event: &returned,
                    output: returned_output,
                    status: Some(200),
                    success: true,
                    effects_complete: false,
                }),
                effects: Vec::new(),
                protocols: Vec::new(),
            },
        )]);
        let proof = BackendProofContract::CodecRoundTrip {
            operation: "codec".into(),
            projections,
        };
        match codec_round_trip_outcome(&proof, &contracts, &invocations) {
            ProofOutcome::Violation { oracle, reason, .. } => {
                ("violation", Some(format!("{oracle}:{reason}")))
            }
            ProofOutcome::Satisfied => ("satisfied", None),
            ProofOutcome::Abstain => ("abstain", None),
        }
    }

    #[test]
    fn codec_projection_has_violation_satisfied_and_abstain_outcomes() {
        let projection = || CodecProjection {
            input_path: "$.typed.amount".into(),
            output_path: "$.decoded.amount".into(),
        };
        assert_eq!(
            codec_result(
                json!({"typed":{"amount":"10.25"}}),
                json!({"decoded":{"amount":"10.25"}}),
                Authority::Declared,
                vec![projection()],
            )
            .0,
            "satisfied"
        );
        let violation = codec_result(
            json!({"typed":{"amount":"10.25"}}),
            json!({"decoded":{"amount":"10.2"}}),
            Authority::Declared,
            vec![projection()],
        );
        assert_eq!(violation.0, "violation");
        assert!(violation.1.unwrap().contains("codec-round-trip"));
        assert_eq!(
            codec_result(
                json!({"typed":{"amount":"10.25"}}),
                json!({"decoded":{}}),
                Authority::Declared,
                vec![projection()],
            )
            .0,
            "abstain"
        );
        assert_eq!(
            codec_result(
                json!({"typed":{"amount":"10.25"}}),
                json!({"decoded":{"amount":"different"}}),
                Authority::Inferred,
                vec![projection()],
            )
            .0,
            "abstain"
        );
    }
}
