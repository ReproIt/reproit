use super::*;

fn document() -> Value {
    serde_json::from_str(
        r#"{
              "openapi":"3.0.3",
              "servers":[{"url":"http://127.0.0.1:9999"}],
              "paths":{"/users/{id}":{"get":{
                "operationId":"getUser",
                "parameters": [{
                    "name": "id",
                    "in": "path",
                    "required": true,
                    "schema": {"type": "integer", "minimum": 1}
                }],
                "responses": {"200": {"content": {"application/json": {"schema": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {"id": {"type": "integer"}}
                }}}}}
              }}}
            }"#,
    )
    .unwrap()
}

#[test]
fn detects_and_builds_a_structural_openapi_request() {
    let document = document();
    let endpoint = openapi_endpoints(&document).pop().unwrap();
    let input = sample_domain(endpoint.contract.input.as_ref().unwrap(), 7, false, 0);
    let request = build_request(&endpoint, "http://127.0.0.1:9999", input).unwrap();
    assert_eq!(request.method, "GET");
    assert_eq!(request.url, "http://127.0.0.1:9999/users/1");
}

#[test]
fn reports_server_errors_for_valid_and_invalid_documented_requests() {
    let endpoint = openapi_endpoints(&document()).pop().unwrap();
    let valid = json!({"path":{"id":1}});
    let request = build_request(&endpoint, "http://127.0.0.1:9999", valid).unwrap();
    let result = evaluate_invocation(&endpoint, &request, 500, json!({"error":"boom"}));
    assert_eq!(result.violations.len(), 1);
    assert_eq!(result.violations[0].oracle, "server-error");
    assert!(result.violations[0].reason.contains("contract-valid"));

    // A 5xx on a contract-invalid input is a crash where the contract
    // requires a 4xx rejection; the finding records the probed mismatch.
    let mut invalid = request.clone();
    invalid.input = json!({"path":{"id":"not-an-integer"}});
    let result = evaluate_invocation(&endpoint, &invalid, 500, json!({"error":"boom"}));
    assert_eq!(result.violations.len(), 1);
    assert_eq!(result.violations[0].oracle, "server-error");
    assert!(result.violations[0].reason.contains("contract-invalid"));
    assert!(result.violations[0]
        .reason
        .contains("$input.path.id must be an integer"));

    // A 4xx rejection of the same invalid input is contract-conformant.
    let mut rejected = request;
    rejected.input = json!({"path":{"id":"not-an-integer"}});
    let result = evaluate_invocation(&endpoint, &rejected, 400, json!({"error":"bad id"}));
    assert!(result.violations.is_empty());
}

fn order_body_domain() -> ValueDomain {
    let mut properties = BTreeMap::new();
    properties.insert(
        "item".into(),
        ValueDomain::String {
            min_length: None,
            max_length: None,
            pattern: None,
            format: None,
            variants: Vec::new(),
        },
    );
    properties.insert(
        "qty".into(),
        ValueDomain::Integer {
            min: Some(1),
            max: None,
        },
    );
    properties.insert(
        "discount".into(),
        ValueDomain::String {
            min_length: None,
            max_length: None,
            pattern: None,
            format: None,
            variants: Vec::new(),
        },
    );
    ValueDomain::Object {
        required: ["item".into(), "qty".into()].into_iter().collect(),
        properties,
        additional: true,
    }
}

#[test]
fn invalid_probes_mutate_each_body_field_including_present_optionals() {
    let domain = order_body_domain();
    let probes = invalid_probes(&domain, 5, true);
    assert_eq!(probes.len(), 3);
    let mismatches = probes
        .iter()
        .map(|probe| domain.mismatch(probe, "$input").expect("out of domain"))
        .collect::<Vec<_>>();
    assert!(mismatches.iter().any(|m| m.contains("$input.item")));
    assert!(mismatches.iter().any(|m| m.contains("$input.qty")));
    assert!(mismatches.iter().any(|m| m.contains("$input.discount")));
    // The optional field is present with the wrong type, not merely absent.
    let discount = probes
        .iter()
        .find(|probe| {
            domain
                .mismatch(probe, "$input")
                .unwrap()
                .contains("discount")
        })
        .unwrap();
    assert!(discount["discount"].is_number());
}

#[test]
fn invalid_probes_only_mutate_the_body_group_of_grouped_inputs() {
    let mut groups = BTreeMap::new();
    let mut path = BTreeMap::new();
    path.insert(
        "id".into(),
        ValueDomain::Integer {
            min: Some(1),
            max: None,
        },
    );
    groups.insert(
        "path".into(),
        ValueDomain::Object {
            required: ["id".into()].into_iter().collect(),
            properties: path,
            additional: true,
        },
    );
    groups.insert("body".into(), order_body_domain());
    let domain = ValueDomain::Object {
        required: ["path".into(), "body".into()].into_iter().collect(),
        properties: groups,
        additional: false,
    };
    let probes = invalid_probes(&domain, 5, false);
    assert_eq!(probes.len(), 3);
    for probe in &probes {
        assert_eq!(probe["path"]["id"], 1, "path group must stay valid");
        let reason = domain.mismatch(probe, "$input").expect("out of domain");
        assert!(reason.contains("$input.body."), "{reason}");
    }
}

#[test]
fn invalid_probes_are_deterministic_and_capped() {
    let domain = order_body_domain();
    assert_eq!(
        invalid_probes(&domain, 9, true),
        invalid_probes(&domain, 9, true)
    );

    let mut properties = BTreeMap::new();
    for index in 0..(MAX_INVALID_PROBES_PER_OPERATION + 5) {
        properties.insert(format!("field{index:02}"), ValueDomain::Boolean);
    }
    let wide = ValueDomain::Object {
        required: BTreeSet::new(),
        properties,
        additional: true,
    };
    assert_eq!(
        invalid_probes(&wide, 1, true).len(),
        MAX_INVALID_PROBES_PER_OPERATION
    );
}

#[test]
fn sample_values_satisfy_their_domains() {
    for domain in [
        ValueDomain::String {
            min_length: Some(12),
            max_length: Some(40),
            pattern: None,
            format: Some("email".into()),
            variants: Vec::new(),
        },
        ValueDomain::Array {
            items: Box::new(ValueDomain::Integer {
                min: Some(2),
                max: Some(8),
            }),
            min_items: Some(2),
            max_items: Some(3),
            unique: false,
        },
    ] {
        let sample = sample_domain(&domain, 3, true, 0);
        assert_eq!(domain.mismatch(&sample, "$"), None, "{sample}");
    }
}

#[test]
fn adversarial_schema_sizes_cannot_force_unbounded_generation() {
    let string = sample_domain(
        &ValueDomain::String {
            min_length: Some(usize::MAX),
            max_length: None,
            pattern: None,
            format: None,
            variants: Vec::new(),
        },
        1,
        true,
        0,
    );
    assert_eq!(
        string.as_str().expect("generated string").chars().count(),
        MAX_GENERATED_STRING_CHARS
    );

    let array = sample_domain(
        &ValueDomain::Array {
            items: Box::new(ValueDomain::Null),
            min_items: Some(usize::MAX),
            max_items: None,
            unique: false,
        },
        1,
        true,
        0,
    );
    assert_eq!(
        array.as_array().expect("generated array").len(),
        MAX_GENERATED_ARRAY_ITEMS
    );
}

#[test]
fn builds_graphql_queries_from_introspection_without_framework_knowledge() {
    let document = json!({"data":{"__schema":{
        "queryType":{"name":"Query"},
        "mutationType":null,
        "subscriptionType":null,
        "types":[
            {"kind":"OBJECT","name":"Query","fields":[{
                "name":"user",
                "args": [{"name": "id", "type": {
                    "kind": "NON_NULL",
                    "name": null,
                    "ofType": {"kind": "SCALAR", "name": "ID", "ofType": null}
                }}],
                "type":{"kind":"OBJECT","name":"User","ofType":null}
            }]},
            {"kind":"OBJECT","name":"User","fields":[
                {"name": "id", "args": [], "type": {
                    "kind": "NON_NULL",
                    "name": null,
                    "ofType": {"kind": "SCALAR", "name": "ID", "ofType": null}
                }},
                {"name":"name","args":[],"type":{"kind":"SCALAR","name":"String","ofType":null}}
            ]}
        ]
    }}});
    let endpoint = graphql_endpoints(&document).pop().unwrap();
    let input = sample_domain(endpoint.contract.input.as_ref().unwrap(), 4, false, 0);
    let request = build_request(&endpoint, "http://127.0.0.1:9999/graphql", input).unwrap();
    let query = request.body.unwrap()["query"].as_str().unwrap().to_string();
    assert!(query.contains("query Reproit($id: ID!)"));
    assert!(query.contains("user(id: $id)"));
    assert!(query.contains("id"));
    assert!(query.contains("name"));
}

#[test]
fn imports_grpc_streaming_as_structural_message_arrays() {
    let document = json!({"file":[{
        "package":"reproit.validation",
        "messageType":[
            {"name":"Request","field":[{"name":"name","type":"TYPE_STRING"}]},
            {"name":"Reply","field":[{"name":"message","type":"TYPE_STRING"}]}
        ],
        "service":[{"name":"Streaming","method":[{
            "name":"Chat",
            "inputType":".reproit.validation.Request",
            "outputType":".reproit.validation.Reply",
            "clientStreaming":true,
            "serverStreaming":true
        }]}]
    }]});
    let endpoint = grpc_endpoints(&document).pop().unwrap();
    assert!(endpoint.client_streaming);
    assert!(endpoint.server_streaming);
    assert!(matches!(
        endpoint.contract.input,
        Some(ValueDomain::Array { .. })
    ));
    assert!(matches!(
        endpoint.contract.output,
        Some(ValueDomain::Array { .. })
    ));
}

#[test]
fn declared_operation_can_make_a_safe_grpc_query_scannable() {
    let mut imported = OperationContract {
        id: "inventory.Reader/Get".into(),
        authority: backend::Authority::Schema,
        input: None,
        output: None,
        outputs_by_status: BTreeMap::new(),
        success_statuses: Vec::new(),
        read_only: false,
        idempotent: false,
        idempotency_response_replay: backend::IdempotencyResponseReplay::Unspecified,
        tenant_isolated: false,
        promised_effects: Vec::new(),
    };
    let mut declared = imported.clone();
    declared.authority = backend::Authority::Declared;
    declared.read_only = true;
    declared.idempotent = true;
    apply_operation_override(&mut imported, &declared);
    assert!(imported.read_only);
    assert!(imported.idempotent);
}

#[test]
fn lifecycle_identity_binding_is_structural_and_does_not_invent_paths() {
    let mut input = json!({"path":{"id":"old"},"body":{"status":"pending"}});
    assert!(set_json_path(&mut input, "$.path.id", json!("new")));
    assert_eq!(input["path"]["id"], "new");
    assert!(!set_json_path(
        &mut input,
        "$.path.missing",
        json!("invented")
    ));
    assert!(input["path"].get("missing").is_none());
}

#[test]
fn lifecycle_endpoint_resolution_abstains_when_operation_is_ambiguous() {
    let endpoint = openapi_endpoints(&document()).pop().unwrap();
    assert!(unique_endpoint(std::slice::from_ref(&endpoint), &endpoint.contract.id).is_some());
    assert!(
        unique_endpoint(&[endpoint.clone(), endpoint.clone()], &endpoint.contract.id).is_none()
    );
}

#[test]
fn lifecycle_replay_rebinds_fresh_resource_identity_into_transport() {
    let endpoint = openapi_endpoints(&document()).pop().unwrap();
    let mut request =
        build_request(&endpoint, "http://127.0.0.1:9999", json!({"path":{"id":1}})).unwrap();
    request.bindings.push(RequestBinding {
        source_step: 0,
        source_output_path: "$.id".into(),
        input_path: "$.path.id".into(),
    });
    assert!(apply_request_bindings(&mut request, &[json!({"id":42})]));
    assert_eq!(request.input["path"]["id"], 42);
    assert_eq!(request.url, "http://127.0.0.1:9999/users/42");
}
