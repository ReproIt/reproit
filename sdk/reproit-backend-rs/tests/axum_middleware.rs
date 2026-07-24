//! Functional test of the axum middleware: a Router with a planted 500, a
//! local stub ingest for the capture batch, and a scan-time header
//! round-trip. Mirrors the Node/Python/Go SDK e2e tests.
#![cfg(feature = "axum")]

mod support;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reproit_backend::axum::{MiddlewareConfig, ReproitLayer};
use reproit_backend::{Capture, CaptureConfig, EffectKind, Recorder, SERVER_ERROR_ORACLE};
use serde_json::{json, Value};
use std::time::Duration;
use tower::ServiceExt;

fn test_app(capture: Option<Capture>) -> Router {
    let layer = ReproitLayer::new(MiddlewareConfig {
        capture,
        ..MiddlewareConfig::default()
    });
    Router::new()
        .route("/ok", get(|| async { Json(json!({"ok": true})) }))
        .route(
            "/boom",
            post(|request: Request<Body>| async move {
                if let Some(recorder) = request.extensions().get::<Recorder>() {
                    let _ = recorder.effect(
                        EffectKind::Write,
                        Some("orders"),
                        Some("1"),
                        None,
                        None,
                        None,
                    );
                }
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "boom"})),
                )
            }),
        )
        .layer(layer)
}

#[tokio::test]
async fn planted_500_ships_a_tagged_finding_batch() {
    let ingest = support::start_stub_ingest();
    let mut config = CaptureConfig::new(&ingest.url, "sk_live_test", "app-e2e");
    config.build = Some("9.9.9".into());
    config.flush_interval = Duration::from_millis(100);
    let capture = Capture::new(config).expect("capture must start");

    let app = test_app(Some(capture.clone()));
    let request = Request::post("/boom")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"item":"widget","apiKey":"sk_live_leak"}"#))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(capture.flush(Duration::from_secs(5)));

    let (authorization, batch) = ingest
        .received
        .recv_timeout(Duration::from_secs(5))
        .expect("stub ingest received a batch");
    assert_eq!(authorization, "bearer sk_live_test");
    assert_eq!(batch["appId"], "app-e2e");
    assert_eq!(batch["deployment"]["version"], "9.9.9");
    let parsed: reproit_protocol::EventBatch =
        serde_json::from_value(batch.clone()).expect("batch matches event-batch-v1");
    parsed.validate().expect("batch passes protocol validation");
    let frames = batch["frames"].as_array().unwrap();
    let findings: Vec<&Value> = frames
        .iter()
        .map(|frame| &frame["event"])
        .filter(|event| event["kind"] == "finding")
        .collect();
    assert_eq!(findings.len(), 1);
    let finding = findings[0];
    assert_eq!(finding["identity"]["oracle"], SERVER_ERROR_ORACLE);
    assert_eq!(finding["context"]["capture"], "reproit-backend-rs");
    let payload = &finding["context"]["reproitCapture"];
    let kinds: Vec<&str> = payload["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["start", "effect", "return"]);
    assert_eq!(payload["events"][1]["resource"], "orders");
    assert_eq!(payload["events"][2]["status"], 500);
    assert_eq!(payload["events"][2]["success"], false);
    // The secret-shaped input field was structurally redacted before upload.
    let body = &payload["events"][0]["input"]["body"];
    assert_eq!(body["apiKey"]["$reproit"]["redacted"], true);
    assert_eq!(body["item"], "widget");

    // The healthy scan-time request must not have been captured.
    scan_header_round_trips(test_app(Some(capture.clone()))).await;
    assert_eq!(capture.stats().captured_operations, 1);
}

#[tokio::test]
async fn scan_time_works_without_capture() {
    scan_header_round_trips(test_app(None)).await;
}

async fn scan_header_round_trips(app: Router) {
    let request = Request::get("/ok")
        .header("x-reproit-trace", "trace-e2e")
        .header("x-reproit-actor", "alice")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let header = response
        .headers()
        .get("x-reproit-events")
        .expect("expected an x-reproit-events response header")
        .to_str()
        .unwrap()
        .to_string();
    let decoded = URL_SAFE_NO_PAD.decode(header).unwrap();
    let events: Vec<Value> = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(events[0]["traceId"], "trace-e2e");
    assert_eq!(events[0]["actor"], "alice");
    let last = events.last().unwrap();
    assert_eq!(last["kind"], "return");
    assert_eq!(last["status"], 200);
    assert_eq!(last["output"]["ok"], true);
    // The response body must survive the buffering untouched.
    let body = to_bytes(response.into_body(), 1 << 16).await.unwrap();
    assert_eq!(&body[..], br#"{"ok":true}"#);
}

#[tokio::test]
async fn stays_inert_without_header_or_capture() {
    let app = test_app(None);
    let request = Request::get("/ok").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert!(response.headers().get("x-reproit-events").is_none());
}
