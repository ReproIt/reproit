use super::*;

fn lifecycle_operation(id: &str, status: u16) -> OperationContract {
    OperationContract {
        id: id.into(),
        authority: Authority::Declared,
        input: None,
        output: None,
        outputs_by_status: BTreeMap::new(),
        success_statuses: vec![status],
        read_only: id == "getOrder",
        idempotent: false,
        idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
        tenant_isolated: false,
        promised_effects: vec![],
    }
}

fn lifecycle_resource(consistency: ResourceConsistency) -> ResourceLifecycleContract {
    ResourceLifecycleContract {
        name: "order".into(),
        consistency,
        create: ResourceCreateContract {
            operation: "createOrder".into(),
            output_identity_path: "$.id".into(),
        },
        read: ResourceReadContract {
            operation: "getOrder".into(),
            input_identity_path: "$.id".into(),
            output_identity_path: "$.id".into(),
            absent_statuses: vec![404],
        },
        update: Some(ResourceMutationContract {
            operation: "updateOrder".into(),
            input_identity_path: "$.id".into(),
        }),
        delete: Some(ResourceMutationContract {
            operation: "deleteOrder".into(),
            input_identity_path: "$.id".into(),
        }),
        fields: vec![ResourceFieldContract {
            read_output_path: "$.status".into(),
            create_output_path: Some("$.status".into()),
            update_input_path: Some("$.status".into()),
        }],
    }
}

fn lifecycle_config(consistency: ResourceConsistency) -> BackendConfig {
    BackendConfig {
        enabled: true,
        operations: vec![
            lifecycle_operation("createOrder", 201),
            lifecycle_operation("getOrder", 200),
            lifecycle_operation("updateOrder", 200),
            lifecycle_operation("deleteOrder", 204),
        ],
        resources: vec![lifecycle_resource(consistency)],
        ..BackendConfig::default()
    }
}

#[test]
fn lifecycle_contract_yaml_is_language_independent_and_strict() {
    let config: BackendConfig = serde_yaml::from_str(
        r#"
enabled: true
resources:
  - name: order
    consistency: strong
    create: { operation: createOrder, outputIdentityPath: $.id }
    read:
      operation: getOrder
      inputIdentityPath: $.path.id
      outputIdentityPath: $.id
      absentStatuses: [404]
    update: { operation: updateOrder, inputIdentityPath: $.path.id }
    delete: { operation: deleteOrder, inputIdentityPath: $.path.id }
    fields:
      - createOutputPath: $.status
        updateInputPath: $.body.status
        readOutputPath: $.status
"#,
    )
    .unwrap();
    assert_eq!(config.resources.len(), 1);
    assert_eq!(config.resources[0].consistency, ResourceConsistency::Strong);
    assert_eq!(config.resources[0].read.absent_statuses, [404]);
    assert!(serde_yaml::from_str::<BackendConfig>(
        r#"
enabled: true
resources:
  - name: order
    consistency: guessed
    create: { operation: createOrder, outputIdentityPath: $.id }
    read: { operation: getOrder, inputIdentityPath: $.id, outputIdentityPath: $.id }
"#,
    )
    .is_err());
}

fn invocation_events(
    sequence: u64,
    span: &str,
    operation: &str,
    input: Value,
    status: u16,
    success: bool,
    output: Value,
) -> Vec<BackendEvent> {
    vec![
        event(sequence, span, operation, BackendEventKind::Start { input }),
        event(
            sequence + 1,
            span,
            operation,
            BackendEventKind::Return {
                output,
                status: Some(status),
                success,
                effects_complete: false,
            },
        ),
    ]
}

#[test]
fn lifecycle_proves_create_and_update_read_contradictions() {
    let config = lifecycle_config(ResourceConsistency::Strong);
    let mut create_read = invocation_events(
        1,
        "create",
        "createOrder",
        json!({"status":"pending"}),
        201,
        true,
        json!({"id":"o1","status":"pending"}),
    );
    create_read.extend(invocation_events(
        3,
        "read",
        "getOrder",
        json!({"id":"o1"}),
        200,
        true,
        json!({"id":"o1","status":"cancelled"}),
    ));
    assert!(evaluate(&config, &create_read)
        .iter()
        .any(|violation| violation.oracle == "resource-state"));

    let mut update_read = invocation_events(
        1,
        "create",
        "createOrder",
        json!({}),
        201,
        true,
        json!({"id":"o1","status":"pending"}),
    );
    update_read.extend(invocation_events(
        3,
        "update",
        "updateOrder",
        json!({"id":"o1","status":"accepted"}),
        200,
        true,
        json!({"id":"o1"}),
    ));
    update_read.extend(invocation_events(
        5,
        "read",
        "getOrder",
        json!({"id":"o1"}),
        200,
        true,
        json!({"id":"o1","status":"pending"}),
    ));
    assert!(evaluate(&config, &update_read)
        .iter()
        .any(|violation| violation.oracle == "resource-state"));
}

#[test]
fn lifecycle_proves_missing_create_and_visible_delete() {
    let config = lifecycle_config(ResourceConsistency::Strong);
    let mut missing = invocation_events(
        1,
        "create",
        "createOrder",
        json!({}),
        201,
        true,
        json!({"id":"o1","status":"pending"}),
    );
    missing.extend(invocation_events(
        3,
        "read",
        "getOrder",
        json!({"id":"o1"}),
        404,
        false,
        json!({}),
    ));
    assert!(evaluate(&config, &missing)
        .iter()
        .any(|violation| violation.oracle == "resource-create-missing"));

    let mut visible = invocation_events(
        1,
        "create",
        "createOrder",
        json!({}),
        201,
        true,
        json!({"id":"o1","status":"pending"}),
    );
    visible.extend(invocation_events(
        3,
        "delete",
        "deleteOrder",
        json!({"id":"o1"}),
        204,
        true,
        Value::Null,
    ));
    visible.extend(invocation_events(
        5,
        "read",
        "getOrder",
        json!({"id":"o1"}),
        200,
        true,
        json!({"id":"o1","status":"pending"}),
    ));
    assert!(evaluate(&config, &visible)
        .iter()
        .any(|violation| violation.oracle == "resource-delete-visible"));
}

#[test]
fn lifecycle_abstains_for_eventual_or_ambiguous_identity() {
    let mut events = invocation_events(
        1,
        "create",
        "createOrder",
        json!({}),
        201,
        true,
        json!({"id":["o1"],"status":"pending"}),
    );
    events.extend(invocation_events(
        3,
        "read",
        "getOrder",
        json!({"id":"o1"}),
        404,
        false,
        json!({}),
    ));
    assert!(evaluate(&lifecycle_config(ResourceConsistency::Strong), &events).is_empty());

    let mut eventual = events;
    eventual[1].event = BackendEventKind::Return {
        output: json!({"id":"o1","status":"pending"}),
        status: Some(201),
        success: true,
        effects_complete: false,
    };
    assert!(evaluate(&lifecycle_config(ResourceConsistency::Eventual), &eventual).is_empty());
}
