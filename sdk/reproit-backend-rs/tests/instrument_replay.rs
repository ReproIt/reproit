//! Instrument-boundary tests: capture records exchanges with responses onto
//! the ambient trace; replay serves them strictly with no live dependency.
//! One process: REPROIT_REPLAY is latched once, so the replay half runs in a
//! dedicated child test process spawned by the capture half.
#![cfg(all(feature = "axum", feature = "instrument"))]

use axum::routing::get;
use axum::Router;
use reproit_backend::instrument::db::{DbError, DbOutcome};
use reproit_backend::instrument::{self, db, http};
use reproit_backend::{BackendTrace, EffectKind, Recorder, TraceContext};
use serde_json::{json, Value};

fn context() -> TraceContext {
    TraceContext {
        trace_id: "cap-test-1".into(),
        actor: None,
        action_index: 0,
        build: None,
        config_contract: None,
        capture_envelope: true,
        replay_seed: Some("00ff00ff00ff00ff".into()),
    }
}

fn begin() -> BackendTrace {
    BackendTrace::begin(
        context(),
        "GET /quote",
        None,
        None,
        None,
        json!({"query": {"symbol": "ACME"}}),
        Vec::new(),
    )
    .expect("begin")
}

/// Capture side: http::send and db::run attach bounded, redacted exchanges
/// (request AND response) to the ambient trace, and envelope stamps ride
/// capture-mode events only.
#[tokio::test]
async fn capture_records_exchanges_with_responses() {
    let upstream = Router::new().route(
        "/prices",
        get(|| async { axum::Json(json!({"prices": [1, 2], "apiKey": "sk-live-secret"})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.expect("serve");
    });

    let recorder = Recorder::standalone(begin());
    let client = reqwest::Client::new();
    instrument::scope(recorder.clone(), async {
        let request = client
            .get(format!("http://127.0.0.1:{port}/prices?tier=gold"))
            .build()
            .expect("request");
        let response = http::send(&client, request).await.expect("send");
        assert_eq!(response.status, 200);
        assert_eq!(
            response.json::<Value>().expect("json")["prices"],
            json!([1, 2])
        );

        let outcome = db::run(
            "SELECT id FROM issuers WHERE symbol = $1",
            &[json!("ACME")],
            || async {
                Ok::<_, DbError>(DbOutcome {
                    command: Some("SELECT".into()),
                    row_count: 1,
                    rows: vec![json!({"id": 7})],
                })
            },
        )
        .await
        .expect("db");
        assert_eq!(outcome.rows.len(), 1);
    })
    .await;

    let trace = recorder.into_trace().expect("trace");
    let exchanges: Vec<&Value> = trace
        .events()
        .iter()
        .filter_map(|event| event.get("exchange"))
        .collect();
    assert_eq!(exchanges.len(), 2, "http and pg exchanges recorded");
    let http_exchange = exchanges
        .iter()
        .find(|exchange| exchange["protocol"] == "http")
        .expect("http exchange");
    assert_eq!(http_exchange["response"]["status"], 200);
    assert_eq!(http_exchange["response"]["body"]["prices"], json!([1, 2]));
    // Structural redaction applies INSIDE captured exchange bodies.
    assert_eq!(
        http_exchange["response"]["body"]["apiKey"]["$reproit"]["redacted"],
        true
    );
    let pg_exchange = exchanges
        .iter()
        .find(|exchange| exchange["protocol"] == "pg")
        .expect("pg exchange");
    assert_eq!(pg_exchange["response"]["rows"], json!([{"id": 7}]));
    // Envelope stamps ride capture-mode events.
    assert!(trace.events()[0].get("at").is_some());
    assert!(trace.events()[0].get("monoNs").is_some());
    // Scan-time traces stay byte-stable: no stamps.
    let scan = TraceContext::from_header_fn(|name| {
        (name == "x-reproit-trace").then(|| "trace-a".to_string())
    })
    .expect("scan context");
    let scan_trace =
        BackendTrace::begin(scan, "op", None, None, None, Value::Null, Vec::new()).expect("begin");
    assert!(scan_trace.events()[0].get("at").is_none());
}

/// Replay side runs in a child process (REPROIT_REPLAY latches once per
/// process): recorded exchanges serve without any live dependency, and an
/// unmatched call diverges with the structured marker on stderr.
#[test]
fn replay_serves_and_diverges_in_a_child_process() {
    let dir = std::env::temp_dir().join(format!("reproit-rs-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let capture = dir.join("capture.json");
    let payload = json!({
        "format": "reproit-backend-capture",
        "version": 2,
        "operation": "GET /quote",
        "oracle": "backend-server-error",
        "envelope": {"observedAtMs": 1753747200000u64, "tz": "Europe/Berlin",
                     "replaySeed": "00ff00ff00ff00ff"},
        "events": [
            {"kind": "effect", "effect": "read", "exchange": {
                "protocol": "pg",
                "request": {"text": "SELECT 1", "values": [json!("ACME")]},
                "response": {"command": "SELECT", "rowCount": 1, "rows": [{"id": 7}]},
            }},
            {"kind": "effect", "effect": "call", "exchange": {
                "protocol": "http",
                "request": {"method": "GET", "url": "http://pricing.internal/prices?tier=gold"},
                "response": {"status": 200, "headers": {"content-type": "application/json"},
                             "body": {"prices": null}},
            }},
            {"kind": "effect", "effect": "call", "exchange": {
                "protocol": "http",
                "request": {"method": "GET", "url": "http://llm.internal/stream"},
                "response": {"status": 200,
                             "headers": {"content-type": "text/event-stream"},
                             "body": "data: a\n\ndata: b\n\ndata: c\n\n",
                             "stream": {"chunks": [9, 9, 9]}},
            }},
            {"kind": "effect", "effect": "read", "exchange": {
                "protocol": "pg",
                "request": {"text": "SELECT sym FROM issuers"},
                "response": {"command": "SELECT", "rowCount": 1,
                             "rows": [{"sym": "ACME"}]},
            }},
        ],
    });
    std::fs::write(&capture, serde_json::to_vec(&payload).expect("payload")).expect("write");

    let output = std::process::Command::new(std::env::current_exe().expect("exe"))
        .arg("replay_child_serves_recorded_exchanges")
        .arg("--exact")
        .arg("--nocapture")
        .arg("--ignored")
        .env("REPROIT_REPLAY", &capture)
        .output()
        .expect("child test run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "child failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("REPROIT:DIVERGENCE "),
        "structured divergence marker emitted: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The child body: only meaningful under REPROIT_REPLAY (spawned above).
#[tokio::test]
#[ignore = "run by replay_serves_and_diverges_in_a_child_process"]
async fn replay_child_serves_recorded_exchanges() {
    assert!(instrument::replaying(), "REPROIT_REPLAY must be set");
    // Envelope pins: TZ, the clock shim (offset to the capture moment), and
    // the seeded RNG stream.
    assert_eq!(std::env::var("TZ").as_deref(), Ok("Europe/Berlin"));
    let now = instrument::now_millis();
    assert!(
        (1753747200000..1753747200000 + 300_000).contains(&now),
        "now_millis pinned to the capture moment, got {now}"
    );
    let mut rng = instrument::replay_rng().expect("rng");
    let draw = rng.next_f64();
    assert!((0.0..1.0).contains(&draw));

    // pg serves the recorded rows; the live closure must never run.
    let outcome = db::run("SELECT 1", &[json!("ACME")], || async {
        panic!("live database reached during hermetic replay");
        #[allow(unreachable_code)]
        Ok::<_, DbError>(DbOutcome::default())
    })
    .await
    .expect("served");
    assert_eq!(outcome.rows, vec![json!({"id": 7})]);

    // http serves the recorded response with no network.
    let client = reqwest::Client::new();
    let request = client
        .get("http://pricing.internal/prices?tier=gold")
        .build()
        .expect("request");
    let response = http::send(&client, request).await.expect("send");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json::<Value>().expect("json")["prices"],
        Value::Null
    );

    // A recorded stream shape re-serves chunk for chunk through the tee API.
    let request = client
        .get("http://llm.internal/stream")
        .build()
        .expect("request");
    let mut stream = http::send_stream(&client, request).await.expect("stream");
    assert_eq!(stream.status, 200);
    let mut chunks = Vec::new();
    while let Some(chunk) = stream.chunk().await.expect("chunk") {
        chunks.push(String::from_utf8_lossy(&chunk).into_owned());
    }
    assert_eq!(chunks, vec!["data: a\n\n", "data: b\n\n", "data: c\n\n"]);

    // The pg connect stub boots with the database down: no server is dialed
    // and the wrapped query serves the recorded exchange.
    #[cfg(feature = "pg")]
    {
        let db = reproit_backend::pg::connect("host=127.0.0.1 port=9 dbname=absent")
            .await
            .expect("connect stub must not dial");
        let outcome = db
            .query("SELECT sym FROM issuers", &[])
            .await
            .expect("served");
        assert_eq!(outcome.rows, vec![json!({"sym": "ACME"})]);
    }

    // An unmatched call is a divergence: 599, never a guess.
    let request = client
        .get("http://pricing.internal/unknown-endpoint")
        .build()
        .expect("request");
    let response = http::send(&client, request).await.expect("send");
    assert_eq!(response.status, 599);
    assert_eq!(
        response.json::<Value>().expect("json")["reproit"],
        "diverged"
    );

    // Exchange recording API exists on the trace for completeness.
    let mut trace = begin();
    trace
        .exchange(
            EffectKind::Call,
            Some("svc"),
            Some("GET /x"),
            json!({"protocol": "http"}),
        )
        .expect("exchange");
    assert!(trace
        .events()
        .iter()
        .any(|event| event.get("exchange").is_some()));
}

/// Capture side, streaming: request headers ride the exchange, SSE bodies
/// record their observed chunk boundaries, the send_stream TEE records at
/// end of body, and an abandoned stream records nothing.
#[tokio::test]
async fn capture_records_headers_and_stream_boundaries() {
    let upstream = Router::new().route(
        "/stream",
        get(|| async {
            (
                [("content-type", "text/event-stream")],
                "data: a\n\ndata: b\n\n",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        axum::serve(listener, upstream).await.expect("serve");
    });

    let recorder = Recorder::standalone(begin());
    let client = reqwest::Client::new();
    instrument::scope(recorder.clone(), async {
        // Buffered send still tees the network chunks for boundaries.
        let request = client
            .get(format!("http://127.0.0.1:{port}/stream"))
            .header("x-fixture", "yes")
            .build()
            .expect("request");
        let response = http::send(&client, request).await.expect("send");
        assert_eq!(response.status, 200);

        // The streaming API hands chunks to the app as they arrive and
        // records the exchange at end of body.
        let request = client
            .get(format!("http://127.0.0.1:{port}/stream"))
            .build()
            .expect("request");
        let mut stream = http::send_stream(&client, request).await.expect("stream");
        let mut seen = Vec::new();
        while let Some(chunk) = stream.chunk().await.expect("chunk") {
            seen.extend_from_slice(&chunk);
        }
        assert_eq!(seen, b"data: a\n\ndata: b\n\n".to_vec());

        // An abandoned stream (dropped before EOF) records nothing.
        let request = client
            .get(format!("http://127.0.0.1:{port}/stream"))
            .build()
            .expect("request");
        let abandoned = http::send_stream(&client, request).await.expect("stream");
        drop(abandoned);
    })
    .await;

    let trace = recorder.into_trace().expect("trace");
    let exchanges: Vec<&Value> = trace
        .events()
        .iter()
        .filter_map(|event| event.get("exchange"))
        .collect();
    assert_eq!(exchanges.len(), 2, "the abandoned stream records nothing");
    for exchange in &exchanges {
        // Request headers are recorded (name-sorted, lowercased), and the
        // SSE body carries its observed chunk boundaries.
        assert_eq!(exchange["response"]["status"], 200);
        let stream = exchange["response"]["stream"]["chunks"]
            .as_array()
            .expect("sse stream boundaries recorded");
        assert!(!stream.is_empty());
        let total: u64 = stream.iter().filter_map(Value::as_u64).sum();
        assert_eq!(total, 18, "boundaries cover the whole body");
    }
    assert_eq!(exchanges[0]["request"]["headers"]["x-fixture"], "yes");
}
