#![allow(dead_code)]

use reproit::backend_contracts::{
    evaluate, import_service_schema, parse_events, BackendConfig, BackendViolation,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn event(sequence: u64, span: &str, operation: &str, fields: &str) -> String {
    format!(
        concat!(
            "REPROIT:BACKEND {{\"sequence\":{sequence},",
            "\"traceId\":\"oss-dogfood\",\"spanId\":\"{span}\",",
            "\"actionIndex\":1,\"operation\":\"{operation}\",",
            "\"actor\":\"alice\",{fields}}}"
        ),
        sequence = sequence,
        span = span,
        operation = operation,
        fields = fields
    )
}

fn evaluate_case(
    document: &Value,
    operation_id: &str,
    input: &Value,
    output: &Value,
) -> Vec<BackendViolation> {
    evaluate_case_with_selections(document, operation_id, input, output, &[])
}

fn evaluate_case_with_selections(
    document: &Value,
    operation_id: &str,
    input: &Value,
    output: &Value,
    selections: &[Value],
) -> Vec<BackendViolation> {
    let operation = import_service_schema(document)
        .into_iter()
        .find(|candidate| candidate.id == operation_id)
        .unwrap_or_else(|| panic!("schema did not import operation {operation_id}"));
    let status = operation.success_statuses.first().copied().unwrap_or(200);
    let config = BackendConfig {
        enabled: true,
        operations: vec![operation],
        ..BackendConfig::default()
    };
    let selection_fields = if selections.is_empty() {
        String::new()
    } else {
        format!(
            "\"selections\":{},",
            serde_json::to_string(selections).unwrap()
        )
    };
    let log = [
        event(
            1,
            operation_id,
            operation_id,
            &format!("\"kind\":\"start\",\"input\":{input}"),
        ),
        event(
            2,
            operation_id,
            operation_id,
            &format!(
                concat!(
                    "{selection_fields}\"kind\":\"return\",\"output\":{output},",
                    "\"status\":{status},\"success\":true,",
                    "\"effectsComplete\":false"
                ),
                selection_fields = selection_fields,
                output = output,
                status = status
            ),
        ),
    ]
    .join("\n");
    evaluate(&config, &parse_events(&log))
}

fn read_json(root: &Path, name: &str) -> Value {
    let path = root.join(name);
    serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
}

fn clean(label: &str, violations: Vec<BackendViolation>) {
    assert!(
        violations.is_empty(),
        "clean OSS case {label} produced {violations:#?}"
    );
    println!("CLEAN {label}");
}

fn broken(label: &str, violations: Vec<BackendViolation>, oracle: &str) {
    assert_eq!(
        violations.len(),
        1,
        "broken OSS case {label} produced {violations:#?}"
    );
    assert_eq!(violations[0].oracle, oracle, "broken OSS case {label}");
    println!("BROKEN {label} -> {oracle}: {}", violations[0].reason);
}

fn evaluate_files(schema_path: &Path, events_path: &Path) {
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(schema_path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", schema_path.display())),
    )
    .unwrap_or_else(|error| panic!("parsing {}: {error}", schema_path.display()));
    let events_text = std::fs::read_to_string(events_path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", events_path.display()));
    let config = BackendConfig {
        enabled: true,
        operations: import_service_schema(&document),
        ..BackendConfig::default()
    };
    let violations = evaluate(&config, &parse_events(&events_text));
    assert!(
        violations.is_empty(),
        "captured backend events violated their schema: {violations:#?}"
    );
    println!("CLEAN captured backend events");
}

fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let [schema, events] = args.as_slice() {
        evaluate_files(Path::new(schema), Path::new(events));
        return;
    }
    let root = PathBuf::from(
        std::env::var_os("REPROIT_OSS_TMP").expect("REPROIT_OSS_TMP must point at captured data"),
    );

    let openapi = read_json(&root, "petstore-openapi.json");
    let operations = import_service_schema(&openapi);
    assert_eq!(operations.len(), 19);
    println!("IMPORTED petstore openapi operations={}", operations.len());
    clean(
        "petstore.addPet.json",
        evaluate_case(
            &openapi,
            "addPet",
            &json!({"id":987654321_i64,"name":"Reproit dogfood","photoUrls":[]}),
            &read_json(&root, "petstore-add.json"),
        ),
    );
    clean(
        "petstore.getPetById.path",
        evaluate_case(
            &openapi,
            "getPetById",
            &json!({"path":{"petId":987654321_i64}}),
            &read_json(&root, "petstore-get.json"),
        ),
    );
    clean(
        "petstore.addPet.form-urlencoded",
        evaluate_case(
            &openapi,
            "addPet",
            &json!({
                "id": 987654322_i64,
                "name": "FormDog",
                "photoUrls": ["https://example.test/dog.png"]
            }),
            &read_json(&root, "petstore-form.json"),
        ),
    );
    clean(
        "petstore.findPetsByStatus.query",
        evaluate_case(
            &openapi,
            "findPetsByStatus",
            &json!({"query":{"status":"available"}}),
            &read_json(&root, "petstore-list.json"),
        ),
    );
    clean(
        "petstore.getInventory.map",
        evaluate_case(
            &openapi,
            "getInventory",
            &Value::Null,
            &read_json(&root, "petstore-inventory.json"),
        ),
    );
    clean(
        "petstore.deletePet.path-header",
        evaluate_case(
            &openapi,
            "deletePet",
            &json!({"path":{"petId":987654321_i64},"headers":{"api_key":"dogfood"}}),
            &Value::Null,
        ),
    );
    let mut missing_name = read_json(&root, "petstore-add.json");
    missing_name.as_object_mut().unwrap().remove("name");
    broken(
        "petstore.addPet.missing-name",
        evaluate_case(
            &openapi,
            "addPet",
            &json!({"name":"Reproit dogfood","photoUrls":[]}),
            &missing_name,
        ),
        "response-shape",
    );

    let countries = read_json(&root, "countries-introspection.json");
    let operations = import_service_schema(&countries);
    assert_eq!(operations.len(), 6);
    println!("IMPORTED countries graphql operations={}", operations.len());
    clean(
        "countries.country.alias-fragment",
        evaluate_case(
            &countries,
            "country",
            &json!({"code":"US"}),
            &read_json(&root, "countries-alias-output.json"),
        ),
    );
    clean(
        "countries.countries.list",
        evaluate_case(
            &countries,
            "countries",
            &json!({"filter":{"code":{"in":["US","CA","MX"]}}}),
            &read_json(&root, "countries-list-output.json"),
        ),
    );
    clean(
        "countries.country.null",
        evaluate_case(&countries, "country", &json!({"code":"ZZ"}), &Value::Null),
    );
    clean(
        "countries.country.partial-error-null",
        evaluate_case(&countries, "country", &json!({"code":"US"}), &Value::Null),
    );
    broken(
        "countries.country.selected-field-type",
        evaluate_case_with_selections(
            &countries,
            "country",
            &json!({"code":"US"}),
            &json!({"name":7}),
            &[json!({"schemaPath":"name","responsePath":"name"})],
        ),
        "response-shape",
    );
    broken(
        "countries.country.missing-required-input",
        evaluate_case(&countries, "country", &json!({}), &Value::Null),
        "accepted-invalid-input",
    );

    let yoga = read_json(&root, "graphql-shapes-introspection.json");
    let operations = import_service_schema(&yoga);
    assert_eq!(operations.len(), 4);
    println!("IMPORTED graphql-shapes operations={}", operations.len());
    clean(
        "graphql-shapes.node.interface-alias-fragment",
        evaluate_case(
            &yoga,
            "node",
            &json!({"kind":"user"}),
            &read_json(&root, "graphql-interface-output.json"),
        ),
    );
    clean(
        "graphql-shapes.search.union-list",
        evaluate_case(
            &yoga,
            "search",
            &Value::Null,
            &read_json(&root, "graphql-union-output.json"),
        ),
    );
    clean(
        "graphql-shapes.nullableNode",
        evaluate_case(&yoga, "nullableNode", &Value::Null, &Value::Null),
    );
    clean(
        "graphql-shapes.error-null",
        evaluate_case(&yoga, "explode", &Value::Null, &Value::Null),
    );
    broken(
        "graphql-shapes.interface-selected-field-type",
        evaluate_case_with_selections(
            &yoga,
            "node",
            &json!({"kind":"user"}),
            &json!({"__typename":"User","id":"u1","name":7,"nickname":null}),
            &[json!({"schemaPath":"name","responsePath":"name","typeCondition":"User"})],
        ),
        "response-shape",
    );
    broken(
        "graphql-shapes.union-member-selected-field-type",
        evaluate_case_with_selections(
            &yoga,
            "search",
            &Value::Null,
            &json!([{"__typename":"User","id":"u1","name":7,"nickname":null}]),
            &[json!({"schemaPath":"name","responsePath":"name","typeCondition":"User"})],
        ),
        "response-shape",
    );

    let grpc = read_json(&root, "grpc-helloworld-descriptor.json");
    let operations = import_service_schema(&grpc);
    assert_eq!(operations.len(), 1);
    println!("IMPORTED grpc-go operations={}", operations.len());
    clean(
        "grpc-go.Greeter.SayHello",
        evaluate_case(
            &grpc,
            "helloworld.Greeter/SayHello",
            &json!({"name":"Reproit"}),
            &read_json(&root, "grpc-helloworld-response.json"),
        ),
    );
    broken(
        "grpc-go.Greeter.SayHello.wrong-type",
        evaluate_case(
            &grpc,
            "helloworld.Greeter/SayHello",
            &json!({"name":"Reproit"}),
            &json!({"message":7}),
        ),
        "response-shape",
    );

    let int64 = read_json(&root, "grpc-int64-descriptor.json");
    clean(
        "protobuf.protojson.int64-string",
        evaluate_case(
            &int64,
            "dogfood.v1.Counters/Get",
            &json!({}),
            &json!({
                "total": "9007199254740993",
                "unsignedTotal": "18446744073709551615",
                "samples": ["1", "2"]
            }),
        ),
    );
    broken(
        "protobuf.protojson.int64-non-decimal",
        evaluate_case(
            &int64,
            "dogfood.v1.Counters/Get",
            &json!({}),
            &json!({"total":"not-an-int64"}),
        ),
        "response-shape",
    );
}
