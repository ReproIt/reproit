//! Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//!
//! Eleven SDKs hand implement one contract, so a defect otherwise has to be
//! found eleven times. Four instances of one class landed in a single day, and
//! every group here was written against one of them. The groups are harvested,
//! not invented; each names the defect it pins:
//!
//! - `bounds`: a budget measured in string length rather than encoded bytes
//!   recorded 4096 characters of `€` inline, 12288 bytes, past a budget the
//!   replayer trusts.
//! - `headers`: the 32 header cap applied in arrival order recorded a
//!   different subset per run (Go's defect, repeated by Android and by this
//!   crate, which took 32 off the iterator before sorting). The cap is over
//!   NAME SORTED order, so the generated case is fed scrambled on purpose.
//! - `redaction.typeCases`: the `$reproit` stub must report the ORIGINAL type
//!   and length, not `string` for everything.
//! - `redaction.foldingCases`: secret detection folds case and separators and
//!   matches substrings, so `X-Authorization` and `tokenizer` are secret and
//!   `username` is not.
//! - `redaction.nestingCases`: redaction recurses through objects AND arrays;
//!   a top-level-only scrub shipped nested keys in plaintext.
//! - `redaction.structureCases`: redaction preserves shape. No key dropped, no
//!   array shortened, an explicit null stays a null VALUE. An Android encoder
//!   dropping null map values made a capsule say `{"symbol":"ACME"}` where
//!   production sent `{"prices":null}`, and replay reproduced a DIFFERENT bug.

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

/// `body` verbatim, or `bodyRepeat: [unit, count]` expanded. The euro case
/// only bites when the budget counts ENCODED BYTES, so expansion happens as a
/// String and the bound sees its utf-8 bytes.
#[cfg(feature = "instrument")]
fn body_of(spec: &Value) -> String {
    if let Some(pair) = spec.get("bodyRepeat") {
        let unit = pair[0].as_str().unwrap();
        return unit.repeat(pair[1].as_u64().unwrap() as usize);
    }
    spec.get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Build the generated header table in an order that is neither ascending nor
/// descending: 17 is coprime with 40, so `step * 17 % count` is a permutation.
/// A cap taken before sorting then keeps a visibly wrong subset instead of
/// accidentally passing on already-sorted input.
#[cfg(feature = "instrument")]
fn scrambled_headers(spec: &Value) -> Vec<(String, String)> {
    let count = spec["headerCount"].as_u64().unwrap() as usize;
    let value = spec["value"].as_str().unwrap().to_string();
    // The pattern is `x-h%02d`; Rust has no printf, so the width is applied
    // here and the literal prefix comes from the vector.
    let prefix = spec["namePattern"].as_str().unwrap().replace("%02d", "");
    (0..count)
        .map(|step| (format!("{prefix}{:02}", (step * 17) % count), value.clone()))
        .collect()
}

#[cfg(feature = "instrument")]
#[test]
fn bounds_vectors() {
    let vectors = vectors();
    for case in vectors["bounds"]["cases"].as_array().unwrap() {
        let body = body_of(&case["input"]);
        let content_type = case["input"]["contentType"].as_str().unwrap_or("");
        let actual = Value::Object(reproit_backend::instrument::bounded_body(
            body.as_bytes(),
            content_type,
        ));
        let mut expect = case["expect"].clone();
        if let Some(repeat) = expect.get("body").and_then(|body| body.get("repeat")) {
            let text = repeat[0]
                .as_str()
                .unwrap()
                .repeat(repeat[1].as_u64().unwrap() as usize);
            expect["body"] = Value::String(text);
        }
        assert_eq!(actual, expect, "bounds case {}", case["name"]);
    }
}

#[cfg(feature = "instrument")]
#[test]
fn header_vectors() {
    let vectors = vectors();
    for case in vectors["headers"]["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if let Some(literal) = case["input"].get("headers") {
            let pairs: Vec<(String, String)> = literal
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_string()))
                .collect();
            let actual = Value::Object(reproit_backend::instrument::bounded_headers(
                pairs.into_iter(),
            ));
            assert_eq!(actual, case["expect"], "headers case {name}");
            continue;
        }
        let pairs = scrambled_headers(&case["inputGenerated"]);
        let bounded = reproit_backend::instrument::bounded_headers(pairs.into_iter());
        let kept = bounded["headers"].as_object().unwrap();
        let mut names: Vec<&String> = kept.keys().collect();
        names.sort();
        assert_eq!(
            names.len() as u64,
            case["expect"]["headerCount"].as_u64().unwrap(),
            "headers case {name}"
        );
        assert_eq!(
            names[0].as_str(),
            case["expect"]["firstName"].as_str().unwrap(),
            "the cap must be over sorted names, not the order the headers arrived in"
        );
        assert_eq!(
            names[names.len() - 1].as_str(),
            case["expect"]["lastName"].as_str().unwrap(),
            "the cap must be over sorted names, not the order the headers arrived in"
        );
    }
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
fn redaction_structure_vectors() {
    let vectors = vectors();
    for case in vectors["redaction"]["structureCases"].as_array().unwrap() {
        let actual = reproit_backend::redact(case["input"].clone());
        assert_eq!(actual, case["expect"], "structure case {}", case["name"]);
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
