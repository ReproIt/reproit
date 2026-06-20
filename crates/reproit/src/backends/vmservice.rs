//! Dart VM service client (instrument v1a): live memory sampling over the
//! service WebSocket. The drive log prints the service URI
//! ("Connecting to Flutter application at http://..."), drive.rs captures
//! it, and the orchestrator samples every few seconds into
//! memory-<dev>.jsonl: the heap-trend probe soak mode's leak oracle reads.
//! Coverage collection (getSourceReport) is instrument v1b on the same
//! transport.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct MemSample {
    pub heap_used: u64,
    pub heap_capacity: u64,
    pub external: u64,
}

/// One-shot sample: connect, sum memory across isolates, close. Stateless
/// per sample so a mid-run app restart can't wedge the sampler.
pub async fn sample_memory(http_url: &str) -> Result<MemSample> {
    let ws_url = http_to_ws(http_url);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .with_context(|| format!("connecting {ws_url}"))?;
    let mut next_id = 1u64;
    let vm = rpc(&mut ws, &mut next_id, "getVM", json!({})).await?;
    let mut sample = MemSample {
        heap_used: 0,
        heap_capacity: 0,
        external: 0,
    };
    for iso in vm
        .get("isolates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let Some(id) = iso.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Ok(mem) = rpc(
            &mut ws,
            &mut next_id,
            "getMemoryUsage",
            json!({"isolateId": id}),
        )
        .await
        {
            sample.heap_used += mem.get("heapUsage").and_then(Value::as_u64).unwrap_or(0);
            sample.heap_capacity += mem.get("heapCapacity").and_then(Value::as_u64).unwrap_or(0);
            sample.external += mem
                .get("externalUsage")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }
    }
    let _ = ws.close(None).await;
    Ok(sample)
}

/// Coverage collection (instrument v1b): connect, ask each isolate for a
/// `getSourceReport(["Coverage"])`, and return the set of covered elements as
/// "<script-uri>#<tokenPos>" tokens. These feed spectrum-based fault
/// localization (fault.rs): the same identifiers across runs let Ochiai rank
/// which code is suspicious when some runs fail. Stateless per call, like the
/// memory sampler.
pub async fn collect_coverage(http_url: &str) -> Result<std::collections::BTreeSet<String>> {
    let ws_url = http_to_ws(http_url);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .with_context(|| format!("connecting {ws_url}"))?;
    let mut next_id = 1u64;
    let vm = rpc(&mut ws, &mut next_id, "getVM", json!({})).await?;
    let mut covered = std::collections::BTreeSet::new();
    for iso in vm
        .get("isolates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let Some(id) = iso.get("id").and_then(Value::as_str) else {
            continue;
        };
        let report = rpc(
            &mut ws,
            &mut next_id,
            "getSourceReport",
            json!({"isolateId": id, "reports": ["Coverage"], "forceCompile": false}),
        )
        .await;
        let Ok(report) = report else { continue };
        // scripts[scriptIndex].uri resolves each range's script; coverage.hits
        // are token positions executed at least once.
        let scripts = report
            .get("scripts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let uri_of = |i: usize| -> Option<&str> {
            scripts
                .get(i)
                .and_then(|s| s.get("uri"))
                .and_then(Value::as_str)
        };
        for range in report
            .get("ranges")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(uri) = range
                .get("scriptIndex")
                .and_then(Value::as_u64)
                .and_then(|i| uri_of(i as usize))
            else {
                continue;
            };
            for hit in range
                .get("coverage")
                .and_then(|c| c.get("hits"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
            {
                covered.insert(format!("{uri}#{hit}"));
            }
        }
    }
    let _ = ws.close(None).await;
    Ok(covered)
}

/// "http://127.0.0.1:PORT/TOKEN=/" -> "ws://127.0.0.1:PORT/TOKEN=/ws"
fn http_to_ws(http: &str) -> String {
    let base = http.trim().trim_end_matches('/');
    format!("{}/ws", base.replacen("http://", "ws://", 1))
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn rpc(ws: &mut Ws, next_id: &mut u64, method: &str, params: Value) -> Result<Value> {
    let id = next_id.to_string();
    *next_id += 1;
    ws.send(Message::Text(
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
    ))
    .await?;
    // Read until our reply id (the service may interleave other frames).
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .context("vm service rpc timeout")?
            .context("vm service stream closed")??;
        if let Message::Text(text) = msg {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if v.get("id").and_then(Value::as_str) == Some(id.as_str()) {
                    if let Some(err) = v.get("error") {
                        anyhow::bail!("vm service error: {err}");
                    }
                    return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                }
            }
        }
    }
}
