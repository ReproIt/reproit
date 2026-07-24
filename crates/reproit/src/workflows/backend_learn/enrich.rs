//! Optional live enrichment for `reproit init --learn`: one bounded GET per
//! parameterless derived GET route against the resolved target. Non-GET
//! methods are never sent. Every probe is fail-soft; the whole pass has a
//! hard route cap and time budget.

use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub(super) const MAX_PROBED_ROUTES: usize = 32;
pub(super) const TOTAL_BUDGET: Duration = Duration::from_secs(10);
const PER_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_TRAIL_HEADER_BYTES: usize = 64 * 1024;
const MAX_EFFECTS_NOTED: usize = 8;

/// What one live probe observed about a route.
pub(super) struct Observation {
    pub(super) status: u16,
    /// The JSON response body sample; None for non-JSON or empty bodies.
    pub(super) body: Option<Value>,
    /// Adapter effect kinds, as `kind(resource)` strings, when the target
    /// answered with an `x-reproit-events` trail.
    pub(super) effects: Vec<String>,
}

pub(super) struct ProbeOutcome {
    pub(super) observations: BTreeMap<String, Observation>,
    pub(super) attempted: usize,
    /// True when any probed route returned an adapter effect trail.
    pub(super) adapter: bool,
}

/// Probe up to [`MAX_PROBED_ROUTES`] parameterless GET paths within the total
/// budget. Errors and timeouts skip the route, never the pass.
pub(super) async fn probe(base: &str, paths: &[String]) -> ProbeOutcome {
    let mut outcome = ProbeOutcome {
        observations: BTreeMap::new(),
        attempted: 0,
        adapter: false,
    };
    let Ok(client) = reqwest::Client::builder()
        .timeout(PER_REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    else {
        return outcome;
    };
    let base = base.trim_end_matches('/');
    let started = Instant::now();
    for (index, path) in paths.iter().take(MAX_PROBED_ROUTES).enumerate() {
        if started.elapsed() >= TOTAL_BUDGET {
            break;
        }
        outcome.attempted += 1;
        let url = format!("{base}{path}");
        let request = client
            .get(&url)
            .header("x-reproit-trace", format!("learn{index:08}"))
            .header("x-reproit-action", "1");
        let Ok(response) = request.send().await else {
            continue;
        };
        let status = response.status().as_u16();
        // Adapter presence is the trail header itself; a read-only probe may
        // legitimately observe zero effects.
        let trail = response
            .headers()
            .get("x-reproit-events")
            .and_then(|header| header.to_str().ok())
            .map(str::to_string);
        if trail.is_some() {
            outcome.adapter = true;
        }
        let effects = trail.as_deref().map(decode_effects).unwrap_or_default();
        let body = read_json_body(response).await;
        outcome.observations.insert(
            path.clone(),
            Observation {
                status,
                body,
                effects,
            },
        );
    }
    outcome
}

/// Read the response body up to the byte cap and parse it as JSON; anything
/// else (oversized, non-UTF8, non-JSON) records no body shape.
async fn read_json_body(mut response: reqwest::Response) -> Option<Value> {
    let mut bytes = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).ok()
}

/// Decode the adapter's base64url(JSON events) trail into deduplicated
/// `kind(resource)` notes. A malformed trail notes nothing; the draft never
/// carries claims it cannot read.
pub(super) fn decode_effects(header: &str) -> Vec<String> {
    use base64::Engine as _;
    if header.len() > MAX_TRAIL_HEADER_BYTES {
        return Vec::new();
    }
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(header) else {
        return Vec::new();
    };
    let Ok(events) = serde_json::from_slice::<Vec<Value>>(&bytes) else {
        return Vec::new();
    };
    let mut notes = Vec::new();
    for event in events {
        let Some(kind) = event.get("effect").and_then(Value::as_str) else {
            continue;
        };
        let note = match event.get("resource").and_then(Value::as_str) {
            Some(resource) => format!("{kind}({resource})"),
            None => kind.to_string(),
        };
        if !notes.contains(&note) {
            notes.push(note);
            if notes.len() >= MAX_EFFECTS_NOTED {
                break;
            }
        }
    }
    notes
}
