//! Real Axum socket middleware cost plus per-dependency capture cost.
use axum::{routing::get, Json, Router};
use reproit_backend::axum::{MiddlewareConfig, ReproitLayer};
use reproit_backend::{BackendTrace, EffectKind, TraceContext};
use serde_json::{json, Value};
use std::time::Instant;

const DEPENDENCIES: usize = 64;

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

async fn measure_http(mounted: bool, traced: bool, runs: usize) -> f64 {
    let app = Router::new().route(
        "/account",
        get(|| async { Json(json!({"account":{"id":42,"ok":true}})) }),
    );
    let app = if mounted {
        app.layer(ReproitLayer::new(MiddlewareConfig::default()))
    } else {
        app
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    for _ in 0..runs.min(500) / 4 {
        let mut request = client.get(format!("http://{address}/account?id=42"));
        if traced {
            request = request.header("x-reproit-trace", "bench-trace");
        }
        request.send().await.unwrap().bytes().await.unwrap();
    }
    let started = Instant::now();
    for _ in 0..runs {
        let mut request = client.get(format!("http://{address}/account?id=42"));
        if traced {
            request = request.header("x-reproit-trace", "bench-trace");
        }
        request.send().await.unwrap().bytes().await.unwrap();
    }
    let micros = started.elapsed().as_secs_f64() * 1_000_000.0 / runs as f64;
    server.abort();
    micros
}

fn context() -> TraceContext {
    TraceContext::from_header_fn(|name| match name {
        "x-reproit-trace" => Some("dependency-benchmark".to_string()),
        "x-reproit-action" => Some("1".to_string()),
        _ => None,
    })
    .unwrap()
}

fn measure_dependencies(captured: bool, runs: usize) -> f64 {
    let exchange = json!({
        "request":{"method":"GET","url":"http://pricing.test/quote?tier=gold"},
        "response":{"status":200,"body":{"price":42}}
    });
    let started = Instant::now();
    for _ in 0..runs {
        let mut trace = BackendTrace::begin(
            context(),
            "dependencyBenchmark",
            None,
            None,
            None,
            Value::Null,
            Vec::new(),
        )
        .unwrap();
        if captured {
            for index in 0..DEPENDENCIES {
                trace
                    .exchange(
                        EffectKind::Call,
                        Some("pricing"),
                        Some(&index.to_string()),
                        exchange.clone(),
                    )
                    .unwrap();
            }
        }
    }
    started.elapsed().as_secs_f64() * 1_000_000.0 / (runs * DEPENDENCIES) as f64
}

#[tokio::main]
async fn main() {
    let runs = std::env::var("REPROIT_ADAPTER_BENCH_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000);
    let rounds = std::env::var("REPROIT_ADAPTER_BENCH_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    let mut baseline = Vec::new();
    let mut inactive = Vec::new();
    let mut active = Vec::new();
    let mut control = Vec::new();
    for _ in 0..rounds {
        baseline.push(measure_http(false, false, runs).await);
        inactive.push(measure_http(true, false, runs).await);
        active.push(measure_http(true, true, runs).await);
        control.push(measure_http(false, false, runs).await);
    }
    let base = median(baseline);
    let inactive_cost = median(inactive) - base;
    let active_cost = median(active) - base;
    let noise = (median(control) - base).abs();
    assert!(noise < 250.0, "Rust HTTP benchmark noise is {noise:.2}us");
    assert!(
        inactive_cost < 250.0,
        "Rust inactive cost is {inactive_cost:.2}us"
    );
    assert!(
        active_cost < 800.0,
        "Rust active cost is {active_cost:.2}us"
    );

    let dependency_runs = runs.min(1_000);
    let mut dep_baseline = Vec::new();
    let mut dep_capture = Vec::new();
    let mut dep_control = Vec::new();
    for _ in 0..rounds {
        dep_baseline.push(measure_dependencies(false, dependency_runs));
        dep_capture.push(measure_dependencies(true, dependency_runs));
        dep_control.push(measure_dependencies(false, dependency_runs));
    }
    let dep_base = median(dep_baseline);
    let dep_cost = median(dep_capture) - dep_base;
    let dep_noise = (median(dep_control) - dep_base).abs();
    assert!(
        dep_noise < 10.0,
        "Rust dependency noise is {dep_noise:.2}us"
    );
    assert!(dep_cost < 50.0, "Rust dependency cost is {dep_cost:.2}us");
    println!(
        "{}",
        json!({
            "language":"rust","runs":runs,"rounds":rounds,
            "noiseFloorMicros":noise,"baselineMicros":base,
            "inactiveCostMicros":inactive_cost,"activeCostMicros":active_cost,
            "dependencyNoiseFloorMicros":dep_noise,
            "dependencyCaptureCostMicros":dep_cost,
            "dependencyCeilingMicros":50
        })
    );
}
