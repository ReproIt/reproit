//! Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//!
//! Eleven SDKs hand implement one contract, so a defect otherwise has to be
//! found eleven times. Four instances of one class landed in a single day, and
//! every group here was written against one of them.

use serde_json::Value;
use std::path::PathBuf;

fn vectors() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sdk dir")
        .join("capture-behavior-v1.json");
    serde_json::from_slice(&std::fs::read(&path).expect("read vectors")).expect("parse vectors")
}

// The bound and the marker live in the instrument module, which is feature
// gated so the core adapter stays dependency light. Without this gate the file
// fails to compile under the default features CI's clippy uses, which is how
// it broke the rust job; the sdk-backend job below builds the feature so the
// constants stay pinned rather than merely skipped.
#[cfg(feature = "instrument")]
#[test]
fn constants_match_the_shared_vectors() {
    let vectors = vectors();
    let constants = &vectors["constants"];
    assert_eq!(
        reproit_backend::instrument::MAX_EXCHANGE_BODY_BYTES as u64,
        constants["maxExchangeBodyBytes"].as_u64().unwrap()
    );
    assert_eq!(
        reproit_backend::instrument::DIVERGENCE_MARKER,
        constants["divergenceMarker"].as_str().unwrap()
    );
}

#[test]
fn redaction_type_vectors() {
    let vectors = vectors();
    for case in vectors["redaction"]["typeCases"].as_array().unwrap() {
        let input = case["input"].clone();
        let actual = reproit_backend::redact(input.clone());
        assert_eq!(actual, case["expect"], "input {input}");
    }
}

#[test]
fn redaction_key_folding_vectors() {
    let vectors = vectors();
    for case in vectors["redaction"]["foldingCases"].as_array().unwrap() {
        let field = case["field"].as_str().unwrap();
        let input = serde_json::json!({ field: "value" });
        let actual = reproit_backend::redact(input.clone());
        let redacted = actual[field].get("$reproit").is_some();
        assert_eq!(
            redacted,
            case["secret"].as_bool().unwrap(),
            "field {field} secrecy"
        );
    }
}

#[test]
fn redaction_nesting_vectors() {
    let vectors = vectors();
    for case in vectors["redaction"]["nestingCases"].as_array().unwrap() {
        let actual = reproit_backend::redact(case["input"].clone());
        assert_eq!(actual, case["expect"], "input {}", case["input"]);
    }
}

#[test]
fn the_trigger_token_is_in_the_protocol_vocabulary() {
    let vectors = vectors();
    let token = vectors["triggerTokens"]["bySdkKind"]["backend"]
        .as_str()
        .unwrap();
    assert!(vectors["triggerTokens"]["allowed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|candidate| candidate == token));
    let source =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/capture.rs"))
            .expect("read capture.rs");
    assert!(source.contains(token), "capture.rs must emit {token}");
    for bad in vectors["triggerTokens"]["rejected"].as_array().unwrap() {
        let bad = bad.as_str().unwrap();
        assert!(
            !source.contains(&format!("\"{bad}\"")),
            "capture.rs must not emit {bad}"
        );
    }
}
