//! Money-test fixture, Rust flavor: an axum app whose GET /quote operation
//! 500s because an upstream pricing service returns `{"prices": null}` and
//! the handler indexes into it.
//!
//! MODE=capture: boots the upstream, runs the failing operation through the
//! instrument boundaries with a standalone recorder, and writes a version-2
//! `reproit-backend-capture` (exchanges + envelope) to CAPTURE_OUT.
//! Default (server) mode: binds ONLY the app on $PORT; with REPROIT_REPLAY
//! set the SDK serves the recorded exchanges, so no upstream and no
//! database exist. FIXED=1 applies the fix.

use axum::routing::get;
use axum::Router;
use reproit_backend::instrument::db::{DbError, DbOutcome};
use reproit_backend::instrument::{self, db, http};
use reproit_backend::{BackendTrace, Recorder, TraceContext};
use serde_json::{json, Value};

async fn pg_lookup(symbol: &str) -> Result<DbOutcome, DbError> {
    let symbol = symbol.to_string();
    db::run(
        "SELECT id, symbol FROM issuers WHERE symbol = $1",
        &[json!(symbol)],
        || async {
            if std::env::var("MODE").as_deref() != Ok("capture") {
                return Err(DbError {
                    message: "live database reached during hermetic replay".into(),
                    code: None,
                });
            }
            Ok(DbOutcome {
                command: Some("SELECT".into()),
                row_count: 1,
                rows: vec![json!({"id": 7, "symbol": symbol})],
            })
        },
    )
    .await
}

/// The planted operation: (status, output) exactly like the Node fixture.
async fn quote(client: &reqwest::Client, upstream: &str, symbol: &str) -> (u16, Value) {
    if pg_lookup(symbol).await.is_err() {
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
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:19971")
        .await
        .expect("bind upstream");
    tokio::spawn(async move {
        axum::serve(upstream_listener, upstream_app)
            .await
            .expect("serve upstream");
    });

    let context = TraceContext {
        trace_id: "cap-money-rs-1".into(),
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
    let (status, output) = instrument::scope(
        recorder.clone(),
        quote(&client, "http://127.0.0.1:19971", "ACME"),
    )
    .await;
    let mut trace = recorder.into_trace().expect("trace back");
    trace
        .finish(output, status, status < 500, true)
        .expect("finish trace");
    let payload = json!({
        "format": "reproit-backend-capture",
        "version": 2,
        "operation": "GET /quote",
        "oracle": "backend-server-error",
        "envelope": {
            "observedAtMs": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis() as u64)
                .unwrap_or(0),
            "runtime": "rust",
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "replaySeed": "c0ffee00c0ffee00",
        },
        "events": trace.events(),
    });
    let out = std::env::var("CAPTURE_OUT").expect("CAPTURE_OUT");
    std::fs::write(&out, serde_json::to_vec(&payload).expect("payload")).expect("write payload");
    println!("capture fixture status {status}");
}

async fn server_mode() {
    instrument::init();
    let client = reqwest::Client::new();
    let app = Router::new().route(
        "/quote",
        get({
            let client = client.clone();
            move |query: axum::extract::RawQuery| async move {
                let symbol = query
                    .0
                    .as_deref()
                    .and_then(|raw| {
                        raw.split('&')
                            .find_map(|pair| pair.strip_prefix("symbol=").map(str::to_string))
                    })
                    .unwrap_or_default();
                let (status, output) = quote(&client, "http://127.0.0.1:19971", &symbol).await;
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
        .unwrap_or(19970);
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
