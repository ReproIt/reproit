use super::*;

#[test]
fn imports_openapi_operations_and_resolves_schema_references() {
    let document = json!({
        "openapi":"3.1.0",
        "paths":{"/messages":{"post":{
            "operationId":"createMessage",
            "responses": {
                "201": {"content": {"application/json": {
                    "schema": {"$ref": "#/components/schemas/Message"}
                }}}
            }
        }}},
        "components": {"schemas": {"Message": {
            "type": "object",
            "required": ["id"],
            "properties": {"id": {"type": "string", "format": "uuid"}}
        }}}
    });
    let operations = import_openapi(&document);
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].authority, Authority::Schema);
    assert_eq!(operations[0].success_statuses, [201]);
    assert!(operations[0]
        .output
        .as_ref()
        .unwrap()
        .mismatch(&json!({}), "$output")
        .is_some());
}

#[test]
fn openapi_30_nullable_accepts_null_without_weakening_non_null_shapes() {
    let document = json!({
        "openapi":"3.0.3",
        "paths":{"/value":{"get":{
            "operationId":"getValue",
            "responses":{"200":{"content":{"application/json":{"schema":{
                "type":"object",
                "required":["nullable","strict"],
                "properties":{
                    "nullable":{"type":"string","nullable":true},
                    "strict":{"type":"string"}
                }
            }}}}}
        }}}
    });
    let output = import_openapi(&document).pop().unwrap().output.unwrap();
    assert!(output
        .mismatch(&json!({"nullable":null,"strict":"ok"}), "$output")
        .is_none());
    assert!(output
        .mismatch(&json!({"nullable":"ok","strict":"ok"}), "$output")
        .is_none());
    assert!(output
        .mismatch(&json!({"nullable":7,"strict":"ok"}), "$output")
        .is_some());
    assert!(output
        .mismatch(&json!({"nullable":null,"strict":null}), "$output")
        .is_some());
}

#[test]
fn openapi_30_nullable_wraps_refs_and_composed_schemas() {
    let document = json!({
        "openapi":"3.0.3",
        "paths":{"/value":{"get":{
            "operationId":"getValue",
            "responses":{"200":{"content":{"application/json":{"schema":{
                "type":"object",
                "required":["refValue","allValue","oneValue","anyValue"],
                "properties":{
                    "refValue":{"$ref":"#/components/schemas/NullableName"},
                    "allValue": {
                        "nullable": true,
                        "allOf": [{"$ref": "#/components/schemas/StrictObject"}]
                    },
                    "oneValue":{"nullable":true,"oneOf":[{"type":"string"},{"type":"integer"}]},
                    "anyValue": {
                        "nullable": true,
                        "anyOf": [
                            {"type": "boolean"},
                            {"type": "array", "items": {"type": "string"}}
                        ]
                    }
                }
            }}}}}
        }}},
        "components":{"schemas":{
            "NullableName":{"type":"string","nullable":true},
            "StrictObject": {
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string"}}
            }
        }}
    });
    let output = import_openapi(&document).pop().unwrap().output.unwrap();
    assert!(output
        .mismatch(
            &json!({"refValue":null,"allValue":null,"oneValue":null,"anyValue":null}),
            "$output"
        )
        .is_none());
    assert!(output
        .mismatch(
            &json!({"refValue":"ok","allValue":{"id":"1"},"oneValue":1,"anyValue":["x"]}),
            "$output"
        )
        .is_none());
    assert!(output
        .mismatch(
            &json!({"refValue":false,"allValue":{},"oneValue":false,"anyValue":"bad"}),
            "$output"
        )
        .is_some());
}

#[test]
fn openapi_31_does_not_treat_legacy_nullable_as_authority() {
    let document = json!({
        "openapi":"3.1.0",
        "paths":{"/value":{"get":{
            "responses":{"200":{"content":{"application/json":{"schema":{
                "type":"string","nullable":true
            }}}}}
        }}}
    });
    let output = import_openapi(&document).pop().unwrap().output.unwrap();
    assert!(output.mismatch(&json!(null), "$output").is_some());
    assert!(output.mismatch(&json!("ok"), "$output").is_none());
}

#[test]
fn pinned_rspec_openapi_nullable_examples_are_valid_but_bad_types_are_not() {
    let document: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/backend/rspec-openapi-nullable.json"
    )))
    .unwrap();
    let operation = import_openapi(&document).pop().unwrap();
    let output = operation.output.unwrap();
    let example = document
        .pointer("/paths/~1nullable/get/responses/200/content/application~1json/example")
        .unwrap();
    assert!(output.mismatch(example, "$output").is_none());
    let mut invalid = example.clone();
    invalid["label"] = json!(7);
    assert!(output.mismatch(&invalid, "$output").is_some());
}

#[test]
fn openapi_imports_exact_parameters_and_only_safe_media() {
    let document = json!({
        "openapi":"3.1.0",
        "paths":{"/projects/{project}/export":{"post":{
            "operationId":"exportProject",
            "parameters":[
                {"in":"path","name":"project","required":true,"schema":{"type":"string"}},
                {
                    "in": "query",
                    "name": "limit",
                    "required": true,
                    "schema": {"type": "integer", "minimum": 1}
                },
                {"in":"header","name":"X-Mode","schema":{"type":"string","enum":["safe"]}},
                {"in":"cookie","name":"session","required":true,"schema":{"type":"string"}},
                {
                    "in": "query",
                    "name": "filter",
                    "style": "deepObject",
                    "schema": {
                        "type": "object",
                        "properties": {"x": {"type": "string"}}
                    }
                }
            ],
            "requestBody":{"required":true,"content":{
                "application/vnd.reproit+json": {"schema": {
                    "type": "object",
                    "required": ["format"],
                    "properties": {"format": {"type": "string"}}
                }},
                "application/xml": {"schema": {
                    "type": "object",
                    "required": ["unsafe"],
                    "properties": {"unsafe": {"type": "string"}}
                }}
            }},
            "responses":{"200":{"content":{
                "text/plain":{"schema":{"type":"string"}},
                "application/octet-stream":{"schema":{"type":"string"}}
            }}}
        }}}
    });
    let operation = import_openapi(&document).pop().unwrap();
    let input = operation.input.unwrap();
    assert!(input
        .mismatch(
            &json!({
                "path":{"project":"p1"},
                "query":{"limit":1},
                "headers":{"x-mode":"safe"},
                "body":{"format":"text"}
            }),
            "$input"
        )
        .is_none());
    assert!(input
        .mismatch(
            &json!({
                "path":{}, "query":{"limit":1}, "body":{"format":"text"}
            }),
            "$input"
        )
        .is_some());
    assert!(operation
        .output
        .unwrap()
        .mismatch(&json!("ok"), "$output")
        .is_none());
}

#[test]
fn openapi_response_shapes_are_bound_to_the_observed_status() {
    let document = json!({"openapi":"3.1.0","paths":{"/items":{"post":{
        "operationId":"createItem","responses":{
            "200": {"content": {"application/json": {"schema": {
                "type": "object",
                "required": ["existing"],
                "properties": {"existing": {"type": "boolean"}}
            }}}},
            "201": {"content": {"application/json": {"schema": {
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string"}}
            }}}}
        }
    }}}});
    let operation = import_openapi(&document).pop().unwrap();
    assert!(operation.outputs_by_status[&200]
        .mismatch(&json!({"existing":true}), "$output")
        .is_none());
    assert!(operation.outputs_by_status[&201]
        .mismatch(&json!({"existing":true}), "$output")
        .is_some());
}

#[test]
fn openapi_recursive_references_are_bounded_without_losing_outer_shape() {
    let document = json!({
        "openapi":"3.1.0",
        "paths":{
            "/nodes":{"get":{"operationId":"getNodes","responses":{"200":{"content":{
                "application/json":{"schema":{"$ref":"#/components/schemas/Node"}}
            }}}}}
        },
        "components":{"schemas":{
            "Node":{"type":"object","required":["name"],"properties":{
                "name":{"type":"string"},
                "parent":{"$ref":"#/components/schemas/Node"},
                "children":{"type":"array","items":{"$ref":"#/components/schemas/Node"}}
            }}
        }}
    });
    let operation = import_openapi(&document).pop().unwrap();
    let output = operation.output.unwrap();
    assert!(output
        .mismatch(
            &json!({"name":"root","parent":null,"children":[]}),
            "$output"
        )
        .is_none());
    assert_eq!(
        output.mismatch(&json!({"parent":null,"children":[]}), "$output"),
        Some("$output.name is required".into())
    );
}

#[test]
fn loads_multifile_openapi_references_hermetically() {
    let root =
        std::env::temp_dir().join(format!("reproit-backend-multifile-{}", std::process::id()));
    std::fs::create_dir_all(root.join("schemas")).unwrap();
    std::fs::write(
        root.join("openapi.yaml"),
        r#"openapi: 3.1.0
paths:
  /users/{id}:
    get:
      operationId: getUser
      parameters:
        - $ref: schemas/parameters.yaml#/Id
      responses:
        "200":
          content:
            application/json:
              schema:
                $ref: schemas/user.yaml#/User
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("schemas/parameters.yaml"),
        r#"Id:
  name: id
  in: path
  required: true
  schema: { type: string }
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("schemas/user.yaml"),
        r#"User:
  type: object
  required: [id, name]
  properties:
    id: { type: string }
    name: { type: string }
"#,
    )
    .unwrap();
    let document = load_service_document(&root.join("openapi.yaml")).unwrap();
    let operation = import_openapi(&document).pop().unwrap();
    assert_eq!(
        operation
            .input
            .as_ref()
            .unwrap()
            .mismatch(&json!({"path":{"id":"u1"}}), "$input"),
        None
    );
    assert_eq!(
        operation
            .output
            .as_ref()
            .unwrap()
            .mismatch(&json!({"id":"u1"}), "$output"),
        Some("$output.name is required".into())
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn imports_graphql_introspection_without_localized_names() {
    let document = json!({"data":{"__schema":{
        "queryType":{"name":"Query"},
        "mutationType":{"name":"Mutation"},
        "types":[
            {"kind": "OBJECT", "name": "Query", "fields": [{
                "name": "message",
                "args": [{"name": "id", "type": {
                    "kind": "NON_NULL",
                    "name": null,
                    "ofType": {"kind": "SCALAR", "name": "ID"}
                }}],
                "type": {"kind": "OBJECT", "name": "Message"}
            }]},
            {"kind": "OBJECT", "name": "Mutation", "fields": [{
                "name": "createMessage",
                "args": [{
                    "name": "body",
                    "type": {"kind": "SCALAR", "name": "String"}
                }],
                "type": {"kind": "OBJECT", "name": "Message"}
            }]},
            {"kind": "OBJECT", "name": "Message", "fields": [
                {"name": "id", "type": {
                    "kind": "NON_NULL",
                    "name": null,
                    "ofType": {"kind": "SCALAR", "name": "ID"}
                }},
                {"name": "body", "type": {"kind": "SCALAR", "name": "String"}}
            ]}
        ]
    }}});
    let operations = import_service_schema(&document);
    assert_eq!(operations.len(), 2);
    assert!(
        operations
            .iter()
            .find(|op| op.id == "message")
            .unwrap()
            .read_only
    );
    assert!(
        !operations
            .iter()
            .find(|op| op.id == "createMessage")
            .unwrap()
            .read_only
    );
    let query = operations.iter().find(|op| op.id == "message").unwrap();
    assert!(query
        .input
        .as_ref()
        .unwrap()
        .mismatch(&json!({}), "$input")
        .is_some());
}

#[test]
fn imports_raw_graphql_sdl_without_a_framework_adapter() {
    let document = graphql_sdl_document(
        r#"
              type Query { account(id: ID!): Account }
              type Account { id: ID!, exposure: Float!, limit: Float! }
            "#,
    )
    .unwrap();
    let operations = import_service_schema(&document);
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].id, "account");
    assert!(operations[0].read_only);
    assert!(operations[0].input.is_some());
    assert!(operations[0].output.is_some());
}

#[test]
fn graphql_output_contract_respects_selection_sets_without_losing_type_checks() {
    // Reduced from the open-source Countries GraphQL API. Both Country
    // fields are NON_NULL in the schema, but selecting only `code` is a
    // complete and valid GraphQL response. Until traces carry a normalized
    // selection set, absence of an unselected field cannot be a finding.
    let document = json!({"data":{"__schema":{
        "queryType":{"name":"Query"},
        "types":[
            {"kind":"OBJECT","name":"Query","fields":[{
                "name":"country",
                "args": [{"name": "code", "type": {
                    "kind": "NON_NULL",
                    "name": null,
                    "ofType": {"kind": "SCALAR", "name": "String"}
                }}],
                "type":{"kind":"OBJECT","name":"Country"}
            }]},
            {"kind":"OBJECT","name":"Country","fields":[
                {"name": "code", "type": {
                    "kind": "NON_NULL",
                    "name": null,
                    "ofType": {"kind": "SCALAR", "name": "String"}
                }},
                {"name": "awsRegion", "type": {
                    "kind": "NON_NULL",
                    "name": null,
                    "ofType": {"kind": "SCALAR", "name": "String"}
                }}
            ]}
        ]
    }}});
    let operations = import_service_schema(&document);
    let country = operations
        .iter()
        .find(|operation| operation.id == "country")
        .unwrap();
    let input = country.input.as_ref().unwrap();
    assert!(input.mismatch(&json!({"code":"US"}), "$input").is_none());
    assert!(input.mismatch(&json!({}), "$input").is_some());

    let output = country.output.as_ref().unwrap();
    assert!(output.mismatch(&json!({"code":"US"}), "$output").is_none());
    assert!(output.mismatch(&Value::Null, "$output").is_none());
    assert_eq!(
        output.mismatch(&json!({"code": 7}), "$output"),
        Some("$output does not match any allowed variant".into())
    );
    let selected = [GraphqlSelection {
        schema_path: "awsRegion".into(),
        response_path: "region".into(),
        type_condition: None,
    }];
    assert!(selection_mismatch(output, &json!({"code":"US"}), &selected)
        .unwrap()
        .contains("region was selected"));
    assert!(selection_mismatch(
        output,
        &json!({"code":"US","region":"us-east-2"}),
        &selected,
    )
    .is_none());
}

#[test]
fn graphql_union_selection_applies_only_to_the_exact_runtime_type() {
    let document = json!({"data":{"__schema":{
        "queryType":{"name":"Query"},
        "types":[
            {"kind": "OBJECT", "name": "Query", "fields": [{
                "name": "search",
                "args": [],
                "type": {"kind": "UNION", "name": "SearchResult"}
            }]},
            {"kind": "UNION", "name": "SearchResult", "possibleTypes": [
                {"kind": "OBJECT", "name": "Human"},
                {"kind": "OBJECT", "name": "Bot"}
            ]},
            {"kind": "OBJECT", "name": "Human", "fields": [{
                "name": "handle",
                "type": {"kind": "NON_NULL", "ofType": {
                    "kind": "SCALAR", "name": "String"
                }}
            }]},
            {"kind": "OBJECT", "name": "Bot", "fields": [{
                "name": "id",
                "type": {"kind": "NON_NULL", "ofType": {
                    "kind": "SCALAR", "name": "ID"
                }}
            }]}
        ]
    }}});
    let operation = import_graphql(&document).pop().unwrap();
    let output = operation.output.unwrap();
    let selected = [GraphqlSelection {
        schema_path: "handle".into(),
        response_path: "name".into(),
        type_condition: Some("Human".into()),
    }];
    assert!(
        selection_mismatch(&output, &json!({"__typename":"Bot","id":"b1"}), &selected,).is_none()
    );
    assert!(
        selection_mismatch(&output, &json!({"__typename":"Human"}), &selected,)
            .unwrap()
            .contains("name was selected")
    );
    assert!(selection_mismatch(
        &output,
        &json!({"__typename":"Human","name":"ada"}),
        &selected,
    )
    .is_none());

    let list = ValueDomain::Array {
        items: Box::new(output.clone()),
        min_items: None,
        max_items: None,
        unique: false,
    };
    assert!(selection_mismatch(
        &list,
        &json!([
            {"__typename":"Bot","id":"b1"},
            {"__typename":"Human","name":7}
        ]),
        &selected,
    )
    .unwrap()
    .contains("$output[1].name"));
}

#[test]
fn imports_protobuf_descriptor_json_as_grpc_operations() {
    let document = json!({"file":[{
        "package":"chat.v1",
        "messageType":[
            {"name":"GetRequest","field":[{"name":"id","type":"TYPE_STRING"}]},
            {"name": "Message", "field": [
                {"name": "id", "type": "TYPE_STRING"},
                {"name": "tags", "type": "TYPE_STRING", "label": "LABEL_REPEATED"}
            ]}
        ],
        "service": [{"name": "Chat", "method": [{
            "name": "Get",
            "inputType": ".chat.v1.GetRequest",
            "outputType": ".chat.v1.Message"
        }]}]
    }]});
    let operations = import_service_schema(&document);
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].id, "chat.v1.Chat/Get");
    assert!(operations[0]
        .output
        .as_ref()
        .unwrap()
        .mismatch(&json!({"id":"m1","tags":["a"]}), "$output")
        .is_none());
}

#[test]
fn protobuf_64_bit_domains_follow_exact_protojson_encoding() {
    let signed = ValueDomain::ProtoInteger64 { signed: true };
    let unsigned = ValueDomain::ProtoInteger64 { signed: false };
    for value in [
        json!("-9223372036854775808"),
        json!("9223372036854775807"),
        json!(0),
        json!(9_007_199_254_740_991_i64),
    ] {
        assert!(signed.mismatch(&value, "$value").is_none(), "{value}");
    }
    for value in [
        json!("0"),
        json!("18446744073709551615"),
        json!(9_007_199_254_740_991_u64),
    ] {
        assert!(unsigned.mismatch(&value, "$value").is_none(), "{value}");
    }
    for value in [
        json!("01"),
        json!("-0"),
        json!("1.0"),
        json!("9223372036854775808"),
        json!(9_007_199_254_740_992_u64),
    ] {
        assert!(signed.mismatch(&value, "$value").is_some(), "{value}");
    }
    for value in [
        json!("-1"),
        json!("18446744073709551616"),
        json!("0x10"),
        json!(9_007_199_254_740_992_u64),
    ] {
        assert!(unsigned.mismatch(&value, "$value").is_some(), "{value}");
    }

    let repeated = ValueDomain::Array {
        items: Box::new(unsigned),
        min_items: None,
        max_items: None,
        unique: false,
    };
    assert!(repeated
        .mismatch(&json!(["0", "18446744073709551615"]), "$values")
        .is_none());
    assert!(repeated.mismatch(&json!(["0", "-1"]), "$values").is_some());
}
