use super::*;

impl OperationContract {
    fn is_success(&self, returned: &ReturnEvent) -> bool {
        returned.success
            && (self.success_statuses.is_empty()
                || returned
                    .status
                    .is_some_and(|status| self.success_statuses.contains(&status)))
    }
}

#[derive(Default)]
pub(super) struct Invocation<'a> {
    pub(super) start: Option<&'a BackendEvent>,
    pub(super) returned: Option<ReturnEvent<'a>>,
    pub(super) effects: Vec<EffectEvent<'a>>,
    pub(super) protocols: Vec<(&'a BackendEvent, &'a ProtocolEvidence)>,
}

pub(super) struct ReturnEvent<'a> {
    pub(super) event: &'a BackendEvent,
    pub(super) output: &'a Value,
    pub(super) status: Option<u16>,
    pub(super) success: bool,
    pub(super) effects_complete: bool,
}

pub(super) struct EffectEvent<'a> {
    pub(super) event: &'a BackendEvent,
    pub(super) effect: EffectKind,
    pub(super) resource: Option<&'a str>,
    pub(super) key: Option<&'a str>,
    pub(super) tenant: Option<&'a str>,
    pub(super) emitted: Option<&'a str>,
    pub(super) before: Option<&'a Value>,
    pub(super) after: Option<&'a Value>,
}

pub fn evaluate(config: &BackendConfig, events: &[BackendEvent]) -> Vec<BackendViolation> {
    if !config.enabled {
        return Vec::new();
    }
    let mut invocations = BTreeMap::<(String, String), Invocation<'_>>::new();
    for event in events {
        let invocation = invocations
            .entry((event.trace_id.clone(), event.span_id.clone()))
            .or_default();
        match &event.event {
            BackendEventKind::Start { .. } => invocation.start = Some(event),
            BackendEventKind::Return {
                output,
                status,
                success,
                effects_complete,
            } => {
                invocation.returned = Some(ReturnEvent {
                    event,
                    output,
                    status: *status,
                    success: *success,
                    effects_complete: *effects_complete,
                })
            }
            BackendEventKind::Effect {
                effect,
                resource,
                key,
                tenant,
                event: emitted,
                before,
                after,
                ..
            } => invocation.effects.push(EffectEvent {
                event,
                effect: *effect,
                resource: resource.as_deref(),
                key: key.as_deref(),
                tenant: tenant.as_deref(),
                emitted: emitted.as_deref(),
                before: before.as_ref(),
                after: after.as_ref(),
            }),
            BackendEventKind::Protocol { proof } => invocation.protocols.push((event, proof)),
        }
    }

    let mut contracts = BTreeMap::new();
    for contract in config
        .operations
        .iter()
        .filter(|contract| contract.authority != Authority::Inferred)
    {
        match contracts.entry(contract.id.as_str()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(contract);
            }
            std::collections::btree_map::Entry::Occupied(mut entry)
                if contract.authority == Authority::Declared
                    && entry.get().authority != Authority::Declared =>
            {
                entry.insert(contract);
            }
            _ => {}
        }
    }
    let mut violations = Vec::new();
    for invocation in invocations.values() {
        let Some(start) = invocation.start else {
            continue;
        };
        let Some(contract) = contracts.get(start.operation.as_str()) else {
            continue;
        };
        let Some(returned) = &invocation.returned else {
            continue;
        };
        // A repeatable 5xx for an input that satisfies a schema-owned request
        // contract is a concrete server failure, not merely rejected fuzz
        // traffic. Keep this boundary narrow: undocumented or structurally
        // invalid inputs cannot create this finding, and the headless runner
        // still requires exact replay before publishing it.
        if !returned.success
            && returned.status.is_some_and(|status| status >= 500)
            && match (&contract.input, &start.event) {
                (Some(domain), BackendEventKind::Start { input }) => {
                    domain.mismatch(input, "$input").is_none()
                }
                (None, BackendEventKind::Start { input }) => input.is_null(),
                _ => false,
            }
        {
            violations.push(violation(
                contract,
                returned.event,
                "server-error",
                format!(
                    "contract-valid request returned HTTP {}",
                    returned.status.unwrap_or_default()
                ),
            ));
            continue;
        }
        if returned.success
            && !contract.success_statuses.is_empty()
            && returned
                .status
                .is_none_or(|status| !contract.success_statuses.contains(&status))
        {
            violations.push(violation(
                contract,
                returned.event,
                "response-status",
                format!(
                    "operation reported successful status {} outside its declared success \
                     statuses {:?}",
                    returned
                        .status
                        .map_or_else(|| "missing".into(), |status| status.to_string()),
                    contract.success_statuses
                ),
            ));
            continue;
        }
        if !contract.is_success(returned) {
            continue;
        }
        if let (Some(domain), BackendEventKind::Start { input }) = (&contract.input, &start.event) {
            if let Some(reason) = domain.mismatch(input, "$input") {
                violations.push(violation(
                    contract,
                    start,
                    "accepted-invalid-input",
                    format!("operation accepted input outside its declared domain: {reason}"),
                ));
            }
        }
        let output_domain = returned
            .status
            .and_then(|status| contract.outputs_by_status.get(&status))
            .or(contract.output.as_ref());
        if let Some(domain) = output_domain {
            if let Some(reason) = domain.mismatch(returned.output, "$output") {
                violations.push(violation(
                    contract,
                    returned.event,
                    "response-shape",
                    reason,
                ));
            } else if let Some(reason) =
                selection_mismatch(domain, returned.output, &returned.event.selections)
            {
                violations.push(violation(
                    contract,
                    returned.event,
                    "response-selection",
                    reason,
                ));
            }
        }
        evaluate_authored_invariants(config, contract, start, returned, &mut violations);
        if contract.read_only {
            if let Some(effect) = invocation
                .effects
                .iter()
                .find(|effect| matches!(effect.effect, EffectKind::Write | EffectKind::Delete))
            {
                violations.push(violation(
                    contract,
                    effect.event,
                    "read-only-mutation",
                    format!(
                        "read-only operation mutated {}",
                        effect.resource.unwrap_or("persistent state")
                    ),
                ));
            }
        }
        for promised in &contract.promised_effects {
            let count = invocation
                .effects
                .iter()
                .filter(|effect| effect_matches(effect, promised))
                .count();
            if returned.effects_complete
                && count < promised.at_least
                && !idempotent_group_satisfies(contract, start, promised, &invocations)
            {
                violations.push(violation(
                    contract,
                    returned.event,
                    "missing-effect",
                    format!(
                        "successful operation promised at least {} {:?} effect(s) on {}, but \
                         observed {}",
                        promised.at_least,
                        promised.kind,
                        promised
                            .resource
                            .as_deref()
                            .or(promised.event.as_deref())
                            .unwrap_or("any resource"),
                        count
                    ),
                ));
            }
            if promised.at_most.is_some_and(|maximum| count > maximum) {
                violations.push(violation(
                    contract,
                    returned.event,
                    "excess-effect",
                    format!(
                        "successful operation allowed at most {} {:?} effect(s) on {}, but \
                         observed {}",
                        promised.at_most.unwrap_or_default(),
                        promised.kind,
                        promised
                            .resource
                            .as_deref()
                            .or(promised.event.as_deref())
                            .unwrap_or("any resource"),
                        count
                    ),
                ));
            }
        }
        if contract.tenant_isolated {
            if let Some(operation_tenant) = start.tenant.as_deref() {
                if let Some(effect) = invocation.effects.iter().find(|effect| {
                    effect
                        .tenant
                        .is_some_and(|tenant| tenant != operation_tenant)
                }) {
                    violations.push(violation(
                        contract,
                        effect.event,
                        "tenant-isolation",
                        "operation crossed its declared tenant boundary".into(),
                    ));
                }
            }
        }
    }
    for invocation in invocations.values() {
        for (event, proof) in &invocation.protocols {
            let Some(contract) = contracts.get(event.operation.as_str()) else {
                continue;
            };
            for proven in proof.evaluate() {
                violations.push(violation(contract, event, &proven.oracle, proven.reason));
            }
        }
    }
    evaluate_resource_lifecycles(config, &contracts, &invocations, &mut violations);
    evaluate_query_pagination(config, &contracts, &invocations, &mut violations);
    evaluate_proof_contracts(config, &contracts, &invocations, &mut violations);
    evaluate_idempotency(config, &contracts, &invocations, &mut violations);
    evaluate_fleet(config, &contracts, events, &mut violations);
    violations.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    violations.dedup_by(|a, b| a.fingerprint == b.fingerprint);
    violations
}

mod lifecycle;
use lifecycle::{evaluate_resource_lifecycles, scalar_at};
mod invariants;
use invariants::{evaluate_authored_invariants, evaluate_query_pagination};
mod proofs;
pub(in crate::model::backend) use proofs::selection_mismatch;
use proofs::{
    common_query_scalar_input, effect_matches, evaluate_proof_contracts,
    idempotent_group_satisfies, json_path, json_path_values, optional_query_scalar,
    paired_transition,
};
#[cfg(test)]
pub(in crate::model::backend) use proofs::{
    failed_atomicity_effect_outcome, AtomicityEffectOutcome,
};
mod idempotency;
use idempotency::evaluate_idempotency;
mod fleet;
use fleet::{evaluate_fleet, persistent_final_effects};
fn violation(
    contract: &OperationContract,
    event: &BackendEvent,
    oracle: &str,
    reason: String,
) -> BackendViolation {
    let contract_hash =
        hash(&serde_json::to_vec(contract).expect("contract serialization"))[..16].to_string();
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
