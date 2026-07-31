//! Re-recording a guard's dependency exchanges against the current code.
//!
//! This is the RECORD half of `keep --refresh`. It boots the guard's stored
//! recipe with capture enabled and replay OFF, so the app talks to its real
//! local dependencies, fires the guard's recorded inbound trigger, and reads
//! back the capture the SDK writes. Nothing here decides whether the result
//! should be adopted; that is the diff-and-confirm step in `refresh`.

use super::capture_replay::CaptureArtifact;
use crate::domain::backend::BackendEvent;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;

/// Boot budget for the re-recording run, matching the hermetic path's.
const BOOT_TIMEOUT: Duration = Duration::from_secs(20);
/// Grace for the SDK's capture sink to flush after the response arrives.
const FLUSH_GRACE: Duration = Duration::from_millis(500);

/// What the current code did when the guard's trigger was fired at it.
pub(super) struct Recorded {
    pub(super) events: Vec<BackendEvent>,
    pub(super) payload: Value,
}

/// Boot `exec` in RECORD mode against a disposable local environment, fire the
/// guard's recorded inbound request, and return the capture the SDK wrote.
///
/// `REPROIT_CAPTURE_OUT` names the file the SDK writes its capture to, and
/// `REPROIT_REPLAY` is explicitly unset so the app reaches its real local
/// dependencies. An app that does not honour the capture-out contract yields
/// no file, which is an error naming the exact next input rather than a
/// silently empty refresh.
pub(super) async fn record_current(old: &CaptureArtifact, exec: &str) -> Result<Recorded> {
    let workspace = tempdir()?;
    let out = workspace.join("refreshed-capture.json");
    let port = free_port()?;
    let base = format!("http://127.0.0.1:{port}");

    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(exec)
        .env_remove("REPROIT_REPLAY")
        .env("REPROIT_CAPTURE_OUT", &out)
        .env("REPROIT_CAPTURE", "1")
        .env("PORT", port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn the guard's recipe {exec:?} for re-recording"))?;
    let guard = KillOnDrop(child);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    if !wait_for_boot(&client, &base).await {
        bail!(
            "the guard's recipe did not serve {base} within {}s, so there is nothing to \
             re-record against; check that `{exec}` boots a local instance",
            BOOT_TIMEOUT.as_secs()
        );
    }
    // The SAME inbound trigger the guard recorded. Re-recording changes how
    // the operation reaches its dependencies, never what was asked of it.
    let _ = super::hermetic::fire_recorded_trigger(&client, &base, old).await;
    tokio::time::sleep(FLUSH_GRACE).await;
    drop(guard);

    let bytes = std::fs::read(&out).with_context(|| {
        format!(
            "the re-recording run wrote no capture to {}; the app must honour \
             REPROIT_CAPTURE_OUT under the reproit SDK for --refresh to read what it did",
            out.display()
        )
    })?;
    let payload: Value =
        serde_json::from_slice(&bytes).context("the re-recorded capture is not valid JSON")?;
    let events: Vec<BackendEvent> = payload
        .get("events")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("the re-recorded capture's events do not parse")?
        .unwrap_or_default();
    if events.is_empty() {
        bail!("the re-recording run captured no events, so there is nothing to compare");
    }
    let _ = std::fs::remove_dir_all(&workspace);
    Ok(Recorded { events, payload })
}

/// Build the refreshed capture: the ORIGINAL payload with only its dependency
/// exchanges replaced by the freshly recorded ones. The inbound trigger (the
/// `start` event) and the oracle stay exactly as captured in production, which
/// is what keeps a refreshed guard the same guard.
pub(super) fn merge_preserving_trigger(old_bytes: &[u8], recorded: &Recorded) -> Result<Value> {
    let mut payload: Value =
        serde_json::from_slice(old_bytes).context("the stored capture is not valid JSON")?;
    let fresh_effects: Vec<Value> = recorded
        .payload
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter(|event| event.get("exchange").is_some_and(|value| !value.is_null()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    let events = payload
        .get_mut("events")
        .and_then(Value::as_array_mut)
        .context("the stored capture has no events array")?;
    // Keep everything that is not an exchange-bearing effect (the start
    // trigger, the return, plain effects), then splice the fresh exchanges in
    // where the old ones were: immediately after the start event.
    let preserved: Vec<Value> = events
        .iter()
        .filter(|event| event.get("exchange").is_none_or(Value::is_null))
        .cloned()
        .collect();
    let split = preserved
        .iter()
        .position(|event| event.get("kind").and_then(Value::as_str) != Some("start"))
        .unwrap_or(preserved.len());
    let mut rebuilt = Vec::with_capacity(preserved.len() + fresh_effects.len());
    rebuilt.extend_from_slice(&preserved[..split]);
    rebuilt.extend(fresh_effects);
    rebuilt.extend_from_slice(&preserved[split..]);
    *events = rebuilt;
    Ok(payload)
}

async fn wait_for_boot(client: &reqwest::Client, base: &str) -> bool {
    let deadline = std::time::Instant::now() + BOOT_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if client.get(base).send().await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn tempdir() -> Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "reproit-refresh-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The re-recording child must never outlive the refresh, on any exit path.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn capture_with(exchanges: Vec<Value>) -> Value {
        let mut events = vec![json!({
            "traceId": "t", "spanId": "s", "actionIndex": 0, "operation": "GET /quote",
            "sequence": 1, "kind": "start", "input": {"query": {"symbol": "ACME"}}
        })];
        events.extend(exchanges);
        events.push(json!({
            "traceId": "t", "spanId": "s", "actionIndex": 0, "operation": "GET /quote",
            "sequence": 9, "kind": "return", "output": {"error": "internal"},
            "status": 500, "success": false, "effectsComplete": true
        }));
        json!({
            "format": "reproit-backend-capture", "version": 2,
            "operation": "GET /quote", "oracle": "backend-server-error",
            "events": events
        })
    }

    fn exchange(sequence: u64, url: &str) -> Value {
        json!({
            "traceId": "t", "spanId": "s", "actionIndex": 0, "operation": "GET /quote",
            "sequence": sequence, "kind": "effect", "effect": "call",
            "resource": "dep", "key": url,
            "exchange": {
                "protocol": "http",
                "request": {"method": "GET", "url": url},
                "response": {"status": 200, "body": {"ok": true}}
            }
        })
    }

    /// The whole safety property of a refresh: the trigger the production
    /// failure arrived with, and the oracle that judges it, survive intact.
    #[test]
    fn a_refresh_replaces_exchanges_and_preserves_trigger_and_oracle() {
        let old = capture_with(vec![exchange(2, "http://old/prices")]);
        let old_bytes = serde_json::to_vec(&old).unwrap();
        let recorded = Recorded {
            events: Vec::new(),
            payload: capture_with(vec![
                exchange(2, "http://new/prices"),
                exchange(3, "http://new/inventory"),
            ]),
        };
        let merged = merge_preserving_trigger(&old_bytes, &recorded).unwrap();

        assert_eq!(merged["oracle"], "backend-server-error");
        assert_eq!(merged["operation"], "GET /quote");
        let events = merged["events"].as_array().unwrap();
        // The original trigger, byte for byte.
        assert_eq!(events[0]["kind"], "start");
        assert_eq!(events[0]["input"]["query"]["symbol"], "ACME");
        // The fresh exchanges, in order, between trigger and return.
        let urls: Vec<&str> = events
            .iter()
            .filter_map(|event| {
                event
                    .pointer("/exchange/request/url")
                    .and_then(Value::as_str)
            })
            .collect();
        assert_eq!(urls, vec!["http://new/prices", "http://new/inventory"]);
        // The recorded outcome the oracle reads is still the production one.
        let last = events.last().unwrap();
        assert_eq!(last["kind"], "return");
        assert_eq!(last["status"], 500);
    }

    /// A refresh that records nothing must not quietly empty the guard.
    #[test]
    fn a_refresh_with_no_new_exchanges_keeps_the_trigger_and_return() {
        let old = capture_with(vec![exchange(2, "http://old/prices")]);
        let old_bytes = serde_json::to_vec(&old).unwrap();
        let recorded = Recorded {
            events: Vec::new(),
            payload: capture_with(Vec::new()),
        };
        let merged = merge_preserving_trigger(&old_bytes, &recorded).unwrap();
        let events = merged["events"].as_array().unwrap();
        assert_eq!(events.len(), 2, "trigger and return survive: {events:?}");
        assert_eq!(events[0]["kind"], "start");
        assert_eq!(events[1]["kind"], "return");
    }
}
