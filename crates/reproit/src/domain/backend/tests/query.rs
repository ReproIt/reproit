use super::*;

pub(super) fn query_operation(id: &str) -> OperationContract {
    let mut operation = contract();
    operation.id = id.into();
    operation.output = None;
    operation.success_statuses = vec![200];
    operation.read_only = true;
    operation.idempotent = false;
    operation.tenant_isolated = false;
    operation.promised_effects.clear();
    operation
}

fn query_invariant(consistency: ResourceConsistency) -> BackendInvariant {
    BackendInvariant::QuerySemantics {
        operation: "listItems".into(),
        items_path: "$.items".into(),
        identity_path: "$.id".into(),
        consistency,
        filters: vec![QueryFilterContract {
            input_path: "$.status".into(),
            item_path: "$.status".into(),
            comparison: QueryComparison::Equal,
        }],
        sort: Some(QuerySortContract {
            item_path: "$.rank".into(),
            direction: QuerySortDirection::Ascending,
            value_type: QuerySortType::Number,
        }),
        limit_input_path: Some("$.limit".into()),
        pagination: Some(QueryPaginationContract {
            cursor_input_path: "$.cursor".into(),
            next_cursor_output_path: "$.nextCursor".into(),
            snapshot_input_path: "$.snapshot".into(),
            reference_operation: Some("listAllItems".into()),
        }),
    }
}

fn query_config(consistency: ResourceConsistency) -> BackendConfig {
    BackendConfig {
        enabled: true,
        origins: vec![],
        schemas: vec![],
        operations: vec![
            query_operation("listItems"),
            query_operation("listAllItems"),
        ],
        programs: vec![],
        invariants: vec![query_invariant(consistency)],
        resources: vec![],
        proofs: vec![],
        fleet: FleetInvariant::default(),
    }
}

pub(super) fn query_invocation(
    sequence: u64,
    span: &str,
    operation: &str,
    input: Value,
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
                status: Some(200),
                success: true,
                effects_complete: true,
            },
        ),
    ]
}

#[test]
fn query_filter_sort_and_limit_need_authored_typed_contradictions() {
    let config = query_config(ResourceConsistency::Strong);
    let input = json!({"status":"open","limit":2,"snapshot":"r1","cursor":null});
    let clean = query_invocation(
        1,
        "clean",
        "listItems",
        input.clone(),
        json!({"items":[
                {"id":"a","status":"open","rank":1},
                {"id":"b","status":"open","rank":2}
            ],"nextCursor":null}),
    );
    assert!(evaluate(&config, &clean).is_empty());

    let filter = query_invocation(
        1,
        "filter",
        "listItems",
        input.clone(),
        json!({"items":[{"id":"a","status":"closed","rank":1}],"nextCursor":null}),
    );
    assert!(evaluate(&config, &filter)
        .iter()
        .any(|violation| violation.oracle == "authored-invariant"));

    let sort = query_invocation(
        1,
        "sort",
        "listItems",
        input.clone(),
        json!({"items":[
                {"id":"a","status":"open","rank":2},
                {"id":"b","status":"open","rank":1}
            ],"nextCursor":null}),
    );
    assert!(evaluate(&config, &sort)
        .iter()
        .any(|violation| violation.reason.contains("Ascending order")));

    let limit = query_invocation(
        1,
        "limit",
        "listItems",
        input,
        json!({"items":[
                {"id":"a","status":"open","rank":1},
                {"id":"b","status":"open","rank":2},
                {"id":"c","status":"open","rank":3}
            ],"nextCursor":null}),
    );
    assert!(evaluate(&config, &limit)
        .iter()
        .any(|violation| violation.reason.contains("exceeded the authored limit")));
}

#[test]
fn query_semantics_abstain_when_types_paths_or_snapshot_are_unknown() {
    let config = query_config(ResourceConsistency::Strong);
    let missing_typed_sort = query_invocation(
        1,
        "missing",
        "listItems",
        json!({"status":"open","limit":2,"snapshot":"r1","cursor":null}),
        json!({"items":[{"id":"a","status":"open","rank":"first"}],"nextCursor":null}),
    );
    assert!(evaluate(&config, &missing_typed_sort).is_empty());

    let mut pages = query_invocation(
        1,
        "one",
        "listItems",
        json!({"status":"open","limit":1,"cursor":null}),
        json!({"items":[{"id":"a","status":"open","rank":1}],"nextCursor":"c1"}),
    );
    pages.extend(query_invocation(
        3,
        "two",
        "listItems",
        json!({"status":"open","limit":1,"cursor":"c1"}),
        json!({"items":[{"id":"a","status":"open","rank":1}],"nextCursor":null}),
    ));
    assert!(evaluate(&config, &pages).is_empty());
    assert!(evaluate(&query_config(ResourceConsistency::Eventual), &pages).is_empty());
}

#[test]
fn pinned_pagination_proves_duplicate_cursor_and_reference_failures() {
    let config = query_config(ResourceConsistency::Strong);
    let page = |sequence, span: &str, cursor: Value, id: &str, next: Value| {
        query_invocation(
            sequence,
            span,
            "listItems",
            json!({"status":"open","limit":1,"snapshot":"r1","cursor":cursor}),
            json!({"items":[{"id":id,"status":"open","rank":sequence}],"nextCursor":next}),
        )
    };

    let mut clean = page(1, "clean-one", Value::Null, "a", json!("c1"));
    clean.extend(page(3, "clean-two", json!("c1"), "b", Value::Null));
    clean.extend(query_invocation(
        5,
        "clean-reference",
        "listAllItems",
        json!({"status":"open","snapshot":"r1"}),
        json!({"items":[
            {"id":"a","status":"open","rank":1},
            {"id":"b","status":"open","rank":3}
        ]}),
    ));
    assert!(evaluate(&config, &clean).is_empty());

    let mut duplicate = page(1, "one", Value::Null, "a", json!("c1"));
    duplicate.extend(page(3, "two", json!("c1"), "a", Value::Null));
    assert!(evaluate(&config, &duplicate)
        .iter()
        .any(|violation| violation.reason.contains("duplicate identity")));

    let mut looped = page(1, "one", Value::Null, "a", json!("c1"));
    looped.extend(page(3, "two", json!("c1"), "b", json!("c1")));
    assert!(evaluate(&config, &looped)
        .iter()
        .any(|violation| violation.reason.contains("without progress")));

    let mut mismatch = page(1, "one", Value::Null, "a", json!("c1"));
    mismatch.extend(page(3, "two", json!("c1"), "b", Value::Null));
    mismatch.extend(query_invocation(
        5,
        "reference",
        "listAllItems",
        json!({"status":"open","snapshot":"r1"}),
        json!({"items":[
            {"id":"a","status":"open","rank":1},
            {"id":"c","status":"open","rank":3}
        ]}),
    ));
    let violations = evaluate(&config, &mismatch);
    assert!(violations
        .iter()
        .any(|violation| violation.oracle == "query-pagination-reference"));
    let findings = violations.iter().map(finding).collect::<Vec<_>>();
    let guard = FrozenBackendGuard::from_findings(&config, &findings).unwrap();
    let log = mismatch
        .iter()
        .map(|event| format!("{EVENT_MARKER}{}", serde_json::to_string(event).unwrap()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(guard.reproduces(&log));
}

#[test]
fn query_contract_yaml_is_language_independent_and_strict() {
    let config: BackendConfig = serde_yaml::from_str(
        r#"
enabled: true
invariants:
  - kind: query-semantics
    operation: listOrders
    itemsPath: $.items
    identityPath: $.id
    consistency: strong
    filters:
      - inputPath: $.query.status
        itemPath: $.status
        comparison: equal
    sort:
      itemPath: $.createdAt
      direction: descending
      valueType: string
    limitInputPath: $.query.limit
    pagination:
      cursorInputPath: $.query.cursor
      nextCursorOutputPath: $.nextCursor
      snapshotInputPath: $.query.revision
      referenceOperation: listAllOrders
"#,
    )
    .unwrap();
    assert!(matches!(
        config.invariants.as_slice(),
        [BackendInvariant::QuerySemantics { .. }]
    ));
    assert!(serde_yaml::from_str::<BackendConfig>(
        r#"
enabled: true
invariants:
  - kind: query-semantics
    operation: listOrders
    itemsPath: $.items
    identityPath: $.id
    filters:
      - inputPath: $.query.status
        itemPath: $.status
        comparison: contains
"#,
    )
    .is_err());
    assert!(serde_yaml::from_str::<BackendConfig>(
        r#"
enabled: true
invariants:
  - kind: query-semantics
    operation: listOrders
    itemsPath: $.items
    identityPath: $.id
    inferredParameterNames: true
"#,
    )
    .is_err());
}

#[test]
fn query_semantics_abstain_for_inferred_contracts_and_mixed_sessions() {
    let mut config = query_config(ResourceConsistency::Strong);
    config.operations[0].authority = Authority::Inferred;
    let bad_filter = query_invocation(
        1,
        "filter",
        "listItems",
        json!({"status":"open","limit":1,"snapshot":"r1","cursor":null}),
        json!({"items":[{"id":"a","status":"closed","rank":1}],"nextCursor":null}),
    );
    assert!(evaluate(&config, &bad_filter).is_empty());

    let config = query_config(ResourceConsistency::Strong);
    let mut mixed = query_invocation(
        1,
        "one",
        "listItems",
        json!({"status":"open","limit":1,"snapshot":"r1","cursor":null}),
        json!({"items":[{"id":"a","status":"open","rank":1}],"nextCursor":"c1"}),
    );
    mixed.extend(query_invocation(
        3,
        "other-query",
        "listItems",
        json!({"status":"closed","limit":1,"snapshot":"r1","cursor":"c1"}),
        json!({"items":[{"id":"a","status":"closed","rank":1}],"nextCursor":null}),
    ));
    assert!(evaluate(&config, &mixed).is_empty());
}
