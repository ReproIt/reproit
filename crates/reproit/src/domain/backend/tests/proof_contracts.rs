use super::query::{query_invocation, query_operation};
use super::*;

pub(super) fn proof_operation(id: &str) -> OperationContract {
    let mut operation = query_operation(id);
    operation.success_statuses = vec![200];
    operation.read_only = false;
    operation
}

fn principal_event(
    sequence: u64,
    span: &str,
    operation: &str,
    actor: &str,
    tenant: &str,
    kind: BackendEventKind,
) -> BackendEvent {
    let mut event = event(sequence, span, operation, kind);
    event.actor = Some(actor.into());
    event.tenant = Some(tenant.into());
    event
}

fn return_kind(output: Value, status: u16, success: bool, complete: bool) -> BackendEventKind {
    BackendEventKind::Return {
        output,
        status: Some(status),
        success,
        effects_complete: complete,
    }
}

fn effect_invocation(events: &[BackendEvent]) -> Invocation<'_> {
    let effects = events
        .iter()
        .filter_map(|event| match &event.event {
            BackendEventKind::Effect {
                effect,
                resource,
                key,
                tenant,
                event: emitted,
                before,
                after,
                ..
            } => Some(EffectEvent {
                event,
                effect: *effect,
                resource: resource.as_deref(),
                key: key.as_deref(),
                tenant: tenant.as_deref(),
                emitted: emitted.as_deref(),
                before: before.as_ref(),
                after: after.as_ref(),
            }),
            _ => None,
        })
        .collect();
    Invocation {
        effects,
        ..Invocation::default()
    }
}

#[test]
fn authorization_matrix_accepts_authored_denials_and_proves_data_disclosure() {
    let proof = BackendProofContract::AuthorizationMatrix {
        operation: "getOrder".into(),
        identity_input_path: "$.id".into(),
        snapshot_input_path: "$.revision".into(),
        consistency: ResourceConsistency::Strong,
        principals: vec![
            AuthorizationPrincipal {
                actor: "alice".into(),
                tenant: "tenant-a".into(),
                decision: AuthorizationDecision::Allow,
            },
            AuthorizationPrincipal {
                actor: "bob".into(),
                tenant: "tenant-b".into(),
                decision: AuthorizationDecision::Deny,
            },
        ],
        deny: AuthorizationDenyPolicy {
            statuses: vec![401, 403, 404],
            redacted_output_paths: vec!["$.secret".into()],
        },
    };
    let config = BackendConfig {
        enabled: true,
        operations: vec![proof_operation("getOrder")],
        proofs: vec![proof],
        ..BackendConfig::default()
    };
    let input = json!({"id":"o1","revision":"r1"});
    let allowed = vec![
        principal_event(
            1,
            "allow",
            "getOrder",
            "alice",
            "tenant-a",
            BackendEventKind::Start {
                input: input.clone(),
            },
        ),
        principal_event(
            2,
            "allow",
            "getOrder",
            "alice",
            "tenant-a",
            return_kind(json!({"secret":"value"}), 200, true, true),
        ),
    ];
    let mut denied = allowed.clone();
    denied.extend([
        principal_event(
            3,
            "deny",
            "getOrder",
            "bob",
            "tenant-b",
            BackendEventKind::Start {
                input: input.clone(),
            },
        ),
        principal_event(
            4,
            "deny",
            "getOrder",
            "bob",
            "tenant-b",
            return_kind(json!({"secret":"leaked"}), 200, true, true),
        ),
    ]);
    assert!(evaluate(&config, &denied)
        .iter()
        .any(|violation| violation.oracle == "authorization-matrix"));

    let mut hidden = allowed.clone();
    hidden.extend([
        principal_event(
            3,
            "deny",
            "getOrder",
            "bob",
            "tenant-b",
            BackendEventKind::Start {
                input: input.clone(),
            },
        ),
        principal_event(
            4,
            "deny",
            "getOrder",
            "bob",
            "tenant-b",
            return_kind(json!({}), 404, false, true),
        ),
    ]);
    assert!(evaluate(&config, &hidden).is_empty());

    let mut redacted = allowed;
    redacted.extend([
        principal_event(
            3,
            "deny",
            "getOrder",
            "bob",
            "tenant-b",
            BackendEventKind::Start { input },
        ),
        principal_event(
            4,
            "deny",
            "getOrder",
            "bob",
            "tenant-b",
            return_kind(json!({"secret":null}), 200, true, true),
        ),
    ]);
    assert!(evaluate(&config, &redacted).is_empty());
}

#[test]
fn transaction_atomicity_proves_partial_commit_and_accepts_exact_rollback() {
    let proof = BackendProofContract::TransactionAtomicity {
        operation: "transfer".into(),
        identity_input_path: "$.account".into(),
        snapshot_input_path: "$.revision".into(),
        consistency: ResourceConsistency::Strong,
        failure: ControlledFailureWitness {
            input_path: "$.failAt".into(),
            value: json!("after-debit"),
            statuses: vec![409],
        },
        durable_effects: vec![EffectPattern {
            kind: EffectKind::Write,
            resource: Some("ledger".into()),
            event: None,
            at_least: 0,
            at_most: None,
        }],
    };
    let config = BackendConfig {
        enabled: true,
        operations: vec![proof_operation("transfer")],
        proofs: vec![proof],
        ..BackendConfig::default()
    };
    let start = event(
        1,
        "tx",
        "transfer",
        BackendEventKind::Start {
            input: json!({"account":"a1","revision":"r1","failAt":"after-debit"}),
        },
    );
    let mut partial_write = event(
        2,
        "tx",
        "transfer",
        BackendEventKind::Effect {
            effect: EffectKind::Write,
            resource: Some("ledger".into()),
            key: Some("entry-1".into()),
            tenant: None,
            event: None,
            before: Some(json!({"amount":20})),
            after: Some(json!({"amount":10})),
            payload: None,
        },
    );
    partial_write.action_index = 1;
    let failed = event(
        3,
        "tx",
        "transfer",
        return_kind(json!({}), 409, false, true),
    );
    let partial = vec![start.clone(), partial_write.clone(), failed.clone()];
    let BackendProofContract::TransactionAtomicity {
        durable_effects, ..
    } = &config.proofs[0]
    else {
        unreachable!()
    };
    assert!(matches!(
        failed_atomicity_effect_outcome(&effect_invocation(&partial), durable_effects),
        AtomicityEffectOutcome::Violation(_)
    ));
    let findings = evaluate(&config, &partial);
    let violation = findings
        .iter()
        .find(|violation| violation.oracle == "transaction-atomicity")
        .expect("partial commit should be proven");
    assert_eq!(violation.action_index, 1);

    let rollback = event(
        3,
        "tx",
        "transfer",
        BackendEventKind::Effect {
            effect: EffectKind::Write,
            resource: Some("ledger".into()),
            key: Some("entry-1".into()),
            tenant: None,
            event: None,
            before: Some(json!({"amount":10})),
            after: Some(json!({"amount":20})),
            payload: None,
        },
    );
    let mut rolled_back_return = failed.clone();
    rolled_back_return.sequence = 4;
    let rolled_back = vec![
        start.clone(),
        partial_write.clone(),
        rollback,
        rolled_back_return,
    ];
    assert!(matches!(
        failed_atomicity_effect_outcome(&effect_invocation(&rolled_back), durable_effects),
        AtomicityEffectOutcome::Satisfied
    ));
    assert!(evaluate(&config, &rolled_back).is_empty());

    assert!(matches!(
        failed_atomicity_effect_outcome(&effect_invocation(&[]), durable_effects),
        AtomicityEffectOutcome::Satisfied
    ));
    assert!(evaluate(&config, &[start.clone(), failed.clone()]).is_empty());

    let mut missing_before = partial_write.clone();
    if let BackendEventKind::Effect { before, .. } = &mut missing_before.event {
        *before = None;
    }
    assert!(matches!(
        failed_atomicity_effect_outcome(
            &effect_invocation(std::slice::from_ref(&missing_before)),
            durable_effects,
        ),
        AtomicityEffectOutcome::Abstain
    ));
    assert!(evaluate(&config, &[start.clone(), missing_before, failed.clone()]).is_empty());

    let incomplete = event(
        3,
        "tx",
        "transfer",
        return_kind(json!({}), 409, false, false),
    );
    assert!(evaluate(&config, &[start.clone(), partial_write, incomplete]).is_empty());

    let finding_values = findings.iter().map(finding).collect::<Vec<_>>();
    let guard = FrozenBackendGuard::from_findings(&config, &finding_values).unwrap();
    assert_eq!(guard.proofs, config.proofs);
    let log = partial
        .iter()
        .map(|event| format!("{EVENT_MARKER}{}", serde_json::to_string(event).unwrap()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(guard.reproduces(&log));

    let mut schema_owned = config;
    schema_owned.operations[0].authority = Authority::Schema;
    assert!(evaluate(&schema_owned, &partial).is_empty());
}

fn concurrent_events(second_success: bool, stale_second_write: bool) -> Vec<BackendEvent> {
    let mut events = vec![
        principal_event(
            1,
            "left",
            "updateBalance",
            "alice",
            "tenant-a",
            BackendEventKind::Start {
                input: json!({"id":"a1","snapshot":"s1","version":1,"delta":1}),
            },
        ),
        principal_event(
            2,
            "right",
            "updateBalance",
            "bob",
            "tenant-a",
            BackendEventKind::Start {
                input: json!({"id":"a1","snapshot":"s1","version":1,"delta":1}),
            },
        ),
        principal_event(
            3,
            "left",
            "updateBalance",
            "alice",
            "tenant-a",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("accounts".into()),
                key: Some("a1".into()),
                tenant: Some("tenant-a".into()),
                event: None,
                before: Some(json!({"balance":10})),
                after: Some(json!({"balance":11})),
                payload: None,
            },
        ),
        principal_event(
            4,
            "left",
            "updateBalance",
            "alice",
            "tenant-a",
            return_kind(json!({"version":2}), 200, true, true),
        ),
    ];
    if second_success {
        events.push(principal_event(
            5,
            "right",
            "updateBalance",
            "bob",
            "tenant-a",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("accounts".into()),
                key: Some("a1".into()),
                tenant: Some("tenant-a".into()),
                event: None,
                before: Some(json!({"balance":if stale_second_write {10} else {11}})),
                after: Some(json!({"balance":if stale_second_write {11} else {12}})),
                payload: None,
            },
        ));
    }
    events.push(principal_event(
        6,
        "right",
        "updateBalance",
        "bob",
        "tenant-a",
        if second_success {
            return_kind(json!({"version":2}), 200, true, true)
        } else {
            return_kind(json!({}), 409, false, true)
        },
    ));
    events
}

#[test]
fn concurrency_contracts_prove_double_commit_and_lost_conservation() {
    let base = BackendConfig {
        enabled: true,
        operations: vec![proof_operation("updateBalance")],
        ..BackendConfig::default()
    };
    let optimistic = BackendProofContract::ConcurrentUpdate {
        operation: "updateBalance".into(),
        identity_input_path: "$.id".into(),
        snapshot_input_path: "$.snapshot".into(),
        consistency: ResourceConsistency::Strong,
        policy: ConcurrencyPolicy::OptimisticVersion {
            resource: "accounts".into(),
            version_input_path: "$.version".into(),
            conflict_statuses: vec![409, 412],
        },
    };
    let mut config = base.clone();
    config.proofs = vec![optimistic];
    assert!(evaluate(&config, &concurrent_events(true, true))
        .iter()
        .any(|violation| violation.oracle == "concurrent-update"));
    let conflict = evaluate(&config, &concurrent_events(false, false));
    assert!(conflict.is_empty(), "{conflict:?}");

    config.proofs = vec![BackendProofContract::ConcurrentUpdate {
        operation: "updateBalance".into(),
        identity_input_path: "$.id".into(),
        snapshot_input_path: "$.snapshot".into(),
        consistency: ResourceConsistency::Strong,
        policy: ConcurrencyPolicy::Conserved {
            resource: "accounts".into(),
            delta_input_path: "$.delta".into(),
            before_path: "$.balance".into(),
            after_path: "$.balance".into(),
        },
    }];
    assert!(evaluate(&config, &concurrent_events(true, true))
        .iter()
        .any(|violation| violation.oracle == "concurrent-conservation"));
    assert!(evaluate(&config, &concurrent_events(true, false)).is_empty());
}

#[test]
fn round_trip_integrity_is_typed_exact_and_frozen_for_replay() {
    let proof = BackendProofContract::ResourceRoundTrip {
        write_operation: "putBlob".into(),
        read_operation: "getBlob".into(),
        write_identity_output_path: "$.id".into(),
        read_identity_input_path: "$.id".into(),
        write_snapshot_output_path: "$.revision".into(),
        read_snapshot_input_path: "$.revision".into(),
        consistency: ResourceConsistency::Strong,
        checks: vec![
            RoundTripCheck::Exact {
                write_input_path: "$.content".into(),
                read_output_path: "$.content".into(),
            },
            RoundTripCheck::Utf8Sha256 {
                write_input_path: "$.content".into(),
                read_hash_output_path: "$.sha256".into(),
            },
            RoundTripCheck::ByteSize {
                write_input_path: "$.content".into(),
                read_size_output_path: "$.size".into(),
            },
            RoundTripCheck::MediaType {
                write_input_path: "$.mediaType".into(),
                read_output_path: "$.mediaType".into(),
            },
        ],
    };
    let config = BackendConfig {
        enabled: true,
        operations: vec![proof_operation("putBlob"), proof_operation("getBlob")],
        proofs: vec![proof],
        ..BackendConfig::default()
    };
    let mut events = query_invocation(
        1,
        "write",
        "putBlob",
        json!({"content":"hello","mediaType":"text/plain"}),
        json!({"id":"b1","revision":"r1"}),
    );
    events.extend(query_invocation(
        3,
        "read",
        "getBlob",
        json!({"id":"b1","revision":"r1"}),
        json!({
            "content":"hell0",
            "sha256":hash(b"hello"),
            "size":5,
            "mediaType":"text/plain"
        }),
    ));
    let violations = evaluate(&config, &events);
    assert!(violations
        .iter()
        .any(|violation| violation.oracle == "resource-round-trip"));
    let findings = violations.iter().map(finding).collect::<Vec<_>>();
    let guard = FrozenBackendGuard::from_findings(&config, &findings).unwrap();
    assert_eq!(guard.proofs.len(), 1);
    let log = events
        .iter()
        .map(|event| format!("{EVENT_MARKER}{}", serde_json::to_string(event).unwrap()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(guard.reproduces(&log));

    let clean_hash = hash(b"hello");
    if let BackendEventKind::Return { output, .. } = &mut events[3].event {
        *output = json!({
            "content":"hello",
            "sha256":clean_hash,
            "size":5,
            "mediaType":"text/plain"
        });
    }
    assert!(evaluate(&config, &events).is_empty());
}

#[test]
fn proof_contract_yaml_is_strict_and_transport_independent() {
    let config: BackendConfig = serde_yaml::from_str(
        r#"
enabled: true
proofs:
  - kind: authorization-matrix
    operation: GetOrder
    identityInputPath: $.request.id
    snapshotInputPath: $.request.revision
    consistency: strong
    principals:
      - { actor: owner, tenant: team-a, decision: allow }
      - { actor: outsider, tenant: team-b, decision: deny }
    deny:
      statuses: [401, 403, 404]
      redactedOutputPaths: [$.response.secret]
  - kind: transaction-atomicity
    operation: Transfer
    identityInputPath: $.request.account
    snapshotInputPath: $.request.revision
    consistency: strong
    failure:
      inputPath: $.request.failAt
      value: after-debit
      statuses: [409]
    durableEffects:
      - { kind: write, resource: ledger, atLeast: 0 }
  - kind: concurrent-update
    operation: UpdateOrder
    identityInputPath: $.request.id
    snapshotInputPath: $.request.snapshot
    consistency: strong
    policy:
      kind: optimistic-version
      resource: orders
      versionInputPath: $.request.version
      conflictStatuses: [409, 412]
  - kind: resource-round-trip
    writeOperation: PutBlob
    readOperation: GetBlob
    writeIdentityOutputPath: $.response.id
    readIdentityInputPath: $.request.id
    writeSnapshotOutputPath: $.response.revision
    readSnapshotInputPath: $.request.revision
    consistency: strong
    checks:
      - { kind: exact, writeInputPath: $.request.name, readOutputPath: $.response.name }
"#,
    )
    .unwrap();
    assert_eq!(config.proofs.len(), 4);
    assert!(serde_yaml::from_str::<BackendConfig>(
        r#"
enabled: true
proofs:
  - kind: authorization-matrix
    operation: GetOrder
    identityInputPath: $.id
    snapshotInputPath: $.revision
    consistency: strong
    guessedRole: admin
    principals: []
    deny: { statuses: [403] }
"#,
    )
    .is_err());
}

#[test]
fn local_atomicity_yaml_fixture_is_proven_and_frozen_for_replay() {
    let config: BackendConfig = serde_yaml::from_str(include_str!(
        "../../../../../../validation/backend/atomicity-contract.yaml"
    ))
    .unwrap();
    let log = include_str!("../../../../../../validation/backend/atomicity-partial-commit.ndjson");
    let violations = evaluate(&config, &parse_events(log));
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].oracle, "transaction-atomicity");
    assert_eq!(violations[0].action_index, 3);

    let findings = violations.iter().map(finding).collect::<Vec<_>>();
    let guard = FrozenBackendGuard::from_findings(&config, &findings).unwrap();
    assert_eq!(guard.operations, config.operations);
    assert_eq!(guard.proofs, config.proofs);
    assert!(guard.reproduces(log));
}
