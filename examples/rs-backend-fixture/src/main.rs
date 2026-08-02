//! Money-test fixture for Rust capsule parity: an axum app whose GET /quote
//! operation 500s because an upstream pricing service returns
//! `{"prices": null}` and the handler indexes into it. The upstream call
//! goes through `instrument::http::send` and the database call through the
//! SDK's tokio-postgres wrapper (`reproit_backend::pg`), a REAL postgres
//! query at capture time and the connect stub at replay.
//!
//! MODE=capture: boots the upstream, connects to the postgres named by
//! REPROIT_PG, runs the failing operation through the instrument boundaries
//! with a standalone recorder, and writes a version-2
//! `reproit-backend-capture` (exchanges + envelope) to CAPTURE_OUT.
//! Default (server) mode: binds ONLY the app on $PORT; with REPROIT_REPLAY
//! set the SDK serves the recorded exchanges and `pg::connect` never dials,
//! so the app boots with the database down and no upstream running.
//! FIXED=1 applies the fix.

use axum::routing::get;
use axum::Router;
use reproit_backend::instrument::{self, http};
use reproit_backend::pg;
use reproit_backend::{determinism_envelope, BackendTrace, Recorder, TraceContext};
use serde_json::{json, Value};
use std::sync::Arc;

const UPSTREAM_PORT: u16 = 19972;
const DEFAULT_PORT: u16 = 19973;

fn conninfo() -> String {
    std::env::var("REPROIT_PG").unwrap_or_else(|_| {
        "host=127.0.0.1 port=15499 user=postgres password=reproit dbname=postgres".to_string()
    })
}

/// The planted operation: (status, output) exactly like the Node and Python
/// fixtures.
async fn quote(
    db: &pg::Client,
    client: &reqwest::Client,
    upstream: &str,
    symbol: &str,
) -> (u16, Value) {
    let lookup = db
        .query(
            "SELECT id, symbol FROM issuers WHERE symbol = $1",
            &[json!(symbol)],
        )
        .await;
    if lookup.is_err() {
        return (500, json!({"error": "internal"}));
    }
    let request = match client.get(format!("{upstream}/prices?tier=gold")).build() {
        Ok(request) => request,
        Err(_) => return (500, json!({"error": "internal"})),
    };
    let Ok(response) = http::send(client, request).await else {
        return (500, json!({"error": "internal"}));
    };
    let Ok(body) = response.json::<Value>() else {
        return (500, json!({"error": "internal"}));
    };
    let prices = &body["prices"];
    if std::env::var("FIXED").as_deref() == Ok("1") && !prices.is_array() {
        return (200, json!({"first": null, "note": "no prices available"}));
    }
    match prices.as_array().and_then(|prices| prices.first()) {
        Some(first) => (200, json!({"first": first})),
        None => (500, json!({"error": "internal"})),
    }
}

async fn capture_mode() {
    let upstream_app = Router::new().route(
        "/prices",
        get(|| async { axum::Json(json!({"prices": null})) }),
    );
    let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", UPSTREAM_PORT))
        .await
        .expect("bind upstream");
    tokio::spawn(async move {
        axum::serve(upstream_listener, upstream_app)
            .await
            .expect("serve upstream");
    });

    let db = pg::connect(&conninfo()).await.expect("connect postgres");
    let context = TraceContext {
        trace_id: "cap-money-rs-parity-1".into(),
        actor: None,
        action_index: 0,
        build: Some("money-fixture".into()),
        config_contract: None,
        capture_envelope: true,
    };
    let trace = BackendTrace::begin(
        context,
        "GET /quote",
        None,
        None,
        None,
        json!({"query": {"symbol": "ACME"}}),
        Vec::new(),
    )
    .expect("begin trace");
    let recorder = Recorder::standalone(trace);
    let client = reqwest::Client::new();
    let upstream = format!("http://127.0.0.1:{UPSTREAM_PORT}");
    let (status, output) =
        instrument::scope(recorder.clone(), quote(&db, &client, &upstream, "ACME")).await;
    let mut trace = recorder.into_trace().expect("trace back");
    trace
        .finish(output, status, status < 500, true)
        .expect("finish trace");
    let observed_at = trace.events()[0].get("at").and_then(Value::as_u64);
    let payload = json!({
        "format": "reproit-backend-capture",
        "version": 2,
        "operation": "GET /quote",
        "oracle": "backend-server-error",
        "envelope": determinism_envelope(observed_at),
        "events": trace.events(),
    });
    let out = std::env::var("CAPTURE_OUT").expect("CAPTURE_OUT");
    std::fs::write(&out, serde_json::to_vec(&payload).expect("payload")).expect("write payload");
    println!("capture fixture status {status}");
}

async fn server_mode() {
    instrument::init();
    // With REPROIT_REPLAY set this is the connect STUB: the app boots with
    // the database down and no socket is dialed.
    let db = Arc::new(pg::connect(&conninfo()).await.expect("connect"));
    let client = reqwest::Client::new();
    let upstream = format!("http://127.0.0.1:{UPSTREAM_PORT}");
    let app = Router::new().route(
        "/quote",
        get({
            move |query: axum::extract::RawQuery| async move {
                let symbol = query
                    .0
                    .as_deref()
                    .and_then(|raw| {
                        raw.split('&')
                            .find_map(|pair| pair.strip_prefix("symbol=").map(str::to_string))
                    })
                    .unwrap_or_default();
                let (status, output) = quote(&db, &client, &upstream, &symbol).await;
                (
                    axum::http::StatusCode::from_u16(status)
                        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
                    axum::Json(output),
                )
            }
        }),
    );
    let port = std::env::var("PORT")
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind app");
    println!("serving on {port}");
    axum::serve(listener, app).await.expect("serve app");
}

#[tokio::main]
async fn main() {
    if std::env::var("MODE").as_deref() == Ok("capture") {
        capture_mode().await;
    } else {
        server_mode().await;
    }
}
