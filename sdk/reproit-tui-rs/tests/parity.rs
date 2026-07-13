//! Parity gate: the SDK must reproduce the golden TUI vectors at the repo root
//! (tui_signature_vectors.json) exactly. Those vectors were produced by the real
//! tui.rs runner code. Because this SDK shares the reproit-tui-sig crate with the
//! runner, this is really a guard against the SHARED crate drifting and a check
//! that the SDK re-exports it faithfully; the Go/TS/Python ports rely on the same
//! vectors to prove THEY match.

use reproit_tui::{content_fingerprint, structural_sig};
use std::path::PathBuf;

#[test]
fn golden_tui_vectors_match() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tui_signature_vectors.json");
    let raw = std::fs::read_to_string(&path).expect("read tui_signature_vectors.json");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("parse vectors");
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(
        vectors.len() >= 18,
        "expected >= 18 vectors, got {}",
        vectors.len()
    );
    for v in vectors {
        let name = v["name"].as_str().unwrap_or("?");
        let contents = v["contents"].as_str().unwrap();
        let cur = v["cursor"].as_array().unwrap();
        let cursor = (
            cur[0].as_u64().unwrap() as u16,
            cur[1].as_u64().unwrap() as u16,
        );
        assert_eq!(
            structural_sig(contents, cursor),
            v["expected_sig"].as_str().unwrap(),
            "structural_sig mismatch for vector '{name}'"
        );
        assert_eq!(
            content_fingerprint(contents, cursor),
            v["expected_fp"].as_str().unwrap(),
            "content_fingerprint mismatch for vector '{name}'"
        );
    }
}
