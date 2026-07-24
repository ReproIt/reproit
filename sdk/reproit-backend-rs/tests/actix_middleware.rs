//! Functional test of the actix-web middleware: an App with a planted 500,
//! a local stub ingest for the capture batch, and a scan-time header
//! round-trip. Mirrors the axum middleware test.
#![cfg(feature = "actix")]

mod support;

use actix_web::http::StatusCode;
use actix_web::{test, web, App, HttpMessage, HttpRequest, HttpResponse};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reproit_backend::actix::{MiddlewareConfig, Reproit};
use reproit_backend::{Capture, CaptureConfig, EffectKind, Recorder, SERVER_ERROR_ORACLE};
use serde_json::{json, Value};
use std::time::Duration;

async fn ok_handler() -> HttpResponse {
    HttpResponse::Ok().json(json!({"ok": true}))
}

async fn boom_handler(request: HttpRequest, _body: web::Json<Value>) -> HttpResponse {
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
    HttpResponse::InternalServerError().json(json!({"error": "boom"}))
}

fn middleware(capture: Option<Capture>) -> Reproit {
    Reproit::new(MiddlewareConfig {
        capture,
        ..MiddlewareConfig::default()
    })
}

#[actix_web::test]
async fn planted_500_ships_a_tagged_finding_batch() {
    let ingest = support::start_stub_ingest();
    let mut config = CaptureConfig::new(&ingest.url, "sk_live_test", "app-e2e");
    config.build = Some("9.9.9".into());
    config.flush_interval = Duration::from_millis(100);
    let capture = Capture::new(config).expect("capture must start");

    let app = test::init_service(
        App::new()
            .wrap(middleware(Some(capture.clone())))
            .route("/ok", web::get().to(ok_handler))
            .route("/boom", web::post().to(boom_handler)),
    )
    .await;
    let request = test::TestRequest::post()
        .uri("/boom")
        .insert_header(("content-type", "application/json"))
        .set_payload(r#"{"item":"widget","apiKey":"sk_live_leak"}"#)
        .to_request();
    let response = test::call_service(&app, request).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(capture.flush(Duration::from_secs(5)));

    let (authorization, batch) = ingest
        .received
        .recv_timeout(Duration::from_secs(5))
        .expect("stub ingest received a batch");
    assert_eq!(authorization, "bearer sk_live_test");
    assert_eq!(batch["appId"], "app-e2e");
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
    // The secret-shaped input field was structurally redacted before upload,
    // and the JSON extractor still saw the re-buffered payload.
    let body = &payload["events"][0]["input"]["body"];
    assert_eq!(body["apiKey"]["$reproit"]["redacted"], true);
    assert_eq!(body["item"], "widget");

    // Scan-time round-trip on the same app; it must not be captured.
    let request = test::TestRequest::get()
        .uri("/ok")
        .insert_header(("x-reproit-trace", "trace-e2e"))
        .insert_header(("x-reproit-actor", "alice"))
        .to_request();
    let response = test::call_service(&app, request).await;
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
    let body = test::read_body(response).await;
    assert_eq!(&body[..], br#"{"ok":true}"#);
    assert_eq!(capture.stats().captured_operations, 1);
}

#[actix_web::test]
async fn stays_inert_without_header_or_capture() {
    let app = test::init_service(
        App::new()
            .wrap(middleware(None))
            .route("/ok", web::get().to(ok_handler)),
    )
    .await;
    let response = test::call_service(&app, test::TestRequest::get().uri("/ok").to_request()).await;
    assert!(response.headers().get("x-reproit-events").is_none());
}
