use super::*;

#[test]
fn graph_joins_runtime_effects_to_declared_operations() {
    let mut config = BackendConfig {
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
    config.programs.push(ProgramSummary {
        language: "rust".into(),
        build: Some("abc123".into()),
        functions: vec![FunctionSummary {
            id: "handlers::create_message".into(),
            name: "create_message".into(),
            source: Some("src/handlers.rs:42".into()),
            operation: Some("createMessage".into()),
            inputs: vec![ValueSlot {
                name: "body".into(),
                domain: ValueDomain::String {
                    min_length: Some(1),
                    max_length: Some(4000),
                    pattern: None,
                    format: None,
                    variants: vec![],
                },
            }],
            output: Some(ValueDomain::Resource {
                resource: "message".into(),
            }),
            calls: vec!["repository::insert_message".into()],
            effects: vec![StaticEffect {
                kind: EffectKind::Write,
                resource: Some("messages".into()),
                event: None,
            }],
            requires: vec!["actor.member_of(room)".into()],
            ensures: vec!["message.exists".into()],
            authority: Authority::Inferred,
        }],
    });
    let events = vec![event(
        1,
        "span-a",
        "createMessage",
        BackendEventKind::Effect {
            effect: EffectKind::Write,
            resource: Some("messages".into()),
            key: Some("m1".into()),
            tenant: Some("tenant-a".into()),
            event: None,
            before: None,
            after: None,
            payload: None,
        },
    )];
    let graph = build_graph(&config, &events);
    assert!(graph.nodes.contains_key("operation:createMessage"));
    assert!(graph
        .nodes
        .contains_key("function:handlers::create_message"));
    assert!(graph
        .nodes
        .contains_key("function:repository::insert_message"));
    assert!(graph.nodes.contains_key("resource:messages"));
    assert!(graph
        .edges
        .iter()
        .any(|edge| edge.relation == GraphRelation::Writes));
    assert!(graph
        .edges
        .iter()
        .any(|edge| edge.relation == GraphRelation::Implements));
    assert!(graph
        .edges
        .iter()
        .any(|edge| edge.relation == GraphRelation::Calls));
}

#[test]
fn read_only_and_missing_effect_oracles_are_exact() {
    let mut read = contract();
    read.id = "getMessage".into();
    read.read_only = true;
    read.promised_effects = vec![EffectPattern {
        kind: EffectKind::Read,
        resource: Some("messages".into()),
        event: None,
        at_least: 1,
        at_most: None,
    }];
    let config = BackendConfig {
        enabled: true,
        origins: vec![],
        schemas: vec![],
        operations: vec![read],
        programs: vec![],
        invariants: vec![],
        resources: vec![],
        proofs: vec![],
        fleet: FleetInvariant::default(),
    };
    let events = vec![
        event(
            1,
            "read",
            "getMessage",
            BackendEventKind::Start { input: json!("m1") },
        ),
        event(
            2,
            "read",
            "getMessage",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("messages".into()),
                key: Some("m1".into()),
                tenant: Some("tenant-a".into()),
                event: None,
                before: None,
                after: Some(json!({"seen":true})),
                payload: None,
            },
        ),
        event(
            3,
            "read",
            "getMessage",
            BackendEventKind::Return {
                output: json!({"id":"m1"}),
                status: Some(201),
                success: true,
                effects_complete: true,
            },
        ),
    ];
    let violations = evaluate(&config, &events);
    assert_eq!(violations.len(), 2);
    assert!(violations.iter().any(|v| v.oracle == "read-only-mutation"));
    assert!(violations.iter().any(|v| v.oracle == "missing-effect"));
}

#[test]
fn incomplete_effect_telemetry_cannot_create_an_absence_finding() {
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
    let events = vec![
        event(
            1,
            "incomplete",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        ),
        event(
            2,
            "incomplete",
            "createMessage",
            BackendEventKind::Return {
                output: json!({"id":"m1"}),
                status: Some(201),
                success: true,
                effects_complete: false,
            },
        ),
    ];
    let violations = evaluate(&config, &events);
    assert!(violations.is_empty(), "{violations:#?}");
}

#[test]
fn upper_effect_bound_confirms_duplicate_side_effects() {
    let mut create = contract();
    create.promised_effects[0].at_most = Some(1);
    let config = BackendConfig {
        enabled: true,
        origins: vec![],
        schemas: vec![],
        operations: vec![create],
        programs: vec![],
        invariants: vec![],
        resources: vec![],
        proofs: vec![],
        fleet: FleetInvariant::default(),
    };
    let events = vec![
        event(
            1,
            "duplicate",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        ),
        event(
            2,
            "duplicate",
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
            "duplicate",
            "createMessage",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("messages".into()),
                key: Some("m2".into()),
                tenant: Some("tenant-a".into()),
                event: None,
                before: None,
                after: Some(json!({"id":"m2"})),
                payload: None,
            },
        ),
        event(
            4,
            "duplicate",
            "createMessage",
            BackendEventKind::Return {
                output: json!({"id":"m1"}),
                status: Some(201),
                success: true,
                effects_complete: true,
            },
        ),
    ];
    let violations = evaluate(&config, &events);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].oracle, "excess-effect");
}

#[test]
fn idempotency_compares_persistent_effects_for_the_same_actor_and_tenant() {
    let mut create = contract();
    create.idempotent = true;
    create.output = None;
    let config = BackendConfig {
        enabled: true,
        origins: vec![],
        schemas: vec![],
        operations: vec![create],
        programs: vec![],
        invariants: vec![],
        resources: vec![],
        proofs: vec![],
        fleet: FleetInvariant::default(),
    };
    let invocation = |sequence, span: &str, key: &str| {
        let mut start = event(
            sequence,
            span,
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        );
        start.idempotency_key = Some("same-key".into());
        vec![
            start,
            event(
                sequence + 1,
                span,
                "createMessage",
                BackendEventKind::Effect {
                    effect: EffectKind::Write,
                    resource: Some("messages".into()),
                    key: Some(key.into()),
                    tenant: Some("tenant-a".into()),
                    event: None,
                    before: None,
                    after: Some(json!({"id":key})),
                    payload: None,
                },
            ),
            event(
                sequence + 2,
                span,
                "createMessage",
                BackendEventKind::Return {
                    output: json!({"id":key}),
                    status: Some(201),
                    success: true,
                    effects_complete: true,
                },
            ),
        ]
    };
    let events = invocation(1, "one", "m1")
        .into_iter()
        .chain(invocation(4, "two", "m2"))
        .collect::<Vec<_>>();
    let violations = evaluate(&config, &events);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].oracle, "idempotency");
}

#[test]
fn idempotent_cached_retry_without_a_second_effect_is_clean() {
    let mut create = contract();
    create.idempotent = true;
    let mut config = BackendConfig {
        enabled: true,
        origins: vec![],
        schemas: vec![],
        operations: vec![create],
        programs: vec![],
        invariants: vec![],
        resources: vec![],
        proofs: vec![],
        fleet: FleetInvariant::default(),
    };
    let mut first = event(
        1,
        "one",
        "createMessage",
        BackendEventKind::Start { input: json!({}) },
    );
    first.idempotency_key = Some("same-key".into());
    let mut retry = event(
        4,
        "two",
        "createMessage",
        BackendEventKind::Start { input: json!({}) },
    );
    retry.idempotency_key = Some("same-key".into());
    let events = vec![
        first,
        event(
            2,
            "one",
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
            "one",
            "createMessage",
            BackendEventKind::Return {
                output: json!({"id":"m1"}),
                status: Some(201),
                success: true,
                effects_complete: true,
            },
        ),
        retry,
        event(
            5,
            "two",
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
            6,
            "two",
            "createMessage",
            BackendEventKind::Return {
                // Generic idempotency does not promise byte-identical
                // responses. A cached retry may use a different success
                // status/body while preserving the same final effect.
                output: json!({"id":"different-response-id"}),
                status: Some(201),
                success: true,
                effects_complete: true,
            },
        ),
    ];
    let violations = evaluate(&config, &events);
    assert!(violations.is_empty(), "{violations:#?}");

    // Byte-identical response replay is a stronger, explicit contract.
    config.operations[0].idempotency_response_replay = IdempotencyResponseReplay::Exact;
    assert!(evaluate(&config, &events)
        .iter()
        .any(|violation| violation.oracle == "idempotency"));

    // A reused key with different request input is caller misuse, not proof
    // that an identical request violated idempotency.
    let mut different_request = events;
    different_request[3].event = BackendEventKind::Start {
        input: json!({"different":true}),
    };
    assert!(!evaluate(&config, &different_request)
        .iter()
        .any(|violation| violation.oracle == "idempotency"));
}

#[test]
fn accepted_input_outside_declared_domain_is_a_hard_finding() {
    let mut create = contract();
    create.input = Some(ValueDomain::Object {
        required: BTreeSet::from(["body".into()]),
        properties: BTreeMap::from([(
            "body".into(),
            ValueDomain::String {
                min_length: Some(1),
                max_length: None,
                pattern: None,
                format: None,
                variants: vec![],
            },
        )]),
        additional: true,
    });
    create.promised_effects.clear();
    let config = BackendConfig {
        enabled: true,
        origins: vec![],
        schemas: vec![],
        operations: vec![create],
        programs: vec![],
        invariants: vec![],
        resources: vec![],
        proofs: vec![],
        fleet: FleetInvariant::default(),
    };
    let events = vec![
        event(
            1,
            "invalid",
            "createMessage",
            BackendEventKind::Start { input: json!({}) },
        ),
        event(
            2,
            "invalid",
            "createMessage",
            BackendEventKind::Return {
                output: json!({"id":"m1"}),
                status: Some(201),
                success: true,
                effects_complete: true,
            },
        ),
    ];
    let violations = evaluate(&config, &events);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].oracle, "accepted-invalid-input");
}

#[test]
fn schema_formats_do_not_reject_valid_edge_cases() {
    let unbounded = ValueDomain::Integer {
        min: None,
        max: None,
    };
    assert!(unbounded.mismatch(&json!(u64::MAX), "$value").is_none());
    assert!(matches_format("date-time", "2026-07-13T03:10:00-07:00"));
    assert!(matches_format("uri", "mailto:person@example.com"));
    assert!(matches_format("email", "\"quoted.local\"@example.test"));
}

#[test]
fn redacted_metadata_preserves_type_and_length_without_content_claims() {
    let secret = json!({"$reproit":{"redacted":true,"type":"string","length":8}});
    let domain = ValueDomain::String {
        min_length: Some(8),
        max_length: Some(12),
        pattern: Some("^visible-content$".into()),
        format: Some("email".into()),
        variants: vec!["visible-content".into()],
    };
    assert!(domain.mismatch(&secret, "$secret").is_none());
    let short = json!({"$reproit":{"redacted":true,"type":"string","length":2}});
    assert_eq!(
        domain.mismatch(&short, "$secret"),
        Some("$secret is shorter than its minimum".into())
    );
    let wrong = json!({"$reproit":{"redacted":true,"type":"boolean"}});
    assert_eq!(
        domain.mismatch(&wrong, "$secret"),
        Some("$secret must be string".into())
    );
}

#[test]
fn marker_parser_abstains_when_recognized_evidence_is_malformed() {
    let log = concat!(
        "unrelated output\n",
        "REPROIT:BACKEND not-json\n",
        "flutter: REPROIT:BACKEND \
             {\"sequence\":1,\"traceId\":\"t\",\"spanId\":\"s\",\"operation\":\"op\",\"kind\":\"\
             start\",\"input\":{}}\n"
    );
    let events = parse_events(log);
    assert!(events.is_empty());
}

#[test]
fn validation_fixture_loads_and_merges_declared_and_schema_contracts() {
    let mut config: BackendConfig = serde_yaml::from_str(include_str!(
        "../../../../../../validation/backend/backend-contract.yaml"
    ))
    .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    config.load_schemas(&root).unwrap();
    assert_eq!(config.operations.len(), 1);
    let operation = &config.operations[0];
    assert_eq!(operation.authority, Authority::Declared);
    assert_eq!(operation.success_statuses, [201]);
    assert!(operation.input.is_some());
    assert!(operation.output.is_some());
    assert_eq!(operation.promised_effects.len(), 2);
    assert_eq!(config.programs.len(), 1);
}

#[test]
fn adversarial_service_fixtures_have_zero_clean_false_positives() {
    let mut config: BackendConfig = serde_yaml::from_str(include_str!(
        "../../../../../../validation/backend/backend-contract.yaml"
    ))
    .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    config.load_schemas(&root).unwrap();

    let clean = evaluate(
        &config,
        &parse_events(include_str!(
            "../../../../../../validation/backend/clean.ndjson"
        )),
    );
    assert!(clean.is_empty(), "clean fixture produced {clean:?}");

    for (log, expected, action) in [
        (
            include_str!("../../../../../../validation/backend/broken-response.ndjson"),
            "response-shape",
            2,
        ),
        (
            include_str!("../../../../../../validation/backend/broken-tenant.ndjson"),
            "tenant-isolation",
            3,
        ),
        (
            include_str!("../../../../../../validation/backend/broken-duplicate.ndjson"),
            "excess-effect",
            4,
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
