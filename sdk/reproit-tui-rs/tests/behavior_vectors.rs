//! Executes the shared behavioral vectors for the FROZEN runner wire, which is
//! deliberately not the capture wire. This SDK is replay only: it never records
//! a capture batch, so it has no inline body budget, no header table and no
//! $reproit placeholder. Its whole shared surface with the rest of the fleet is
//! the secret-key predicate, and eight languages hand implement that predicate.
//! A divergence about which keys count as secret is silent in both directions:
//! too narrow and a credential ships inside a capsule, too wide and a field
//! replay needs is scrubbed into a placeholder that never matches.
//! ../capture-behavior-v1.json states the predicate once so a defect is found
//! once instead of eight times.
//!
//! One difference from the capture wire is deliberate and is asserted here so
//! it cannot be closed by accident: idempotency_key IS secret on the capture
//! wire and is NOT secret here. The runner list is thirteen parts, one shorter,
//! because changing it would change bytes the fuzz harness compares.
//!
//! `secret_rs` and `redact_rs` are private, so the predicate is driven through
//! the public `CausalTransport::capture` instead of widening their visibility:
//! the header slot proves the `<reproit:secret>` placeholder and the body slot
//! proves the length form, both on the bytes that actually reach the wire.

use reproit_tui::CausalTransport;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[test]
fn causal_redaction_folding_cases() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../capture-behavior-v1.json");
    let raw = std::fs::read_to_string(&path).expect("read capture-behavior-v1.json");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("parse vectors");
    let causal = &doc["causalRedaction"];
    let placeholder = causal["placeholder"].as_str().expect("placeholder");
    let cases = causal["foldingCases"].as_array().expect("foldingCases");
    assert!(!cases.is_empty(), "causalRedaction.foldingCases is empty");

    let dir = std::env::temp_dir().join(format!("reproit-tui-vectors-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let network = dir.join("network.ndjson");
    std::env::set_var("REPROIT_NETWORK_FILE", &network);
    std::env::set_var("REPROIT_DEVICE", "a");

    let mut headers = BTreeMap::new();
    let mut body = serde_json::Map::new();
    for case in cases {
        let field = case["field"].as_str().expect("field");
        headers.insert(field.to_string(), "raw-value".to_string());
        body.insert(field.to_string(), serde_json::json!("raw-value"));
    }
    let mut transport = CausalTransport::from_env();
    transport.capture(
        "POST",
        "https://app.test/feed",
        headers,
        Some(serde_json::Value::Object(body)),
        200,
        BTreeMap::new(),
        None,
    );
    let line = std::fs::read_to_string(&network).expect("read network file");
    let exchange: serde_json::Value = serde_json::from_str(line.trim()).expect("parse exchange");
    std::env::remove_var("REPROIT_NETWORK_FILE");
    std::env::remove_var("REPROIT_DEVICE");
    let _ = std::fs::remove_dir_all(&dir);

    for case in cases {
        let field = case["field"].as_str().expect("field");
        let secret = case["secret"].as_bool().expect("secret");
        let header = exchange["requestHeaders"][field].as_str().expect(field);
        let value = exchange["requestBody"][field].as_str().expect(field);
        if secret {
            assert_eq!(header, placeholder, "header {field} should be redacted");
            assert_eq!(value, "<reproit:string:length=9>", "body {field} redaction");
        } else {
            assert_eq!(header, "raw-value", "header {field} must not be scrubbed");
            assert_eq!(value, "raw-value", "body {field} must not be scrubbed");
        }
    }
}
