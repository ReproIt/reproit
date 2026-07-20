use super::*;

pub(super) fn evaluate_fleet(
    config: &BackendConfig,
    contracts: &BTreeMap<&str, &OperationContract>,
    events: &[BackendEvent],
    violations: &mut Vec<BackendViolation>,
) {
    evaluate_fleet_dimension(
        config.fleet.same_build,
        "build",
        events,
        contracts,
        violations,
        |event| event.build.as_deref(),
    );
    evaluate_fleet_dimension(
        config.fleet.same_config_contract,
        "config contract",
        events,
        contracts,
        violations,
        |event| event.config_contract.as_deref(),
    );
}

fn evaluate_fleet_dimension<'a>(
    enabled: bool,
    label: &str,
    events: &'a [BackendEvent],
    contracts: &BTreeMap<&str, &OperationContract>,
    violations: &mut Vec<BackendViolation>,
    value: impl Fn(&'a BackendEvent) -> Option<&'a str>,
) {
    if !enabled {
        return;
    }
    let mut first = None;
    for event in events {
        let Some(current) = value(event) else {
            continue;
        };
        match first {
            None => first = Some(current),
            Some(expected) if expected != current => {
                if let Some(contract) = contracts.get(event.operation.as_str()) {
                    violations.push(violation(
                        contract,
                        event,
                        "fleet-consistency",
                        format!("fleet mixed {label} {expected:?} with {current:?}"),
                    ));
                }
            }
            _ => {}
        }
    }
}

/// The externally persistent final-state witness available in one invocation.
/// Events, calls, and unkeyed mutations are intentionally excluded: they do not
/// prove a comparable durable state for generic idempotency.
pub(super) fn persistent_final_effects(
    effects: &[EffectEvent<'_>],
    intended_resources: &BTreeSet<&str>,
    invocation_tenant: Option<&str>,
) -> Option<BTreeMap<(String, String, String), String>> {
    let mut final_effects = BTreeMap::new();
    for effect in effects {
        if !matches!(effect.effect, EffectKind::Write | EffectKind::Delete) {
            continue;
        }
        let Some(resource) = effect.resource else {
            continue;
        };
        if !intended_resources.contains(resource) {
            continue;
        }
        let (Some(key), Some(tenant)) = (effect.key, effect.tenant.or(invocation_tenant)) else {
            return None;
        };
        let value = match effect.effect {
            EffectKind::Write => {
                let after = effect.after?;
                format!("write:{}", canonical_json(after))
            }
            EffectKind::Delete => "delete".to_string(),
            _ => unreachable!(),
        };
        final_effects.insert(
            (tenant.to_string(), resource.to_string(), key.to_string()),
            value,
        );
    }
    Some(final_effects)
}
