use super::*;

#[derive(Default)]
struct ResourceLifecycleState {
    expected: BTreeMap<usize, Value>,
    deleted: bool,
}

pub(super) fn evaluate_resource_lifecycles(
    config: &BackendConfig,
    contracts: &BTreeMap<&str, &OperationContract>,
    invocations: &BTreeMap<(String, String), Invocation<'_>>,
    violations: &mut Vec<BackendViolation>,
) {
    for resource in &config.resources {
        // Eventual and unspecified consistency need an authored observation
        // boundary. The first lifecycle slice deliberately abstains instead of
        // guessing that an immediately stale read is a defect.
        if resource.consistency != ResourceConsistency::Strong
            || resource.read.absent_statuses.is_empty()
        {
            continue;
        }
        let required = [
            resource.create.operation.as_str(),
            resource.read.operation.as_str(),
        ];
        if required
            .into_iter()
            .any(|operation| !contracts.contains_key(operation))
        {
            continue;
        }

        let mut ordered = invocations.values().collect::<Vec<_>>();
        ordered.sort_by_key(|invocation| invocation.start.map(|event| event.sequence));
        let mut states = BTreeMap::<
            (String, Option<String>, Option<String>, String),
            ResourceLifecycleState,
        >::new();

        for invocation in ordered {
            let (Some(start), Some(returned)) = (invocation.start, invocation.returned.as_ref())
            else {
                continue;
            };
            let input = match &start.event {
                BackendEventKind::Start { input } => input,
                _ => continue,
            };
            let scope = || {
                (
                    start.trace_id.clone(),
                    start.actor.clone(),
                    start.tenant.clone(),
                )
            };

            if start.operation == resource.create.operation {
                let Some(contract) = contracts.get(start.operation.as_str()) else {
                    continue;
                };
                if !contract.is_success(returned) {
                    continue;
                }
                let Some(identity) =
                    scalar_at(returned.output, &resource.create.output_identity_path)
                else {
                    continue;
                };
                let (trace, actor, tenant) = scope();
                let mut state = ResourceLifecycleState::default();
                for (index, field) in resource.fields.iter().enumerate() {
                    if let Some(path) = field.create_output_path.as_deref() {
                        if let Some(value) = json_path(returned.output, path) {
                            state.expected.insert(index, value.clone());
                        }
                    }
                }
                states.insert((trace, actor, tenant, canonical_json(identity)), state);
                continue;
            }

            let mutation = resource
                .update
                .as_ref()
                .filter(|value| value.operation == start.operation);
            if let Some(update) = mutation {
                let Some(contract) = contracts.get(start.operation.as_str()) else {
                    continue;
                };
                if !contract.is_success(returned) {
                    continue;
                }
                let Some(identity) = scalar_at(input, &update.input_identity_path) else {
                    continue;
                };
                let (trace, actor, tenant) = scope();
                let Some(state) = states.get_mut(&(trace, actor, tenant, canonical_json(identity)))
                else {
                    continue;
                };
                for (index, field) in resource.fields.iter().enumerate() {
                    if let Some(path) = field.update_input_path.as_deref() {
                        if let Some(value) = json_path(input, path) {
                            state.expected.insert(index, value.clone());
                        }
                    }
                }
                continue;
            }

            let deletion = resource
                .delete
                .as_ref()
                .filter(|value| value.operation == start.operation);
            if let Some(delete) = deletion {
                let Some(contract) = contracts.get(start.operation.as_str()) else {
                    continue;
                };
                if !contract.is_success(returned) {
                    continue;
                }
                let Some(identity) = scalar_at(input, &delete.input_identity_path) else {
                    continue;
                };
                let (trace, actor, tenant) = scope();
                if let Some(state) =
                    states.get_mut(&(trace, actor, tenant, canonical_json(identity)))
                {
                    state.deleted = true;
                }
                continue;
            }

            if start.operation != resource.read.operation {
                continue;
            }
            let Some(read_contract) = contracts.get(start.operation.as_str()) else {
                continue;
            };
            let Some(requested) = scalar_at(input, &resource.read.input_identity_path) else {
                continue;
            };
            let (trace, actor, tenant) = scope();
            let Some(state) = states.get(&(trace, actor, tenant, canonical_json(requested))) else {
                continue;
            };
            let absent = returned
                .status
                .is_some_and(|status| resource.read.absent_statuses.contains(&status));

            if state.deleted {
                if read_contract.is_success(returned)
                    && scalar_at(returned.output, &resource.read.output_identity_path)
                        .is_some_and(|actual| actual == requested)
                {
                    violations.push(lifecycle_violation(
                        resource,
                        read_contract,
                        returned.event,
                        "resource-delete-visible",
                        format!(
                            "strong resource {} remained readable after successful delete",
                            resource.name
                        ),
                    ));
                }
                continue;
            }

            if absent {
                violations.push(lifecycle_violation(
                    resource,
                    read_contract,
                    returned.event,
                    "resource-create-missing",
                    format!(
                        "strong resource {} was absent after successful create",
                        resource.name
                    ),
                ));
                continue;
            }
            if !read_contract.is_success(returned) {
                continue;
            }
            let Some(actual_identity) =
                scalar_at(returned.output, &resource.read.output_identity_path)
            else {
                continue;
            };
            if actual_identity != requested {
                violations.push(lifecycle_violation(
                    resource,
                    read_contract,
                    returned.event,
                    "resource-identity",
                    format!(
                        "strong resource {} read returned a different declared identity",
                        resource.name
                    ),
                ));
                continue;
            }
            for (index, expected) in &state.expected {
                let field = &resource.fields[*index];
                let Some(actual) = json_path(returned.output, &field.read_output_path) else {
                    continue;
                };
                if actual != expected {
                    violations.push(lifecycle_violation(
                        resource,
                        read_contract,
                        returned.event,
                        "resource-state",
                        format!(
                            "strong resource {} read contradicted declared field {}",
                            resource.name, field.read_output_path
                        ),
                    ));
                }
            }
        }
    }
}

pub(super) fn scalar_at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    json_path(value, path)
        .filter(|value| matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)))
}

fn lifecycle_violation(
    resource: &ResourceLifecycleContract,
    contract: &OperationContract,
    event: &BackendEvent,
    oracle: &str,
    reason: String,
) -> BackendViolation {
    let contract_hash =
        hash(&serde_json::to_vec(&(resource, contract)).expect("lifecycle contract serialization"))
            [..16]
            .to_string();
    let identity = format!("{}:{contract_hash}:{oracle}:{reason}", resource.name);
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
