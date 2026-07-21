use super::proof_contracts::proof_operation;
use super::*;

#[test]
fn openapi_parameter_uniqueness_resolves_refs_and_allows_operation_override() {
    let document = json!({
        "openapi": "3.1.0",
        "components": { "parameters": {
            "Id": {
                "name": "id",
                "in": "path",
                "required": true,
                "schema": {"type": "string"}
            },
            "IdAlias": { "$ref": "#/components/parameters/Id" }
        }},
        "paths": { "/items/{id}": {
            "parameters": [{ "$ref": "#/components/parameters/Id" }],
            "get": {
                "operationId": "getItem",
                "parameters": [{ "$ref": "#/components/parameters/IdAlias" }],
                "responses": { "200": { "description": "ok" } }
            }
        }}
    });
    assert!(validate_openapi_parameter_uniqueness(&document).is_empty());
}

#[test]
fn openapi_parameter_uniqueness_reports_only_duplicates_in_one_list() {
    let document = json!({
        "openapi": "3.1.0",
        "components": { "parameters": {
            "Q": { "name": "q", "in": "query", "schema": { "type": "string" } }
        }},
        "paths": { "/search": { "get": {
            "operationId": "search",
            "parameters": [
                { "$ref": "#/components/parameters/Q" },
                { "name": "q", "in": "query", "schema": { "type": "integer" } }
            ],
            "responses": { "200": { "description": "ok" } }
        }}}
    });
    let violations = validate_openapi_parameter_uniqueness(&document);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].operation, "search");
    assert_eq!(violations[0].oracle, "openapi-parameter-uniqueness");
    assert_eq!(violations[0].pointer, "/paths/~1search/get/parameters/1");

    let distinct_locations = json!({
        "openapi": "3.1.0",
        "paths": { "/search": { "get": {
            "parameters": [
                { "name": "q", "in": "query" },
                { "name": "q", "in": "header" }
            ]
        }}}
    });
    assert!(validate_openapi_parameter_uniqueness(&distinct_locations).is_empty());
}

#[test]
fn openapi_parameter_uniqueness_abstains_on_unresolved_and_cyclic_refs() {
    let document = json!({
        "openapi": "3.1.0",
        "components": { "parameters": {
            "A": { "$ref": "#/components/parameters/B" },
            "B": { "$ref": "#/components/parameters/A" }
        }},
        "paths": { "/safe": { "get": {
            "parameters": [
                { "$ref": "#/components/parameters/A" },
                { "$ref": "#/components/parameters/Missing" }
            ]
        }}}
    });
    assert!(validate_openapi_parameter_uniqueness(&document).is_empty());
}

fn exchange(
    method: &str,
    request_headers: &[(&str, &str)],
    request_body: &[u8],
    status: u16,
    response_headers: &[(&str, &str)],
    response_body: &[u8],
) -> HttpExchangeEvidence {
    HttpExchangeEvidence {
        request_method: method.into(),
        request_target: "/fixture".into(),
        request_headers: request_headers
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect(),
        request_body: request_body.into(),
        response_status: status,
        response_headers: response_headers
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect(),
        response_body: response_body.into(),
    }
}

#[test]
fn byte_range_requires_and_checks_exact_raw_representation() {
    let full = b"0123456789";
    let valid = exchange(
        "GET",
        &[("Range", "bytes=2-5")],
        &[],
        206,
        &[("Content-Range", "bytes 2-5/10"), ("Content-Length", "4")],
        b"2345",
    );
    assert_eq!(validate_http_byte_range(&valid, full), None);

    let wrong = exchange(
        "GET",
        &[("range", "bytes=-4")],
        &[],
        206,
        &[("content-range", "bytes 6-9/10")],
        b"5678",
    );
    assert_eq!(
        validate_http_byte_range(&wrong, full).unwrap().oracle,
        "http-byte-range"
    );

    let encoded = exchange(
        "GET",
        &[("range", "bytes=0-1")],
        &[],
        206,
        &[
            ("content-range", "bytes 0-1/10"),
            ("content-encoding", "gzip"),
        ],
        b"xx",
    );
    assert_eq!(validate_http_byte_range(&encoded, full), None);

    let ignored = exchange(
        "GET",
        &[("range", "bytes=0-1,4-5")],
        &[],
        206,
        &[("content-range", "bytes 0-1/10")],
        b"01",
    );
    assert_eq!(validate_http_byte_range(&ignored, full), None);

    let full_response = exchange("GET", &[("range", "bytes=0-1")], &[], 200, &[], full);
    assert_eq!(validate_http_byte_range(&full_response, full), None);
}

#[test]
fn redirect_transition_checks_the_observed_follow_up_hop() {
    let redirect = exchange("POST", &[], b"payload", 303, &[("Location", "/next")], &[]);
    let valid = exchange("GET", &[], &[], 200, &[], &[]);
    assert_eq!(validate_http_redirect_transition(&redirect, &valid), None);
    let wrong = exchange("POST", &[], b"payload", 200, &[], &[]);
    assert_eq!(
        validate_http_redirect_transition(&redirect, &wrong)
            .unwrap()
            .oracle,
        "http-redirect-transition"
    );

    let preserve = exchange("PUT", &[], b"payload", 307, &[("location", "/next")], &[]);
    let dropped = exchange("PUT", &[], &[], 200, &[], &[]);
    assert!(validate_http_redirect_transition(&preserve, &dropped).is_some());

    let historical = exchange("POST", &[], b"payload", 302, &[("location", "/next")], &[]);
    let rewritten = exchange("GET", &[], &[], 200, &[], &[]);
    assert_eq!(
        validate_http_redirect_transition(&historical, &rewritten),
        None
    );
    let preserved = exchange("POST", &[], b"payload", 200, &[], &[]);
    assert_eq!(
        validate_http_redirect_transition(&historical, &preserved),
        None
    );

    let non_redirect = exchange("POST", &[], b"payload", 300, &[], &[]);
    assert_eq!(
        validate_http_redirect_transition(&non_redirect, &rewritten),
        None
    );
}

#[test]
fn websocket_checks_only_explicit_route_auth_and_message_contracts() {
    let contract = WebSocketContract {
        route: "/chat".into(),
        allowed_principals: BTreeSet::from(["member".into()]),
        denied_principals: BTreeSet::from(["blocked".into()]),
        allowed_client_messages: vec![ValueDomain::Object {
            required: BTreeSet::from(["text".into()]),
            properties: BTreeMap::from([(
                "text".into(),
                ValueDomain::String {
                    min_length: None,
                    max_length: None,
                    pattern: None,
                    format: None,
                    variants: Vec::new(),
                },
            )]),
            additional: false,
        }],
        allowed_server_messages: Vec::new(),
        denied_close_codes: BTreeSet::from([1011]),
    };
    let evidence = WebSocketEvidence {
        route: "/chat".into(),
        principal: "blocked".into(),
        accepted: true,
        close_code: Some(1011),
        client_messages: vec![json!({"unexpected": true})],
        server_messages: vec![json!({"not": "checked"})],
    };
    let violations = validate_websocket_contract(&contract, &evidence);
    assert_eq!(violations.len(), 3);
    assert!(violations
        .iter()
        .any(|value| value.oracle == "websocket-authorization"));
    assert!(violations
        .iter()
        .any(|value| value.oracle == "websocket-close"));
    assert!(violations
        .iter()
        .any(|value| value.oracle == "websocket-message"));

    let unknown = WebSocketEvidence {
        route: "/other".into(),
        principal: "unknown".into(),
        accepted: true,
        close_code: None,
        client_messages: vec![Value::Null],
        server_messages: Vec::new(),
    };
    assert!(validate_websocket_contract(&contract, &unknown).is_empty());

    let unlisted_principal = WebSocketEvidence {
        route: "/chat".into(),
        principal: "observer".into(),
        accepted: true,
        close_code: None,
        client_messages: Vec::new(),
        server_messages: vec![Value::Null],
    };
    assert!(validate_websocket_contract(&contract, &unlisted_principal).is_empty());
}

#[test]
fn protocol_proofs_flow_through_evaluation_and_frozen_replay() {
    let operation = proof_operation("download");
    let config = BackendConfig {
        enabled: true,
        operations: vec![operation],
        ..BackendConfig::default()
    };
    let event = BackendEvent {
        sequence: 1,
        trace_id: "trace-protocol".into(),
        span_id: "span-protocol".into(),
        action_index: 4,
        parent_span_id: None,
        operation: "download".into(),
        build: None,
        config_contract: None,
        actor: None,
        tenant: None,
        idempotency_key: None,
        selections: Vec::new(),
        event: BackendEventKind::Protocol {
            proof: ProtocolEvidence::HttpByteRange {
                exchange: exchange(
                    "GET",
                    &[("range", "bytes=1-3")],
                    &[],
                    206,
                    &[("content-range", "bytes 1-3/5")],
                    b"bad",
                ),
                authoritative_full_representation: b"abcde".to_vec(),
            },
        },
    };
    let violations = evaluate(&config, std::slice::from_ref(&event));
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].oracle, "http-byte-range");
    assert_eq!(violations[0].action_index, 4);

    let finding = finding(&violations[0]);
    let guard = FrozenBackendGuard::from_findings(&config, &[finding]).unwrap();
    let log = format!("{EVENT_MARKER}{}", serde_json::to_string(&event).unwrap());
    assert!(guard.reproduces(&log));
}

#[test]
fn authored_lifecycle_protocol_flows_through_backend_evaluation() {
    let config = BackendConfig {
        enabled: true,
        operations: vec![proof_operation("worker")],
        ..BackendConfig::default()
    };
    let event = BackendEvent {
        sequence: 1,
        trace_id: "trace-lifecycle".into(),
        span_id: "span-lifecycle".into(),
        action_index: 7,
        parent_span_id: None,
        operation: "worker".into(),
        build: None,
        config_contract: None,
        actor: None,
        tenant: None,
        idempotency_key: None,
        selections: Vec::new(),
        event: BackendEventKind::Protocol {
            proof: ProtocolEvidence::Lifecycle {
                contract: ProtocolLifecycleContract {
                    scope_kind: "worker".into(),
                    rules: vec![ProtocolLifecycleRule::ForbidAfter {
                        event: "callback".into(),
                        boundary: "worker.close".into(),
                    }],
                },
                evidence: ProtocolLifecycleEvidence {
                    scope_kind: "worker".into(),
                    scope_id: "worker-17".into(),
                    complete: true,
                    events: vec![
                        ProtocolLifecycleEvent {
                            sequence: 0,
                            name: "worker.close".into(),
                            scope_id: "worker-17".into(),
                        },
                        ProtocolLifecycleEvent {
                            sequence: 1,
                            name: "callback".into(),
                            scope_id: "worker-17".into(),
                        },
                    ],
                },
            },
        },
    };

    let violations = evaluate(&config, &[event]);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].oracle, "lifecycle-forbid-after");
    assert_eq!(violations[0].action_index, 7);
}
