//! Axum fixture for the reproit-backend production capture mode.
//!
//! One endpoint, one planted production bug: `POST /orders` assumes the
//! optional `discount` field is always a string, so a numeric discount
//! panics the order logic and returns HTTP 500. Every request is traced
//! with the normal begin/effect/finish machinery; the capture sampler
//! (not an `x-reproit-trace` header) decides what leaves the process.
//!
//! Environment: REPROIT_CAPTURE_ENDPOINT, REPROIT_CAPTURE_KEY,
//! REPROIT_CAPTURE_APP, optional REPROIT_CAPTURE_BUILD. Without them the
//! service still runs; capture is simply disabled.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::post;
use axum::Router;
use reproit_backend::{BackendTrace, Capture, CaptureConfig, EffectKind, HttpInput, TraceContext};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

struct AppState {
    capture: Option<Capture>,
    next_order: AtomicU64,
}

fn capture_from_env() -> Option<Capture> {
    let endpoint = std::env::var("REPROIT_CAPTURE_ENDPOINT").ok()?;
    let key = std::env::var("REPROIT_CAPTURE_KEY").ok()?;
    let app = std::env::var("REPROIT_CAPTURE_APP").ok()?;
    let mut config = CaptureConfig::new(endpoint, key, app);
    config.build = std::env::var("REPROIT_CAPTURE_BUILD").ok();
    config.flush_interval = std::time::Duration::from_millis(500);
    Capture::new(config)
}

/// The planted production bug: a numeric `discount` panics the "business
/// logic", which the handler surfaces as HTTP 500.
fn order_summary(body: &Value) -> (i64, Option<String>) {
    let quantity = body["qty"].as_i64().unwrap_or(1);
    let discount = body
        .get("discount")
        .map(|value| value.as_str().unwrap().to_string());
    (quantity, discount)
}

async fn create_order(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    // Capture is optional: without it the request is still traced locally,
    // and nothing leaves the process.
    let context = match &state.capture {
        Some(capture) => capture.context(),
        None => TraceContext {
            trace_id: "local-only".into(),
            actor: None,
            action_index: 0,
            build: None,
            config_contract: None,
        },
    };
    let input = HttpInput {
        body: Some(body.clone()),
        ..HttpInput::default()
    };
    let Ok(mut trace) = BackendTrace::begin(
        context,
        "createOrder",
        None,
        None,
        None,
        input.into_value(),
        Vec::new(),
    ) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})));
    };
    let item = body["item"].as_str().unwrap_or("unknown").to_string();
    let _ = trace.effect(
        EffectKind::Read,
        Some("inventory"),
        Some(&item),
        None,
        None,
        None,
    );
    let body_for_logic = body.clone();
    match std::panic::catch_unwind(move || order_summary(&body_for_logic)) {
        Ok((quantity, discount)) => {
            let id = state.next_order.fetch_add(1, Ordering::Relaxed);
            let key = format!("order-{id}");
            let _ = trace.effect(
                EffectKind::Write,
                Some("orders"),
                Some(&key),
                None,
                None,
                None,
            );
            let output = json!({ "id": id, "item": item, "qty": quantity, "discount": discount });
            let _ = trace.finish(output.clone(), 201, true, true);
            if let Some(capture) = &state.capture {
                capture.record(&trace);
            }
            (StatusCode::CREATED, Json(output))
        }
        Err(_) => {
            let output = json!({ "error": "internal server error" });
            let _ = trace.finish(output.clone(), 500, false, true);
            if let Some(capture) = &state.capture {
                capture.record(&trace);
            }
            (StatusCode::INTERNAL_SERVER_ERROR, Json(output))
        }
    }
}

#[tokio::main]
async fn main() {
    let capture = capture_from_env();
    if capture.is_none() {
        eprintln!("capture disabled (set REPROIT_CAPTURE_ENDPOINT/KEY/APP)");
    }
    let state = Arc::new(AppState {
        capture,
        next_order: AtomicU64::new(1),
    });
    let app = Router::new()
        .route("/orders", post(create_order))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4477")
        .await
        .expect("bind 127.0.0.1:4477");
    println!("axum-capture-fixture listening on http://127.0.0.1:4477");
    axum::serve(listener, app).await.expect("serve");
}
