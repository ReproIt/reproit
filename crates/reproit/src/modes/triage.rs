//! Production-bug triage: the "here's my issue, look at it and reproduce it"
//! flow, over the cloud's telemetry. Pairs with `reproit mcp` so a coding agent
//! (or a person) can ask, in plain words, what a bug might be and get a
//! deterministic reproduction.
//!
//! - `find`: list production error clusters + their context discriminator.
//! - `explain`: one cluster in full (path, "which users" discriminator,
//!   suspected source from the stack, and the replay).
//! - `reproduce`: materialize the deterministic replay and run it.
//! - `diagnose`: match a free-text report to a cluster, then explain (+repro).
//!
//! The cloud base URL/key come from --cloud/--key, then REPROIT_CLOUD_URL /
//! REPROIT_API_KEY, then localhost. Output is plain text so MCP can relay it.

use anyhow::{Context, Result};
use serde_json::Value;

struct Cloud {
    base: String,
    key: Option<String>,
}

impl Cloud {
    fn new(cloud: Option<String>, key: Option<String>) -> Self {
        // Defaults to the hosted cloud; set REPROIT_CLOUD_URL to point elsewhere
        // (e.g. http://cloud.reproit.localhost for the local Traefik edge proxy).
        let base = cloud
            .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
            .unwrap_or_else(|| "https://cloud.reproit.com".to_string());
        let key = key.or_else(|| std::env::var("REPROIT_API_KEY").ok());
        Cloud {
            base: base.trim_end_matches('/').to_string(),
            key,
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let client = reqwest::Client::new();
        let mut req = client.get(format!("{}{}", self.base, path));
        if let Some(k) = &self.key {
            req = req.bearer_auth(k);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("GET {}{}", self.base, path))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("cloud {} -> {}: {}", path, status, body.trim());
        }
        serde_json::from_str(&body).with_context(|| format!("parsing {path}"))
    }
}

/// Raw GET against the cloud errors namespace: `/v1/errors/:app{suffix}`. Used
/// by the `--export` paths (`cloud query`, `cloud findings --export`, etc.) to
/// surface the unrendered JSON the dashboard views are built from. Fails
/// gracefully: a connection error or non-2xx surfaces as an anyhow error with a
/// clear message (Cloud::get already bails), never a panic.
pub async fn raw(
    app: &str,
    suffix: &str,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<Value> {
    let c = Cloud::new(cloud, key);
    c.get(&format!("/v1/errors/{app}{suffix}")).await
}

/// Lightweight reachability + auth probe used by `cloud login` to validate a
/// freshly stored token. Hits the root; any successful response (even an
/// unrelated body) means the cloud is up and the bearer was accepted.
pub async fn ping(base: &str, key: Option<&str>) -> Result<()> {
    let client = reqwest::Client::new();
    let mut req = client.get(base.trim_end_matches('/'));
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("connecting to {base}"))?;
    let status = resp.status();
    if status.is_success() || status.as_u16() == 404 {
        // 404 at the root still proves the host + auth layer are reachable.
        Ok(())
    } else {
        anyhow::bail!("{base} -> {status}")
    }
}

/// Filter a raw errors response by a free-text query against each error's
/// message (case-insensitive substring). Returns the value unchanged when the
/// query is None or the shape is unexpected. Pure, so it is unit-tested.
pub fn filter_errors(mut v: Value, query: Option<&str>) -> Value {
    let Some(q) = query.map(|s| s.to_lowercase()) else {
        return v;
    };
    if let Some(arr) = v.get_mut("errors").and_then(Value::as_array_mut) {
        arr.retain(|e| {
            e.get("message")
                .and_then(Value::as_str)
                .map(|m| m.to_lowercase().contains(&q))
                .unwrap_or(false)
        });
    }
    v
}

/// A one-line discriminator summary like `locale=tr (100% of cohort, 8.3x baseline)`.
fn fmt_discriminators(ds: &[Value]) -> String {
    if ds.is_empty() {
        return "none (not data-specific, or no context captured yet)".to_string();
    }
    ds.iter()
        .take(3)
        .map(|d| {
            let key = d["key"].as_str().unwrap_or("?");
            let val = d["value"].as_str().unwrap_or("?");
            let share = d["cohortShare"].as_f64().unwrap_or(0.0) * 100.0;
            let lift = d["lift"].to_string().replace('"', "");
            format!("{key}={val} ({share:.0}% of cohort, {lift}x baseline)")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Pull a `file.ext:line` hint out of a stack/message if present.
fn suspected_source(message: &str) -> Option<String> {
    let re = regex::Regex::new(r"([\w./-]+\.(?:dart|kt|swift|ts|tsx|js|rs|py)):(\d+)").ok()?;
    re.captures(message).map(|c| format!("{}:{}", &c[1], &c[2]))
}

pub async fn find(
    app: &str,
    query: Option<&str>,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud, key);
    let cohorts = c.get(&format!("/v1/errors/{app}/cohorts")).await?;
    let empty = vec![];
    let clusters = cohorts["errors"].as_array().unwrap_or(&empty);
    let q = query.map(|s| s.to_lowercase());
    let mut shown = 0;
    println!("Production error clusters for '{app}':");
    for cl in clusters {
        let msg = cl["message"].as_str().unwrap_or("");
        if let Some(q) = &q {
            if !msg.to_lowercase().contains(q.as_str()) {
                continue;
            }
        }
        let sig = cl["sig"].as_str().unwrap_or("?");
        let count = cl["count"].as_u64().unwrap_or(0);
        let ds = cl["discriminators"].as_array().cloned().unwrap_or_default();
        println!("\n  [{sig}] x{count}  {}", first_line(msg));
        println!("    who: {}", fmt_discriminators(&ds));
        shown += 1;
    }
    if shown == 0 {
        println!("  (no matching clusters)");
    }
    Ok(())
}

pub async fn explain(
    app: &str,
    sig: Option<&str>,
    idx: Option<usize>,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud, key);
    // resolve to an index into the error list (repro is by index)
    let errors = c.get(&format!("/v1/errors/{app}")).await?;
    let empty = vec![];
    let list = errors["errors"].as_array().unwrap_or(&empty);
    let target_idx = match (sig, idx) {
        (_, Some(i)) => i,
        (Some(sig), None) => list
            .iter()
            .position(|e| e["sig"].as_str() == Some(sig))
            .context("no error with that signature")?,
        (None, None) => 0,
    };
    let err = list.get(target_idx).context("no such error")?;
    let sig = err["sig"].as_str().unwrap_or("?");
    let msg = err["message"].as_str().unwrap_or("");

    // cohort discriminator for this signature
    let cohorts = c.get(&format!("/v1/errors/{app}/cohorts")).await?;
    let ds = cohorts["errors"]
        .as_array()
        .and_then(|cs| cs.iter().find(|cl| cl["sig"].as_str() == Some(sig)))
        .and_then(|cl| cl["discriminators"].as_array().cloned())
        .unwrap_or_default();

    let repro = c
        .get(&format!("/v1/errors/{app}/{target_idx}/repro"))
        .await?;
    let replay = repro["replay"].as_array().cloned().unwrap_or_default();

    println!("Error [{sig}] (#{target_idx}) in '{app}'");
    println!("  message:   {}", first_line(msg));
    if let Some(src) = suspected_source(msg) {
        println!("  suspected: {src}");
    }
    println!("  who:       {}", fmt_discriminators(&ds));
    println!(
        "  ended at:  {}",
        repro["endedAtState"].as_str().unwrap_or("?")
    );
    let actions: Vec<String> = replay
        .iter()
        .filter_map(|a| a.as_str().map(String::from))
        .collect();
    println!(
        "  replay:    {}",
        if actions.is_empty() {
            "(no executable actions)".into()
        } else {
            actions.join(" -> ")
        }
    );
    println!("\nReproduce: reproit cloud reproduce --app {app} --idx {target_idx}");
    Ok(())
}

/// How a cloud-pulled session replayed. The key distinction `reproduce` must
/// make: "replayed clean" (the bug did NOT fire, so it is likely data-dependent)
/// is NOT the same as "could not replay" (the app drifted since the session, so
/// this run is no verdict on the bug at all). The old code collapsed both into
/// "clean" and also counted any process failure as reproduced.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReproVerdict {
    Reproduced,
    Clean,
    Stale,
    Flaky,
    Unknown,
}

/// Classify a reproduce run from `reproit check`'s deterministic verdict (its
/// `--json` `outcome`), falling back to its exit code (1 fail / 2 flaky / 3
/// stale / 0 pass) if the JSON is unreadable.
pub(crate) fn classify_repro(outcome: Option<&str>, exit_code: Option<i32>) -> ReproVerdict {
    match outcome {
        Some("fail") => ReproVerdict::Reproduced,
        Some("pass") => ReproVerdict::Clean,
        Some("stale") => ReproVerdict::Stale,
        Some("flaky") => ReproVerdict::Flaky,
        _ => match exit_code {
            Some(1) => ReproVerdict::Reproduced,
            Some(2) => ReproVerdict::Flaky,
            Some(3) => ReproVerdict::Stale,
            Some(0) => ReproVerdict::Clean,
            _ => ReproVerdict::Unknown,
        },
    }
}

pub async fn reproduce(
    app: &str,
    idx: usize,
    journey: &str,
    run: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud, key);
    let repro = c.get(&format!("/v1/errors/{app}/{idx}/repro")).await?;
    let replay: Vec<String> = repro["replay"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Property-matched replay (tier 3): synthesize concrete, deterministic input
    // data from the cloud's fixtureSpec, so a bug that only hits SOME users (a
    // 312-char unicode name, an emoji, a Turkish dotless "i", an empty/RTL field,
    // a specific locale) reproduces. Features-matching, never the real PII.
    let fixture = crate::fixture::synthesize(&repro["fixtureSpec"]);

    if replay.is_empty() && fixture.is_empty() {
        println!("This error has no executable replay actions and no data signal.");
        println!(
            "context (the distinguishing dimension to synthesize): {}",
            repro["context"]
        );
        return Ok(());
    }

    // Materialize the deterministic config the runner reads: the action replay
    // plus, when the bug is data-specific, the synthesized fixture (inputs +
    // locale) the explorer types into matching fields during replay.
    std::fs::create_dir_all(".reproit").ok();
    let mut cfg = serde_json::Map::new();
    cfg.insert("replay".to_string(), serde_json::json!(replay));
    if !fixture.is_empty() {
        let fc = fixture.to_config();
        if let Some(obj) = fc.as_object() {
            for (k, v) in obj {
                cfg.insert(k.clone(), v.clone());
            }
        }
    }
    let cfg_path = ".reproit/fuzz_config.json";
    std::fs::write(
        cfg_path,
        serde_json::to_string_pretty(&serde_json::Value::Object(cfg))?,
    )
    .with_context(|| format!("writing {cfg_path}"))?;
    println!("Deterministic replay written to {cfg_path}:");
    if replay.is_empty() {
        println!("  (no navigation actions: data-only reproduction)");
    } else {
        println!("  {}", replay.join(" -> "));
    }
    if !fixture.is_empty() {
        println!("  property-matched fixture: {}", fixture.summary());
    }

    if !run {
        println!("\nRun it with:  reproit check {journey} --record --warm   (or pass --run here)");
        return Ok(());
    }
    println!("\nRunning the replay ({journey})...");
    let exe = std::env::current_exe()?;
    let out = std::process::Command::new(exe)
        .args(["check", journey, "--record", "--warm", "--json"])
        .output()
        .context("spawning reproit check")?;
    let log = String::from_utf8_lossy(&out.stdout);
    // Use `check`'s deterministic verdict (its --json `outcome`) rather than
    // grepping, so "replayed clean" and "could not replay" are distinct.
    let outcome = log
        .find('{')
        .zip(log.rfind('}'))
        .filter(|(i, j)| j > i)
        .and_then(|(i, j)| serde_json::from_str::<serde_json::Value>(&log[i..=j]).ok())
        .and_then(|v| v["outcome"].as_str().map(String::from));
    let marker = log
        .lines()
        .find(|l| l.contains("EXCEPTION CAUGHT"))
        .unwrap_or("");
    // A real `check` run always emits its JSON verdict (even on pass) or an
    // EXCEPTION marker. NEITHER present means the replay never started -- e.g.
    // `check` could not resolve the repro/journey and exited 1 during setup.
    // Without this guard, classify_repro's exit-code fallback reads that setup
    // exit-1 as `Reproduced` and prints a FALSE "REPRODUCED" though nothing ran.
    if outcome.is_none() && marker.is_empty() {
        println!(
            "COULD NOT RUN the replay: `check {journey}` produced no verdict (exit {:?}); \
             this is a setup error (the repro/journey did not resolve), not a reproduction.",
            out.status.code()
        );
        return Ok(());
    }
    match classify_repro(outcome.as_deref(), out.status.code()) {
        ReproVerdict::Reproduced => {
            println!("REPRODUCED: the replay re-triggered the failure in this build. {marker}");
        }
        ReproVerdict::Clean => {
            println!(
                "NOT reproduced: the path replayed CLEAN (the bug did not fire). Likely \
                 data-dependent (the production session carried data this replay does not)."
            );
            println!("  -> synthesize from context: {}", repro["context"]);
        }
        ReproVerdict::Stale => {
            println!(
                "COULD NOT REPLAY (stale): the app changed since this session, so a targeted \
                 control is gone. This is NOT a verdict on the bug. Rebuild the map (`reproit \
                 map`) and retry; the bug may also be fixed by the UI change."
            );
        }
        ReproVerdict::Flaky => {
            println!(
                "FLAKY: the failure reproduced inconsistently across replays (an app race), \
                 not a clean reproduction."
            );
        }
        ReproVerdict::Unknown => {
            println!("Could not classify the replay (no verdict from `reproit check`).");
        }
    }
    Ok(())
}

pub async fn diagnose(
    app: &str,
    report: &str,
    run: bool,
    journey: &str,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud.clone(), key.clone());
    let errors = c.get(&format!("/v1/errors/{app}")).await?;
    let empty = vec![];
    let list = errors["errors"].as_array().unwrap_or(&empty);
    if list.is_empty() {
        println!("No production errors recorded for '{app}' yet.");
        return Ok(());
    }
    // Rank candidates by overlap between the report's words and the error
    // message (a cheap, honest first pass; an LLM rerank can slot in later).
    let words: Vec<String> = report
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 3)
        .map(String::from)
        .collect();
    let mut scored: Vec<(usize, usize)> = list
        .iter()
        .enumerate()
        .map(|(i, e)| {
            // Match the report against the message AND the action trail, since
            // symptoms ("compose", "new post") live in the path, not the message.
            let mut hay = e["message"].as_str().unwrap_or("").to_lowercase();
            if let Some(path) = e["path"].as_array() {
                for step in path {
                    if let Some(a) = step["action"].as_str() {
                        hay.push(' ');
                        hay.push_str(&a.to_lowercase());
                    }
                }
            }
            let score = words.iter().filter(|w| hay.contains(w.as_str())).count();
            (i, score)
        })
        .collect();
    scored.sort_by_key(|b| std::cmp::Reverse(b.1));
    let (best, score) = scored[0];
    println!("Report: \"{report}\"");
    if score == 0 {
        println!("\nNo strong textual match. Best-effort: showing the most frequent cluster.\n");
    } else {
        println!("\nBest match (#{best}, {score} term overlap):\n");
    }
    explain(app, None, Some(best), cloud.clone(), key.clone()).await?;
    if run {
        println!();
        reproduce(app, best, journey, true, cloud, key).await?;
    }
    Ok(())
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s).trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_repro_distinguishes_clean_from_stale() {
        // The JSON verdict wins.
        assert_eq!(
            classify_repro(Some("fail"), Some(0)),
            ReproVerdict::Reproduced
        );
        assert_eq!(classify_repro(Some("pass"), Some(0)), ReproVerdict::Clean);
        assert_eq!(classify_repro(Some("stale"), Some(0)), ReproVerdict::Stale);
        assert_eq!(classify_repro(Some("flaky"), Some(0)), ReproVerdict::Flaky);
        // No JSON: fall back to the exit-code contract (1/2/3/0).
        assert_eq!(classify_repro(None, Some(1)), ReproVerdict::Reproduced);
        assert_eq!(classify_repro(None, Some(2)), ReproVerdict::Flaky);
        assert_eq!(classify_repro(None, Some(3)), ReproVerdict::Stale);
        assert_eq!(classify_repro(None, Some(0)), ReproVerdict::Clean);
        assert_eq!(classify_repro(None, None), ReproVerdict::Unknown);
        // The old bug: a stale run (exit 3 / outcome stale) must NOT read as
        // reproduced just because the process did not exit 0.
        assert_ne!(
            classify_repro(Some("stale"), Some(3)),
            ReproVerdict::Reproduced
        );
    }

    #[test]
    fn filter_errors_keeps_matching_messages() {
        let v = json!({ "errors": [
            { "message": "RangeError in feed" },
            { "message": "Null check operator on login" },
            { "message": "RangeError again" },
        ]});
        let out = filter_errors(v, Some("rangeerror"));
        let arr = out["errors"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().all(|e| e["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("rangeerror")));
    }

    #[test]
    fn filter_errors_none_query_is_identity() {
        let v = json!({ "errors": [ { "message": "a" }, { "message": "b" } ] });
        let out = filter_errors(v.clone(), None);
        assert_eq!(out, v);
    }

    #[test]
    fn filter_errors_tolerates_missing_array() {
        let v = json!({ "unexpected": true });
        let out = filter_errors(v.clone(), Some("x"));
        assert_eq!(out, v);
    }
}
