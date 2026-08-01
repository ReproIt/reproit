//! Golden-byte parity probe for sdk/test/backend_replay_parity_test.js.
//!
//! Reads the shared parity capsule from stdin, performs the same two calls
//! every side performs (serve the recorded SSE exchange, then diverge on a
//! prompt-drift probe), and prints one JSON line the harness byte-compares
//! against the Node reference: served status/body/chunks, the 599 body, and
//! the full `REPROIT:DIVERGENCE` marker line.

use reproit_backend::ojson::OValue;
use reproit_backend::replay::{serve_http, ReplaySession};
use std::io::Read;

fn probe(method: &str, url: &str, body: Option<OValue>) -> OValue {
    let mut fields = vec![
        ("method".to_string(), OValue::Str(method.to_string())),
        ("url".to_string(), OValue::Str(url.to_string())),
    ];
    if let Some(body) = body {
        fields.push(("body".to_string(), body));
    }
    OValue::Obj(fields)
}

fn main() {
    let mut capsule = String::new();
    std::io::stdin()
        .read_to_string(&mut capsule)
        .expect("read capsule from stdin");
    let session = ReplaySession::from_text(&capsule).expect("load capsule");

    let served = serve_http(&session, &probe("GET", "http://llm.internal/stream", None));
    let chunks: Vec<String> = served
        .chunks
        .expect("recorded stream shape")
        .into_iter()
        .map(|chunk| String::from_utf8_lossy(&chunk).into_owned())
        .collect();

    let drift = reproit_backend::ojson::parse(concat!(
        r#"{"messages":[{"role":"user","content":"hello"},"#,
        r#"{"role":"assistant","content":"hi"},"#,
        r#"{"role":"user","content":"DIFFERENT QUESTION"}]}"#,
    ))
    .expect("drift body");
    let diverged = serve_http(
        &session,
        &probe("POST", "http://llm.internal/v1/chat", Some(drift)),
    );
    let marker = session.markers().pop().expect("divergence marker");

    let result = serde_json::json!({
        "serve": {
            "status": served.status,
            "bodyText": served.body_text,
            "chunks": chunks,
        },
        "divergedBody": diverged.body_text,
        "marker": marker,
    });
    println!("{result}");
}
