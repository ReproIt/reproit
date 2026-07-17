#![allow(dead_code)]

use reproit::backend_contracts::{
    evaluate, import_service_schema, parse_events, BackendConfig, BackendEvent,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn read_json(root: &Path, relative: &str) -> Value {
    let path = root.join(relative);
    serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
}

fn events_with_outcome(
    operation: &str,
    output: &Value,
    status: u16,
    success: bool,
) -> Vec<BackendEvent> {
    let log = format!(
        concat!(
            "REPROIT:BACKEND {{\"sequence\":1,\"traceId\":\"bug-zoo\",",
            "\"spanId\":\"request\",\"actionIndex\":1,\"operation\":\"{}\",",
            "\"kind\":\"start\",\"input\":null}}\n",
            "REPROIT:BACKEND {{\"sequence\":2,\"traceId\":\"bug-zoo\",",
            "\"spanId\":\"request\",\"actionIndex\":1,\"operation\":\"{}\",",
            "\"kind\":\"return\",\"output\":{},\"status\":{},",
            "\"success\":{},\"effectsComplete\":false}}"
        ),
        operation, operation, output, status, success
    );
    parse_events(&log)
}

fn events(operation: &str, output: &Value) -> Vec<BackendEvent> {
    events_with_outcome(operation, output, 200, true)
}

fn main() {
    let captured = PathBuf::from(
        std::env::var_os("REPROIT_BUG_ZOO_TMP")
            .expect("REPROIT_BUG_ZOO_TMP must point at captured responses"),
    );
    let buggy_response = read_json(&captured, "buggy/response.json");
    let fixed_response = read_json(&captured, "fixed/response.json");
    let buggy_schema = read_json(&captured, "buggy/openapi.json");
    let fixed_schema = read_json(&captured, "fixed/openapi.json");
    assert_eq!(
        buggy_schema, fixed_schema,
        "the fix must not change the API schema"
    );

    let operation_id = "get_model_a_model_get";
    let schema_operation = import_service_schema(&buggy_schema)
        .into_iter()
        .find(|operation| operation.id == operation_id)
        .expect("FastAPI OpenAPI operation must import");
    let schema_config = BackendConfig {
        enabled: true,
        operations: vec![schema_operation],
        ..BackendConfig::default()
    };
    // OpenAPI 3 defaults object schemas to open content. The generated schema
    // therefore cannot prove that the leaked password is forbidden.
    assert!(evaluate(&schema_config, &events(operation_id, &buggy_response)).is_empty());
    println!("UNSUPPORTED fastapi-889 schema-only: OpenAPI object is not closed");

    let declared: BackendConfig = serde_yaml::from_str(include_str!("../contract.yaml")).unwrap();
    let buggy = evaluate(&declared, &events(operation_id, &buggy_response));
    assert_eq!(buggy.len(), 1, "buggy revision produced {buggy:#?}");
    assert_eq!(buggy[0].oracle, "response-shape");
    assert_eq!(buggy[0].reason, "$output.model_b.password is not declared");
    println!("BUGGY fastapi-889 -> response-shape: {}", buggy[0].reason);

    let fixed = evaluate(&declared, &events(operation_id, &fixed_response));
    assert!(fixed.is_empty(), "fixed revision produced {fixed:#?}");
    println!("FIXED fastapi-889 -> clean");

    let null_buggy_response = read_json(&captured, "fastapi-2719/buggy/response.json");
    let null_buggy_outcome = read_json(&captured, "fastapi-2719/buggy/outcome.json");
    let null_buggy_schema = read_json(&captured, "fastapi-2719/buggy/openapi.json");
    let null_fixed_schema = read_json(&captured, "fastapi-2719/fixed/openapi.json");
    let null_fixed_outcome = read_json(&captured, "fastapi-2719/fixed/outcome.json");
    assert_eq!(
        null_buggy_schema, null_fixed_schema,
        "FastAPI #2719 fix must not change the API schema"
    );
    assert_eq!(null_fixed_outcome["kind"], "rejected");
    assert_eq!(null_fixed_outcome["error_type"], "ValidationError");
    let fixed_error = null_fixed_outcome["message"]
        .as_str()
        .expect("fixed ValidationError must include a message");
    assert!(
        fixed_error.contains("response") && fixed_error.contains("none is not an allowed value"),
        "fixed revision rejected for the wrong reason: {fixed_error}"
    );
    assert_eq!(null_buggy_outcome["kind"], "response");
    let null_buggy_status = null_buggy_outcome["status"]
        .as_u64()
        .and_then(|status| u16::try_from(status).ok())
        .expect("buggy wire outcome must include a u16 HTTP status");

    let null_operation_id = "get_resp__get";
    let null_operation = import_service_schema(&null_buggy_schema)
        .into_iter()
        .find(|operation| operation.id == null_operation_id)
        .expect("FastAPI #2719 OpenAPI operation must import");
    let null_config = BackendConfig {
        enabled: true,
        operations: vec![null_operation],
        ..BackendConfig::default()
    };
    let null_buggy = evaluate(
        &null_config,
        &events_with_outcome(
            null_operation_id,
            &null_buggy_response,
            null_buggy_status,
            true,
        ),
    );
    assert_eq!(
        null_buggy.len(),
        1,
        "FastAPI #2719 buggy revision produced {null_buggy:#?}"
    );
    assert_eq!(null_buggy[0].oracle, "response-shape");
    assert_eq!(null_buggy[0].reason, "$output must be an object");
    println!(
        "BUGGY fastapi-2719 schema-only -> response-shape: {}",
        null_buggy[0].reason
    );
    let null_fixed = evaluate(&null_config, &[]);
    assert!(
        null_fixed.is_empty(),
        "fixed rejected response produced {null_fixed:#?}"
    );
    println!("FIXED fastapi-2719 -> framework rejected invalid response before HTTP success");
}
