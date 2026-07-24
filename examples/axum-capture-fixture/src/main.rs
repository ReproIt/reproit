//! Axum fixture for the reproit-backend production capture mode.
//!
//! One endpoint, one planted production bug: `POST /orders` assumes the
//! optional `discount` field is always a string, so a numeric discount
//! panics the order logic and returns HTTP 500. Every request is traced by
//! the first-class `reproit_backend::axum::ReproitLayer` middleware; the
//! capture sampler (not an `x-reproit-trace` header) decides what leaves
//! the process. Handlers record observed effects through the `Recorder`
//! in the request extensions.
//!
//! Environment: REPROIT_CAPTURE_ENDPOINT, REPROIT_CAPTURE_KEY,
//! REPROIT_CAPTURE_APP, optional REPROIT_CAPTURE_BUILD. Without them the
//! service still runs; capture is simply disabled.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::post;
use axum::{Extension, Router};
use reproit_backend::axum::{MiddlewareConfig, ReproitLayer};
use reproit_backend::{Capture, CaptureConfig, EffectKind, Recorder};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

struct AppState {
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
    recorder: Option<Extension<Recorder>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    // The middleware owns begin/finish; the handler only records effects.
    // Without capture (or a scan header) there is no recorder and nothing
    // leaves the process.
    let effect = |kind, resource: &str, key: &str| {
        if let Some(Extension(recorder)) = &recorder {
            let _ = recorder.effect(kind, Some(resource), Some(key), None, None, None);
        }
    };
    let item = body["item"].as_str().unwrap_or("unknown").to_string();
    effect(EffectKind::Read, "inventory", &item);
    let body_for_logic = body.clone();
    match std::panic::catch_unwind(move || order_summary(&body_for_logic)) {
        Ok((quantity, discount)) => {
            let id = state.next_order.fetch_add(1, Ordering::Relaxed);
            effect(EffectKind::Write, "orders", &format!("order-{id}"));
            let output = json!({ "id": id, "item": item, "qty": quantity, "discount": discount });
            (StatusCode::CREATED, Json(output))
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "internal server error" })),
        ),
    }
}

#[tokio::main]
async fn main() {
    let capture = capture_from_env();
    if capture.is_none() {
        eprintln!("capture disabled (set REPROIT_CAPTURE_ENDPOINT/KEY/APP)");
    }
    let layer = ReproitLayer::new(MiddlewareConfig {
        capture,
        operation: Some(Box::new(|_| "createOrder".to_string())),
        ..MiddlewareConfig::default()
    });
    let state = Arc::new(AppState {
        next_order: AtomicU64::new(1),
    });
    let app = Router::new()
        .route("/orders", post(create_order))
        .layer(layer)
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4477")
        .await
        .expect("bind 127.0.0.1:4477");
    println!("axum-capture-fixture listening on http://127.0.0.1:4477");
    axum::serve(listener, app).await.expect("serve");
}
