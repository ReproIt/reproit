use super::*;

#[test]
fn reproit_cloud_schema_and_trace_contracts_catch_json_drift() {
    let mut config: BackendConfig = serde_yaml::from_str(include_str!(
        "../../../../../../validation/backend/cloud-contract.yaml"
    ))
    .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    config.load_schemas(&root).unwrap();

    assert_eq!(config.operations.len(), 5);
    let project = config
        .operations
        .iter()
        .find(|operation| operation.id == "cloudCreateProject")
        .unwrap();
    assert_eq!(project.authority, Authority::Declared);
    assert_eq!(project.success_statuses, [201]);
    assert!(project.input.is_some());
    assert!(project.output.is_some());
    assert!(project.tenant_isolated);

    let clean = evaluate(
        &config,
        &parse_events(include_str!(
            "../../../../../../validation/backend/cloud-clean.ndjson"
        )),
    );
    assert!(clean.is_empty(), "cloud clean trace produced {clean:?}");
    let live_signup = evaluate(
        &config,
        &parse_events(include_str!(
            "../../../../../../validation/backend/cloud-live-signup-clean.ndjson"
        )),
    );
    assert!(
        live_signup.is_empty(),
        "live Cloud signup trace produced {live_signup:?}"
    );

    for (log, expected, action) in [
        (
            include_str!("../../../../../../validation/backend/cloud-broken-shape.ndjson"),
            "response-shape",
            8,
        ),
        (
            include_str!("../../../../../../validation/backend/cloud-broken-input.ndjson"),
            "accepted-invalid-input",
            9,
        ),
        (
            include_str!("../../../../../../validation/backend/cloud-broken-status.ndjson"),
            "response-status",
            10,
        ),
        (
            include_str!("../../../../../../validation/backend/cloud-live-signup-broken.ndjson"),
            "response-shape",
            3,
        ),
    ] {
        let violations = evaluate(&config, &parse_events(log));
        assert_eq!(
            violations.len(),
            1,
            "expected {expected}, got {violations:?}"
        );
        assert_eq!(violations[0].oracle, expected);
        assert_eq!(violations[0].action_index, action);
    }
}

#[test]
fn frozen_guard_preserves_exact_backend_violation_across_trace_positions() {
    let config = BackendConfig {
        enabled: true,
        origins: vec![],
        schemas: vec![],
        operations: vec![contract()],
        programs: vec![],
        invariants: vec![],
        resources: vec![],
        proofs: vec![],
        fleet: FleetInvariant::default(),
    };
    let original = vec![
        event(
            1,
            "span-a",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        ),
        event(
            2,
            "span-a",
            "createMessage",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("messages".into()),
                key: Some("m1".into()),
                tenant: Some("tenant-a".into()),
                event: None,
                before: None,
                after: Some(json!({"id":"m1"})),
                payload: None,
            },
        ),
        event(
            3,
            "span-a",
            "createMessage",
            BackendEventKind::Return {
                output: json!({}),
                status: Some(201),
                success: true,
                effects_complete: true,
            },
        ),
    ];
    let findings = evaluate(&config, &original)
        .iter()
        .map(finding)
        .collect::<Vec<_>>();
    let guard = FrozenBackendGuard::from_findings(&config, &findings).unwrap();
    let mut moved = original;
    for event in &mut moved {
        event.sequence += 40;
        event.trace_id = "different-trace".into();
        event.span_id = "different-span".into();
    }
    let log = moved
        .iter()
        .map(|event| format!("{EVENT_MARKER}{}", serde_json::to_string(event).unwrap()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(guard.reproduces(&log));
}

#[test]
fn authored_invariants_require_a_successful_runtime_witness() {
    let mut config = BackendConfig {
        enabled: true,
        operations: vec![contract()],
        invariants: vec![BackendInvariant::Range {
            operation: "createMessage".into(),
            path: "$.balance".into(),
            min: Some(0.0),
            max: None,
        }],
        ..BackendConfig::default()
    };
    config.operations[0].output = None;
    let events = vec![
        event(
            1,
            "range",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        ),
        event(
            2,
            "range",
            "createMessage",
            BackendEventKind::Return {
                output: json!({"balance": -1}),
                status: Some(201),
                success: true,
                effects_complete: false,
            },
        ),
    ];
    let violations = evaluate(&config, &events);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].oracle, "authored-invariant");

    config.invariants.clear();
    assert!(evaluate(&config, &events).is_empty());
}

#[test]
fn financial_transition_and_fleet_invariants_are_structural() {
    let mut operation = contract();
    operation.output = None;
    let config = BackendConfig {
        enabled: true,
        operations: vec![operation],
        invariants: vec![
            BackendInvariant::Conserved {
                operation: "createMessage".into(),
                left_path: "$.ledger.debits".into(),
                right_path: "$.ledger.credits".into(),
            },
            BackendInvariant::Bounded {
                operation: "createMessage".into(),
                value_path: "$.account.exposure".into(),
                limit_path: "$.account.limit".into(),
            },
            BackendInvariant::Transition {
                operation: "createMessage".into(),
                path: "$.status".into(),
                from: "pending".into(),
                to: vec!["accepted".into(), "rejected".into()],
            },
        ],
        fleet: FleetInvariant {
            same_build: true,
            same_config_contract: true,
        },
        ..BackendConfig::default()
    };
    let mut start = event(
        1,
        "finance",
        "createMessage",
        BackendEventKind::Start {
            input: json!({"status":"pending"}),
        },
    );
    start.build = Some("build-a".into());
    start.config_contract = Some("contract-a".into());
    let mut returned = event(
        2,
        "finance",
        "createMessage",
        BackendEventKind::Return {
            output: json!({
                "status":"cancelled",
                "ledger":{"debits":10,"credits":9},
                "account":{"exposure":11,"limit":10}
            }),
            status: Some(201),
            success: true,
            effects_complete: false,
        },
    );
    returned.build = Some("build-b".into());
    returned.config_contract = Some("contract-b".into());
    let violations = evaluate(&config, &[start, returned]);
    assert_eq!(
        violations
            .iter()
            .filter(|violation| violation.oracle == "authored-invariant")
            .count(),
        3
    );
    assert_eq!(
        violations
            .iter()
            .filter(|violation| violation.oracle == "fleet-consistency")
            .count(),
        2
    );
}

#[test]
fn declarative_backend_invariant_yaml_is_language_independent() {
    let config: BackendConfig = serde_yaml::from_str(
        r#"
enabled: true
invariants:
  - unique: order.id
  - idempotent: submitOrder
  - conserved: ledger.debits == ledger.credits
  - bounded: account.exposure <= account.limit
  - transition: pending -> accepted | rejected
fleet:
  same_build: true
  same_config_contract: true
"#,
    )
    .unwrap();
    assert_eq!(config.invariants.len(), 5);
    assert!(config.fleet.same_build);
    assert!(config.fleet.same_config_contract);
    assert!(matches!(
        &config.invariants[2],
        BackendInvariant::Conserved { left_path, right_path, .. }
            if left_path == "$.ledger.debits" && right_path == "$.ledger.credits"
    ));
    for invariant in &config.invariants {
        let encoded = serde_json::to_value(invariant).unwrap();
        let decoded: BackendInvariant = serde_json::from_value(encoded).unwrap();
        assert_eq!(&decoded, invariant);
    }
}

#[test]
fn unique_invariant_walks_arrays_structurally() {
    let mut operation = contract();
    operation.output = None;
    let config = BackendConfig {
        enabled: true,
        operations: vec![operation],
        invariants: vec![BackendInvariant::Unique {
            operation: "createMessage".into(),
            path: "$.orders.id".into(),
        }],
        ..BackendConfig::default()
    };
    let events = vec![
        event(
            1,
            "unique",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        ),
        event(
            2,
            "unique",
            "createMessage",
            BackendEventKind::Return {
                output: json!({"orders":[{"id":"same"},{"id":"same"}]}),
                status: Some(201),
                success: true,
                effects_complete: false,
            },
        ),
    ];
    let violations = evaluate(&config, &events);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].oracle, "authored-invariant");
}

#[test]
fn imports_raw_proto_with_nested_messages() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("reproit-proto-{}-{}", std::process::id(), nonce));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("service.proto");
    std::fs::write(
        &path,
        r#"syntax = "proto3";
package reproit.validation;
message Envelope {
  message Payload { string name = 1; }
  Payload payload = 1;
}
message Reply { string value = 1; }
service Nested { rpc Send(Envelope) returns (Reply); }
"#,
    )
    .unwrap();
    let document = load_service_document(&path).unwrap();
    let operations = import_service_schema(&document);
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].id, "reproit.validation.Nested/Send");
    let input = operations[0].input.as_ref().unwrap();
    let ValueDomain::Object { properties, .. } = input else {
        panic!("expected message object");
    };
    assert!(matches!(
        properties.get("payload"),
        Some(ValueDomain::Object { .. })
    ));
}
