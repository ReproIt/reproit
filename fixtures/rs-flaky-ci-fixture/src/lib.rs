//! Module under test for the rs flaky-CI fixture: computes an order total
//! from the tax rate the config service answers, plus the shared, stateful
//! config service both tests talk to.
//!
//! The planted bug: the config service's LEGACY format returns the rate as a
//! STRING ("0.25"). The unfixed code reads it with `as_f64()`, whose lenient
//! `unwrap_or(0.0)` fallback silently applies a zero rate, so a 100 subtotal
//! totals 100 instead of 125. FIXED=1 applies the fix: parse a string rate
//! before falling back.

use reproit_backend::instrument;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static LEGACY: AtomicBool = AtomicBool::new(false);
static URL: OnceLock<String> = OnceLock::new();

pub async fn order_total(subtotal: f64, config_url: &str) -> f64 {
    let client = reqwest::Client::new();
    let request = client
        .get(format!("{config_url}/tax-rate"))
        .build()
        .expect("request");
    let response = instrument::http::send(&client, request)
        .await
        .expect("config service");
    let body: serde_json::Value = response.json().expect("config json");
    let rate = &body["rate"];
    let applied = if std::env::var("FIXED").ok().as_deref() == Some("1") {
        rate.as_f64()
            .or_else(|| rate.as_str().and_then(|text| text.parse().ok()))
            .unwrap_or(0.0)
    } else {
        rate.as_f64().unwrap_or(0.0)
    };
    subtotal * (1.0 + applied)
}

/// The config service URL the tests dial. Under replay the SDK serves the
/// recorded exchanges in process and matching is on the path alone, so the
/// placeholder origin is never dialed and no server starts; any real socket
/// attempt would surface as a divergence, not a connection.
pub fn config_url() -> String {
    if std::env::var("REPROIT_REPLAY").is_ok() {
        return "http://127.0.0.1:9".to_string();
    }
    URL.get_or_init(start_config_service).clone()
}

/// Stateful on purpose: the legacy-format test leaks its toggle into it,
/// which is exactly the order dependence the fixture plants.
fn start_config_service() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind config service");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            loop {
                let mut header = String::new();
                match reader.read_line(&mut header) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if header == "\r\n" || header == "\n" => break,
                    Ok(_) => {}
                }
            }
            let response = if request_line.starts_with("POST /format/legacy") {
                LEGACY.store(true, Ordering::SeqCst);
                "HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n".to_string()
            } else {
                let body = if LEGACY.load(Ordering::SeqCst) {
                    r#"{"rate":"0.25"}"#
                } else {
                    r#"{"rate":0.25}"#
                };
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}
