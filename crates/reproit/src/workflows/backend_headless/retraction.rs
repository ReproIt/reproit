use super::*;

/// The verdict for one persisted finding artifact. `Retracted` is the fourth
/// state the three-way replay verdict cannot express: the recorded request still
/// produces the recorded violation, but the project no longer asserts the
/// contract that made it a violation.
///
/// This matters because a schema-driven tool's most common true outcome on a
/// first run against an existing API is "the contract was wrong". The recorded
/// finding stays true under its own contract forever, so replaying it against
/// that contract can never go green, and withdrawing the false claim (the
/// correct fix) would otherwise leave no way to close the finding short of
/// deleting the artifact by hand.
///
/// Retracted is deliberately NOT held: it is not proof of anything about the
/// implementation, so it never counts as a proof-of-fix. It also does not block,
/// because retracting a claim is an explicit, reviewable schema edit, and the
/// same edit already makes `scan` stop reporting the finding. Reporting it in
/// its own bucket keeps the two commands' answers consistent instead of leaving
/// `verify` blocked on a claim the project has disowned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ArtifactVerdict {
    Reproduced,
    Fixed,
    Inconclusive,
    Retracted(String),
}

/// Whether the project still asserts the contract a recorded finding was proven
/// against.
pub(super) enum ContractStatus {
    /// The schema and policy still say exactly what the finding was proven
    /// against, so the recorded verdict stands as-is.
    Unchanged,
    /// The operation is still declared but its claim was edited. The recorded
    /// violation may no longer be assertable, so it is re-checked against this.
    Changed(Box<CurrentClaim>),
    /// The operation itself is gone from the schema.
    Absent,
    /// The project's current claims could not be read (no backend config, an
    /// unreadable or since-moved schema). Never retract on ignorance.
    Unknown,
}

/// What the project asserts about one operation today.
pub(super) struct CurrentClaim {
    pub(super) contract: OperationContract,
    pub(super) policy: BackendPolicy,
}

/// The operation contracts and policy the project asserts right now, as scan
/// would build them: every declared schema aggregated, then the reproit.yaml
/// `operations` overrides applied.
pub(super) struct CurrentContracts {
    operations: Option<BTreeMap<String, OperationContract>>,
    policy: BackendPolicy,
}

impl CurrentContracts {
    /// Read the project's current claims. Every failure path degrades to
    /// `Unknown` rather than erroring: verify's job is to replay findings, and a
    /// project it cannot read is a reason to trust the recorded contract, not a
    /// reason to fail the run.
    pub(super) fn load(config_path: Option<&Path>) -> Self {
        let unknown = Self {
            operations: None,
            policy: BackendPolicy::default(),
        };
        let Ok(Some((targets, config))) = crate::workflows::backend_target::resolve(config_path)
        else {
            return unknown;
        };
        let Ok(schemas) = aggregate_service_endpoints(&targets) else {
            return unknown;
        };
        let mut endpoints = schemas.endpoints;
        for endpoint in &mut endpoints {
            if let Some(declared) = config
                .operations
                .iter()
                .find(|declared| declared.id == endpoint.contract.id)
            {
                apply_operation_override(&mut endpoint.contract, declared);
            }
        }
        Self {
            operations: Some(
                endpoints
                    .into_iter()
                    .map(|endpoint| (endpoint.contract.id.clone(), endpoint.contract))
                    .collect(),
            ),
            policy: BackendPolicy {
                invariants: config.invariants,
                resources: config.resources,
                proofs: config.proofs,
                fleet: config.fleet,
            },
        }
    }

    pub(super) fn status(&self, recorded: &ReplayStep) -> ContractStatus {
        let Some(operations) = &self.operations else {
            return ContractStatus::Unknown;
        };
        let Some(current) = operations.get(&recorded.contract.id) else {
            return ContractStatus::Absent;
        };
        if same(current, &recorded.contract) && same(&self.policy, &recorded.policy) {
            return ContractStatus::Unchanged;
        }
        ContractStatus::Changed(Box::new(CurrentClaim {
            contract: current.clone(),
            policy: self.policy.clone(),
        }))
    }
}

/// Structural equality over the serialized form. A claim that will not serialize
/// is reported as changed, which costs one extra replay and can only make the
/// verdict stricter (a re-check that still reproduces stays Reproduced).
fn same<T: Serialize>(left: &T, right: &T) -> bool {
    match (serde_json::to_value(left), serde_json::to_value(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

pub(super) fn absent_reason(operation: &str) -> String {
    format!("the schema no longer declares {operation}")
}

pub(super) fn changed_reason(operation: &str) -> String {
    format!("the schema no longer makes the violated claim about {operation}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, read_only: bool) -> ReplayStep {
        ReplayStep {
            contract: OperationContract {
                id: id.into(),
                authority: backend::Authority::Schema,
                input: None,
                output: None,
                outputs_by_status: BTreeMap::new(),
                success_statuses: Vec::new(),
                read_only,
                idempotent: false,
                idempotency_response_replay: backend::IdempotencyResponseReplay::Unspecified,
                tenant_isolated: false,
                promised_effects: Vec::new(),
            },
            request: RequestArtifact {
                operation: id.into(),
                method: "GET".into(),
                url: "http://127.0.0.1:1/x".into(),
                input: Value::Null,
                headers: BTreeMap::new(),
                body: None,
                content_type: None,
                schema_source: None,
                client_streaming: false,
                server_streaming: false,
                bindings: Vec::new(),
            },
            policy: BackendPolicy::default(),
        }
    }

    fn contracts(operations: &[&ReplayStep]) -> CurrentContracts {
        CurrentContracts {
            operations: Some(
                operations
                    .iter()
                    .map(|step| (step.contract.id.clone(), step.contract.clone()))
                    .collect(),
            ),
            policy: BackendPolicy::default(),
        }
    }

    #[test]
    fn an_unreadable_project_never_retracts() {
        let recorded = step("getNearby", false);
        let unknown = CurrentContracts {
            operations: None,
            policy: BackendPolicy::default(),
        };
        assert!(matches!(unknown.status(&recorded), ContractStatus::Unknown));
    }

    #[test]
    fn an_identical_claim_is_unchanged() {
        let recorded = step("getNearby", false);
        assert!(matches!(
            contracts(&[&recorded]).status(&recorded),
            ContractStatus::Unchanged
        ));
    }

    #[test]
    fn a_dropped_operation_is_absent() {
        let recorded = step("getNearby", false);
        let other = step("listUsers", false);
        assert!(matches!(
            contracts(&[&other]).status(&recorded),
            ContractStatus::Absent
        ));
    }

    #[test]
    fn an_edited_claim_reports_the_current_contract() {
        // The case this exists for: the schema asserted something false about
        // the API and the fix is to withdraw the assertion, not to change the
        // product. The operation survives; only its claim moved.
        let recorded = step("getNearby", false);
        let edited = step("getNearby", true);
        match contracts(&[&edited]).status(&recorded) {
            ContractStatus::Changed(claim) => assert!(claim.contract.read_only),
            _ => panic!("an edited claim must be re-checked against the current contract"),
        }
    }

    #[test]
    fn a_withdrawn_authored_invariant_also_counts_as_a_changed_claim() {
        // Authored invariants live in reproit.yaml, not the schema, but they are
        // claims the project makes in exactly the same sense.
        let recorded = step("getNearby", false);
        let mut current = contracts(&[&recorded]);
        current
            .policy
            .invariants
            .push(BackendInvariant::Idempotent {
                operation: "getNearby".into(),
            });
        assert!(matches!(
            current.status(&recorded),
            ContractStatus::Changed(_)
        ));
    }
}
