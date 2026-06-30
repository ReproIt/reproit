//! Production-bug triage: the "here's my issue, look at it and reproduce it"
//! flow, over the cloud's telemetry. Pairs with `reproit mcp` so a coding agent
//! (or a person) can ask, in plain words, what a bug might be and get a
//! deterministic reproduction.
//!
//! - `find`: list production error clusters + their context discriminator.
//! - `explain`: one bucket package in full (path, "which users" discriminator,
//!   suspected source from the stack, and the replay).
//! - `reproduce`: pull a bucket package, then run the saved local repro.
//! - `diagnose`: match a free-text report to a cluster, then explain (+repro).
//!
//! The cloud base URL/key come from --cloud/--key, then REPROIT_CLOUD_URL /
//! REPROIT_CLOUD_KEY, then the hosted cloud. Output is plain text so MCP can
//! relay it.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::repro;

struct Cloud {
    base: String,
    key: Option<String>,
}

impl Cloud {
    fn new(cloud: Option<String>, key: Option<String>) -> Self {
        // Defaults to the hosted cloud; set REPROIT_CLOUD_URL to point elsewhere.
        let base = cloud
            .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
            .unwrap_or_else(|| "https://cloud.reproit.com".to_string());
        // Cloud key precedence: explicit key (already resolved by `cloud_creds`)
        // > REPROIT_CLOUD_KEY (the project key, sk_live_...).
        let key = key.or_else(|| std::env::var("REPROIT_CLOUD_KEY").ok());
        Cloud {
            base: base.trim_end_matches('/').to_string(),
            key,
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
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

    /// POST a JSON body to an arbitrary cloud path, mirroring `get`: bearer-auth
    /// when a key is present, bail with a clear message on a non-2xx, and parse
    /// the (JSON) response body. Used by the triage SET path.
    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        let mut req = client.post(format!("{}{}", self.base, path)).json(body);
        if let Some(k) = &self.key {
            req = req.bearer_auth(k);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("POST {}{}", self.base, path))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("cloud {} -> {}: {}", path, status, body.trim());
        }
        serde_json::from_str(&body).with_context(|| format!("parsing {path}"))
    }
}

/// Raw GET against the cloud errors namespace: `/v1/errors/:app{suffix}`. Used
/// by legacy cluster/cohort export paths (`cloud findings --export`, etc.) to
/// surface the unrendered JSON those views are built from. Fails
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

/// Raw bucket list payload: `/v1/apps/:app/buckets`. This is the bucket-first
/// export surface behind `cloud query --export` and `cloud buckets --json`.
pub async fn raw_buckets(app: &str, cloud: Option<String>, key: Option<String>) -> Result<Value> {
    let c = Cloud::new(cloud, key);
    c.get(&format!("/v1/apps/{app}/buckets")).await
}

/// Validate a cloud/project key for `cloud login` by hitting an AUTHENTICATED
/// endpoint, so login proves the key actually WORKS (not just that the host is
/// up). With an `app`, validates against `GET /v1/apps/:app/buckets` (the loop's
/// real entrypoint); without one, against `GET /v1/me`. A 401/403 is a clear
/// "bad key" error; any other non-2xx surfaces the status. On success returns a
/// short human description of what the key resolved to.
pub async fn validate_login(base: &str, key: &str, app: Option<&str>) -> Result<String> {
    let base = base.trim_end_matches('/');
    let path = match app {
        Some(a) => format!("/v1/apps/{a}/buckets"),
        None => "/v1/me".to_string(),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default();
    let req = client.get(format!("{base}{path}")).bearer_auth(key);
    let resp = req
        .send()
        .await
        .with_context(|| format!("validating key against {base}{path}"))?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        anyhow::bail!("the cloud rejected the key ({status}); check it is a valid sk_live_... key");
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{base}{path} -> {status}: {}", body.trim());
    }
    // Describe what the key resolved to, without assuming a shape (the two
    // endpoints differ). For /v1/me: orgId + project count; for the buckets list:
    // the bucket count. Best-effort: a 2xx already proved the key is accepted.
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    match app {
        Some(a) => {
            let n = body["buckets"]
                .as_array()
                .map(|a| a.len())
                .or_else(|| body.as_array().map(|a| a.len()));
            match n {
                Some(n) => Ok(format!("key accepted for app {a} ({n} buckets)")),
                None => Ok(format!("key accepted for app {a}")),
            }
        }
        None => {
            // orgId is an integer (tenant.org_id) but tolerate a string too.
            let org = body["orgId"]
                .as_i64()
                .map(|n| n.to_string())
                .or_else(|| body["orgId"].as_str().map(String::from));
            let projects = body["projects"].as_u64();
            match (org, projects) {
                (Some(o), Some(p)) => Ok(format!("key accepted (org {o}, {p} projects)")),
                (Some(o), None) => Ok(format!("key accepted (org {o})")),
                _ => Ok("key accepted".to_string()),
            }
        }
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

/// Filter a bucket list response by free text across the fields users actually
/// search: bucket id, crash signature, repro hint, and message.
pub fn filter_buckets(mut v: Value, query: Option<&str>) -> Value {
    let Some(q) = query.map(|s| s.to_lowercase()) else {
        return v;
    };
    if let Some(arr) = v.get_mut("items").and_then(Value::as_array_mut) {
        arr.retain(|b| {
            ["bucketId", "crashSig", "repro", "message"]
                .iter()
                .filter_map(|field| b.get(field).and_then(Value::as_str))
                .any(|s| s.to_lowercase().contains(&q))
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

/// `cloud buckets`: the IMPACT-RANKED bug list, each with its content-addressed
/// `bucketId` -- the id the rest of the loop (`pull`/`triage`/`timeline`) keys
/// off. GETs `/v1/apps/:app/buckets` (already impact-sorted server-side). This is
/// the entry point the agent loop starts from: it's the ONLY place the `bkt_...`
/// id is surfaced. Distinct from `find` (the cohort "who's affected" lens over
/// `/v1/errors/:app/cohorts`, which carries sig/count/who but no bucket id).
pub async fn buckets(
    app: &str,
    query: Option<&str>,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let v = filter_buckets(raw_buckets(app, cloud, key).await?, query);
    if json {
        // Raw, already impact-sorted payload straight through for an agent.
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let empty = vec![];
    let items = v["items"].as_array().unwrap_or(&empty);
    let mut shown = 0;
    println!("Impact-ranked buckets for '{app}' (highest impact first):");
    for it in items {
        let msg = it["message"].as_str().unwrap_or("");
        let id = it["bucketId"].as_str().unwrap_or("?");
        let count = it["count"].as_u64().unwrap_or(0);
        let score = it["impact"]["score"].as_f64().unwrap_or(0.0);
        let severity = it["impact"]["severity"].as_str().unwrap_or("?");
        let resolution = it["resolution"]["status"].as_str().unwrap_or("?");
        // One tight, agent-readable row: the id (the loop key) leads, then the
        // ranking signals, then the message.
        println!("\n  [{id}]  impact {score:.2} ({severity})  resolution {resolution}  x{count}");
        println!("    {}", first_line(msg));
        shown += 1;
    }
    if shown == 0 {
        if items.is_empty() {
            println!("  (no buckets yet)");
        } else {
            println!("  (no buckets match the query)");
        }
    }
    println!(
        "\nPull the top one: reproit cloud pull --app {app} --top --as next-fix   (then reproit check next-fix)"
    );
    Ok(())
}

/// Resolve the current top bucket id from the impact-ranked bucket list. This is
/// intentionally small and shares the same server ordering as `cloud buckets`.
pub async fn top_bucket_id(
    app: &str,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<String> {
    let c = Cloud::new(cloud, key);
    let v = c.get(&format!("/v1/apps/{app}/buckets")).await?;
    let items = v["items"]
        .as_array()
        .context("cloud buckets response did not include an items array")?;
    let top = items.first().context(
        "no buckets available to pull; run `reproit cloud buckets` after production data arrives",
    )?;
    let id = top["bucketId"]
        .as_str()
        .context("top bucket did not include bucketId")?;
    Ok(id.to_string())
}

pub async fn explain(
    app: &str,
    bucket: Option<&str>,
    sig: Option<&str>,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud, key);
    let buckets = c.get(&format!("/v1/apps/{app}/buckets")).await?;
    let empty = vec![];
    let list = buckets["items"].as_array().unwrap_or(&empty);
    let item = match (bucket, sig) {
        (Some(bucket), _) => list
            .iter()
            .find(|b| b["bucketId"].as_str() == Some(bucket))
            .with_context(|| {
                format!(
                    "no bucket `{bucket}` in app `{app}`; run `reproit cloud buckets --app {app}`"
                )
            })?,
        (None, Some(sig)) => list
            .iter()
            .find(|b| b["crashSig"].as_str() == Some(sig))
            .with_context(|| {
                format!(
                    "no bucket with crash signature `{sig}`; run `reproit cloud buckets --app {app}`"
                )
            })?,
        (None, None) => list.first().with_context(|| {
            format!("no buckets available for `{app}`; run `reproit cloud buckets --app {app}`")
        })?,
    };
    let bucket = item["bucketId"]
        .as_str()
        .context("bucket list item did not include bucketId")?;
    let pkg = c.get(&format!("/v1/apps/{app}/buckets/{bucket}")).await?;
    let crash_sig = pkg["crashSig"]
        .as_str()
        .or_else(|| item["crashSig"].as_str())
        .unwrap_or("?");
    let msg = pkg["message"]
        .as_str()
        .or_else(|| item["message"].as_str())
        .unwrap_or("");
    let count = pkg["count"]
        .as_u64()
        .or_else(|| item["count"].as_u64())
        .unwrap_or(0);
    let ds = pkg["discriminators"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let replay = pkg["replay"].as_array().cloned().unwrap_or_default();

    println!("Bucket [{bucket}] in '{app}'");
    println!("  crash:     {crash_sig}");
    println!("  message:   {}", first_line(msg));
    if let Some(src) = suspected_source(msg) {
        println!("  suspected: {src}");
    }
    println!("  count:     {count}");
    println!("  who:       {}", fmt_discriminators(&ds));
    if let Some(start) = pkg["startSig"].as_str().filter(|s| !s.is_empty()) {
        println!("  path:      {start} -> {crash_sig}");
    }
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

    println!(
        "\nReproduce: reproit cloud reproduce --app {app} --bucket {bucket} --as <name> --run"
    );
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

/// Spawn `reproit check <target> --json`, read its deterministic verdict, and
/// print a human reproduction summary. Used by `reproduce_bucket`, where
/// `<target>` is the just-pulled repro's alias.
fn run_check_and_classify(target: &str, context_hint: Option<&Value>) -> Result<()> {
    println!("\nRunning the replay ({target})...");
    let exe = std::env::current_exe()?;
    // `check` has no `--warm` flag; a plain check replays the saved repro.
    let out = std::process::Command::new(exe)
        .args(["check", target, "--json"])
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
            "COULD NOT RUN the replay: `check {target}` produced no verdict (exit {:?}); \
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
            if let Some(ctx) = context_hint {
                println!("  -> synthesize from context: {ctx}");
            }
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

/// Bucket-first `cloud reproduce`: `pull` the content-addressed bucket into a
/// first-class LOCAL repro named `as_name`, then (with `run`) `check` it. This is
/// the one-step "show me this prod bug locally" verb; it REUSES the existing pull
/// + check code paths (no duplicated materialize/replay logic), so the pulled
/// repro carries its property-matched fixture and replays exactly as a kept one.
#[allow(clippy::too_many_arguments)]
pub async fn reproduce_bucket(
    root: &std::path::Path,
    app: &str,
    bucket: &str,
    as_name: &str,
    run: bool,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    // Pull is the ONE cloud boundary: it writes .reproit/repros/<id>/{meta,replay}
    // (fixture folded in) and prints the save summary + the `check` hint.
    pull(root, app, bucket, as_name, json, cloud, key).await?;
    if !run {
        return Ok(());
    }
    // Reuse the standard local verification by alias; no context hint (the pulled
    // repro carries its own fixture, so a CLEAN verdict is a genuine no-repro).
    run_check_and_classify(as_name, None)
}

/// What a pulled cloud package materializes into LOCALLY: the same on-disk
/// artifacts `keep` writes (`meta.json` + `replay.json`), so a pulled repro is
/// byte-identical in SHAPE to a kept one and `check` reads it unchanged. This is
/// the PURE core of `cloud pull`: a replay-package JSON in, a `Meta` + action
/// sequence + property-matched fixture out, with no network and no filesystem.
/// The boundary is one explicit verb; once materialized, the repro is
/// local-first-class.
///
/// The `fixture` carries the property-matched replay data (tier 3) synthesized
/// from the package's `fixtureSpec`: the locale + per-field concrete values a
/// data-dependent prod bug needs. `build_replay_json` folds it into replay.json
/// so it flows through `check` to the runner, NOT just sits in meta.
pub struct PulledRepro {
    pub meta: repro::Meta,
    pub actions: Vec<String>,
    pub fixture: crate::fixture::Fixture,
}

/// Build the replay.json a pulled (or kept) repro stores on disk, in the EXACT
/// shape `check_repro` reads and forwards verbatim to the runner's fuzz config:
/// `{ "seed", "replay", [inputs], [locale] }`. The `inputs`/`locale` keys are the
/// property-matched fixture (`Fixture::to_config`), spread at the TOP LEVEL so the
/// web/RN/native runners read them per-seed (they read `inputs` off each seed
/// config; `check_repro` resolves a top-level `locale` to `REPROIT_LOCALE`). This
/// is the SAME shape `reproduce` writes into `.reproit/tmp/fuzz_config.json`, so a
/// pulled repro and a `reproduce`d one drive the runner identically.
pub fn build_replay_json(
    seed: u64,
    actions: &[String],
    fixture: &crate::fixture::Fixture,
) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("seed".to_string(), serde_json::json!(seed));
    m.insert("replay".to_string(), serde_json::json!(actions));
    if !fixture.is_empty() {
        // Spread the fixture's `inputs`/`locale` at the top level, matching the
        // shape `reproduce` builds for the fuzz config (so the runner consumes
        // them the same way on a pulled repro as on a `reproduce`d one).
        if let Some(obj) = fixture.to_config().as_object() {
            for (k, v) in obj {
                m.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(m)
}

/// Materialize a cloud replay package into a local saved repro, EXACTLY as
/// `keep` would write one.
///
/// Field mapping (faithful to `keep_repro` in main.rs):
///   - `replay`      -> the action sequence (PII-safe `tap:`/`key:`/`type:<sel>=<class>`).
///   - `seed`        -> the package's `seed` if present, else 0 (cloud sessions
///                      are deterministic replays, not seeded fuzz runs).
///   - `id`          -> the content hash over (seed + normalized actions), the
///                      SAME `repro_id` `keep` uses (self-deduping across machines).
///   - `alias`       -> the explicit `--as <name>`.
///   - `trigger_index` -> the replay length (the finding fired after performing
///                      all of them), mirroring `keep`.
///   - `trigger_sig` -> the package's `crashSig` (or `startSig` fallback) when
///                      present, so `check` can re-confirm the same finding.
///   - `oracle`      -> "crash" (cloud error events are crash-class).
///   - `status`      -> quarantined (a fresh save, like a fresh keep).
pub fn materialize_pull(pkg: &Value, as_name: &str, created: &str) -> Result<PulledRepro> {
    let actions: Vec<String> = pkg["replay"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if actions.is_empty() {
        anyhow::bail!(
            "the cloud package has no executable replay actions (its `replay` is empty); \
             there is nothing to reproduce locally"
        );
    }
    let seed = pkg["seed"].as_u64().unwrap_or(0);
    let id = repro::repro_id(seed, &actions);
    // The crash signature re-confirms the SAME finding on replay; fall back to the
    // session's start sig, then None (the trigger_index does the work alone).
    let trigger_sig = pkg["crashSig"]
        .as_str()
        .or_else(|| pkg["startSig"].as_str())
        .map(String::from)
        .filter(|s| !s.is_empty());
    let meta = repro::Meta {
        id,
        alias: Some(as_name.to_string()),
        status: repro::Status::Quarantined,
        seed,
        created: created.to_string(),
        last_checked: None,
        last_result: None,
        trigger_index: Some(repro::normalize_actions(&actions).len()),
        trigger_sig,
        oracle: Some("crash".to_string()),
    };
    // Property-matched replay (tier 3): synthesize the concrete locale + per-field
    // values from the cloud's `fixtureSpec`, the SAME way `reproduce` does, so a
    // data-dependent prod bug (a 312-char unicode name, an RTL field, a specific
    // locale/role/plan) actually reproduces under a later `check`. Empty spec ->
    // empty fixture (a path-only repro), so this is inert for non-data bugs.
    let fixture = crate::fixture::synthesize(&pkg["fixtureSpec"]);
    Ok(PulledRepro {
        meta,
        actions,
        fixture,
    })
}

/// `cloud pull`: EXPLICITLY download a cloud bucket as a first-class LOCAL repro.
///
/// This is the ONE cloud boundary in the check loop: it fetches the bucket's
/// replay package (the content-addressed `GET /v1/apps/:app/buckets/:bucket`),
/// materializes it the way `keep` does, and writes
/// `.reproit/repros/<id>/{meta,replay}.json`. After this, `reproit check <name>`
/// runs the STANDARD local, network-free verification and `reproit repros` lists
/// it -- indistinguishable from a locally found repro.
pub async fn pull(
    root: &std::path::Path,
    app: &str,
    bucket: &str,
    as_name: &str,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud, key);
    // The content-addressed bucket endpoint (matches the content-hash model).
    let pkg = c.get(&format!("/v1/apps/{app}/buckets/{bucket}")).await?;
    let source = format!("bucket {bucket}");

    let pulled = materialize_pull(&pkg, as_name, &chrono::Local::now().to_rfc3339())?;
    let meta = &pulled.meta;

    // Write the SAME two artifacts `keep` writes, so `check` reads it unchanged:
    // replay.json for the action sequence (PLUS the property-matched fixture's
    // inputs/locale when the bug is data-dependent, so it flows through `check` to
    // the runner), meta.json for the identity + trigger context + alias.
    let dir = repro::repro_dir(root, &meta.id);
    std::fs::create_dir_all(&dir)?;
    let replay = build_replay_json(meta.seed, &pulled.actions, &pulled.fixture);
    std::fs::write(
        dir.join("replay.json"),
        serde_json::to_string_pretty(&replay)?,
    )
    .with_context(|| format!("writing {}", dir.join("replay.json").display()))?;
    repro::save_meta(root, meta)?;

    let expected = pkg["expectedError"]
        .as_str()
        .or_else(|| pkg["message"].as_str())
        .map(first_line)
        .unwrap_or("(unknown)");
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "command": "cloud pull",
                "app": app,
                "bucket": bucket,
                "id": repro::display_repro_id(&meta.id),
                "kind": "repro",
                "alias": as_name,
                "status": meta.status.as_str(),
                "expected": expected,
                "signature": meta.trigger_sig,
                "actions": pulled.actions,
                "fixture": (!pulled.fixture.is_empty()).then(|| pulled.fixture.summary()),
                "dir": dir.to_string_lossy(),
            }))?
        );
        return Ok(());
    }
    println!("Pulled {source} from '{app}' as a local repro.");
    println!("  expected:  {expected}");
    if let Some(sig) = &meta.trigger_sig {
        println!("  signature: {sig}");
    }
    println!("  replay:    {}", pulled.actions.join(" -> "));
    if !pulled.fixture.is_empty() {
        println!("  fixture:   {}", pulled.fixture.summary());
    }
    println!(
        "  saved:     {} ({}, alias {})",
        repro::display_repro_id(&meta.id),
        meta.status.as_str(),
        as_name
    );
    println!("  files:     {}", dir.join("meta.json").display());
    println!("\nnow run: reproit check {as_name}   (standard local verification, no network)");
    Ok(())
}

/// `cloud triage`: READ or SET a bucket's triage status (the management state a
/// dev/agent acts on, distinct from prod-truth resolution).
///
/// With no `status`, GETs `/v1/apps/:app/buckets/:bucket/triage` and renders the
/// current state. With a `status`, POSTs the same endpoint with the body the
/// cloud's `post_triage` expects (`{status, fixedInBuild?, assignee?}`) and
/// renders the persisted state back. `fixed_in_build`/`assignee` are only
/// meaningful for the matching statuses (the cloud enforces coherence: `fixed`
/// takes a build anchor, `assigned` requires an assignee, others must not carry
/// one), so we forward them and let the server be the authority.
///
/// Agent use: after a local `check` proves a fix holds, set `--status fixed
/// --fixed-in-build <ver>` to RECORD the intent; production then confirms or
/// regresses it (read back via `resolution_events`).
#[allow(clippy::too_many_arguments)]
pub async fn triage(
    app: &str,
    bucket: &str,
    status: Option<&str>,
    fixed_in_build: Option<&str>,
    assignee: Option<i64>,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud, key);
    let path = format!("/v1/apps/{app}/buckets/{bucket}/triage");
    let v = match status {
        // SET: POST the cloud's expected body. Only emit the optional anchors when
        // present so a `triaged`/`wontfix` move doesn't carry a stray field.
        Some(s) => {
            let mut body = serde_json::Map::new();
            body.insert("status".into(), Value::from(s));
            if let Some(fib) = fixed_in_build {
                body.insert("fixedInBuild".into(), Value::from(fib));
            }
            if let Some(a) = assignee {
                body.insert("assignee".into(), Value::from(a));
            }
            c.post(&path, &Value::Object(body)).await?
        }
        // READ: GET the current state.
        None => c.get(&path).await?,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    let t = &v["triage"];
    let suffix = if status.is_some() { " (set)" } else { "" };
    println!("Triage for bucket {bucket} in '{app}'{suffix}:");
    println!("  status:    {}", t["status"].as_str().unwrap_or("?"));
    let assignee = &t["assignee"];
    if !assignee.is_null() {
        println!("  assignee:  {assignee}");
    }
    let fib = &t["fixedInBuild"];
    if !fib.is_null() {
        println!("  fixed in:  {}", fib.as_str().unwrap_or("?"));
    }
    // The server returns snake_case `updated_at`; tolerate the camelCase form too.
    if let Some(updated) = t["updated_at"].as_str().or_else(|| t["updatedAt"].as_str()) {
        println!("  updated:   {updated}");
    }
    if status.is_none() {
        println!(
            "\nSet it with: reproit cloud triage --app {app} --bucket {bucket} --status fixed --fixed-in-build <ver>"
        );
    } else {
        println!(
            "\nMonitor prod-truth: reproit cloud resolution-events --app {app}   (watch for a regression)"
        );
    }
    Ok(())
}

/// `cloud resolution-events`: list recent prod-truth TRANSITIONS the background
/// pass recorded (`resolved->regressed`, `resolving->resolved`, ...), newest
/// first. GETs `/v1/apps/:app/resolution-events`.
///
/// Agent use: an autonomous monitor reads this to see what REGRESSED after it
/// marked a bucket fixed (the gap between dev intent and production reality).
pub async fn resolution_events(
    app: &str,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud, key);
    let v = c.get(&format!("/v1/apps/{app}/resolution-events")).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let empty = vec![];
    let events = v["events"].as_array().unwrap_or(&empty);
    println!("Recent resolution events for '{app}':");
    if events.is_empty() {
        println!("  (none yet -- no fix anchors have been confirmed or regressed)");
        return Ok(());
    }
    for e in events {
        let bucket = e["bucketId"].as_str().unwrap_or("?");
        let from = e["fromStatus"].as_str().unwrap_or("new");
        let to = e["toStatus"].as_str().unwrap_or("?");
        let at = e["at"].as_str().unwrap_or("?");
        let build = e["build"]
            .as_str()
            .map(|b| format!(" on {b}"))
            .unwrap_or_default();
        // REGRESSED is the loud one: flag it so the agent's eye lands on it.
        let mark = if to == "regressed" { "!! " } else { "   " };
        println!("{mark}[{bucket}] {from} -> {to}{build}  ({at})");
    }
    Ok(())
}

/// `cloud timeline`: the per-bucket OCCURRENCE time-series (segmented by build)
/// plus the computed prod-truth resolution. GETs
/// `/v1/apps/:app/buckets/:bucket/timeline`. The series shows whether occurrences
/// dropped (resolving/resolved) or returned (regressed) after a fix anchor.
pub async fn timeline(
    app: &str,
    bucket: &str,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud, key);
    let v = c
        .get(&format!("/v1/apps/{app}/buckets/{bucket}/timeline"))
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let empty = vec![];
    let series = v["series"].as_array().unwrap_or(&empty);
    println!("Occurrence timeline for bucket {bucket} in '{app}':");
    println!("  total occurrences: {}", v["total"].as_u64().unwrap_or(0));
    if let Some(status) = v["resolution"]["status"].as_str() {
        println!("  prod-truth:        {status}");
    }
    if series.is_empty() {
        println!("  (no occurrences recorded yet)");
        return Ok(());
    }
    for cell in series {
        let window = cell["window"].as_str().unwrap_or("?");
        let count = cell["count"].as_u64().unwrap_or(0);
        let build = cell["build"]
            .as_str()
            .map(|b| format!(" [{b}]"))
            .unwrap_or_default();
        println!("  {window}{build}  x{count}");
    }
    Ok(())
}

pub async fn diagnose(
    app: &str,
    report: &str,
    run: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud.clone(), key.clone());
    let buckets = c.get(&format!("/v1/apps/{app}/buckets")).await?;
    let empty = vec![];
    let list = buckets["items"].as_array().unwrap_or(&empty);
    if list.is_empty() {
        println!("No production buckets recorded for '{app}' yet.");
        return Ok(());
    }
    // Rank candidates by overlap between the report's words and the bucket
    // summary/signature (a cheap, honest first pass; an LLM rerank can slot in
    // later).
    let words: Vec<String> = report
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 3)
        .map(String::from)
        .collect();
    let mut scored: Vec<(usize, usize)> = list
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut hay = String::new();
            for field in ["message", "crashSig", "bucketId", "repro"] {
                if let Some(s) = b[field].as_str() {
                    hay.push(' ');
                    hay.push_str(&s.to_lowercase());
                }
            }
            let score = words.iter().filter(|w| hay.contains(w.as_str())).count();
            (i, score)
        })
        .collect();
    scored.sort_by_key(|b| std::cmp::Reverse(b.1));
    let (best, score) = scored[0];
    let bucket = list[best]["bucketId"]
        .as_str()
        .context("matched bucket did not include bucketId")?;
    println!("Report: \"{report}\"");
    if score == 0 {
        println!("\nNo strong textual match. Best-effort: showing the most frequent cluster.\n");
    } else {
        println!("\nBest match ({bucket}, {score} term overlap):\n");
    }
    explain(app, Some(bucket), None, cloud.clone(), key.clone()).await?;
    if run {
        println!(
            "\n`cloud diagnose --run` now resolves the bucket only. Pull and run it with:\n  reproit cloud reproduce --app {app} --bucket {bucket} --as <name> --run"
        );
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
    fn materialize_pull_writes_a_checkable_repro_shape() {
        // A bucket replay package (the content-addressed endpoint's shape) ->
        // Meta + actions identical in SHAPE to what `keep` writes, so `check`
        // reads it unchanged. This is the PURE core of `cloud pull` (no network,
        // no fs): given the package JSON, materialize the local repro.
        let pkg = json!({
            "bucketId": "b00b",
            "expectedError": "Uncaught TypeError: state.reset is not a function",
            "crashSig": "crash:TypeError:state.reset",
            "startSig": "home",
            "replay": ["tap:key:id:reset", "key:Enter"],
            "fixtureSpec": {},
        });
        let pulled = materialize_pull(&pkg, "login-crash", "2026-06-21T00:00:00+00:00").unwrap();

        // The action sequence is the package's PII-safe replay, in order.
        assert_eq!(pulled.actions, vec!["tap:key:id:reset", "key:Enter"]);

        let m = &pulled.meta;
        // Identity: the SAME content hash `keep`/`check` use (seed 0, normalized
        // actions). 12 hex chars, deterministic.
        assert_eq!(m.id, repro::repro_id(0, &pulled.actions));
        assert_eq!(m.id.len(), 12);
        // Alias = --as; status quarantined (a fresh save); seed defaulted to 0.
        assert_eq!(m.alias.as_deref(), Some("login-crash"));
        assert_eq!(m.status, repro::Status::Quarantined);
        assert_eq!(m.seed, 0);
        // Trigger context: index = replay length, sig = crashSig, oracle = crash.
        assert_eq!(m.trigger_index, Some(2));
        assert_eq!(
            m.trigger_sig.as_deref(),
            Some("crash:TypeError:state.reset")
        );
        assert_eq!(m.oracle.as_deref(), Some("crash"));
        assert_eq!(m.created, "2026-06-21T00:00:00+00:00");
        // An empty fixtureSpec -> empty fixture: replay.json is the bare
        // {seed, replay} shape, no inputs/locale (a path-only repro).
        assert!(pulled.fixture.is_empty());
        let replay = build_replay_json(m.seed, &pulled.actions, &pulled.fixture);
        assert_eq!(replay["seed"], json!(0));
        assert_eq!(replay["replay"], json!(["tap:key:id:reset", "key:Enter"]));
        assert!(replay.get("inputs").is_none());
        assert!(replay.get("locale").is_none());
    }

    #[test]
    fn pull_preserves_fixture_in_replay_json() {
        // TASK 1: a data-dependent prod bug (locale + a long-name field) must pull
        // with its property-matched fixture FOLDED INTO replay.json, in the shape
        // `check_repro` forwards verbatim to the runner (top-level `inputs`, and a
        // top-level `locale` it lifts to REPROIT_LOCALE). Without this the repro
        // pulls path-only and replays clean (the bug never fires).
        let pkg = json!({
            "expectedError": "RangeError: index out of range",
            "crashSig": "crash:RangeError:render",
            "replay": ["tap:key:id:name", "type:key:id:name=longname"],
            "fixtureSpec": {
                "locale": "tr",
                "inputs": [{
                    "field": "name",
                    "generate": { "charset": "unicode", "minLen": 312 },
                }],
            },
        });
        let pulled = materialize_pull(&pkg, "name-crash", "t").unwrap();
        assert!(
            !pulled.fixture.is_empty(),
            "the fixtureSpec carries locale + a field, so the fixture is non-empty"
        );

        let replay = build_replay_json(pulled.meta.seed, &pulled.actions, &pulled.fixture);
        // The action sequence is preserved as before.
        assert_eq!(
            replay["replay"],
            json!(["tap:key:id:name", "type:key:id:name=longname"])
        );
        // Locale is lifted to a top-level key (check_repro forwards it to
        // REPROIT_LOCALE when no explicit --locale is given).
        assert_eq!(replay["locale"], json!("tr"));
        // The per-field synthesized value lands in a top-level `inputs` array,
        // exactly where the runner's loadInputs() reads it off each seed config.
        let inputs = replay["inputs"].as_array().expect("inputs array present");
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["field"], json!("name"));
        // A concrete, non-empty synthesized value (deterministic; no RNG).
        let v = inputs[0]["value"].as_str().expect("a string value");
        assert!(
            !v.is_empty(),
            "the long-name field synthesized to a concrete value"
        );
    }

    #[test]
    fn materialize_pull_honors_seed_and_startsig_fallback() {
        // A package carrying an explicit seed and NO crashSig: seed flows into the
        // id, and the trigger sig falls back to startSig.
        let pkg = json!({
            "seed": 7,
            "startSig": "checkout",
            "replay": ["tap:key:id:pay"],
        });
        let pulled = materialize_pull(&pkg, "pay", "t").unwrap();
        assert_eq!(pulled.meta.seed, 7);
        assert_eq!(pulled.meta.id, repro::repro_id(7, &["tap:key:id:pay"]));
        assert_eq!(pulled.meta.trigger_sig.as_deref(), Some("checkout"));
    }

    #[test]
    fn materialize_pull_rejects_empty_replay() {
        // A package with no executable actions cannot become a check-able repro.
        let pkg = json!({ "replay": [], "crashSig": "x" });
        assert!(materialize_pull(&pkg, "x", "t").is_err());
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

    #[test]
    fn filter_buckets_matches_bucket_identity_fields() {
        let v = json!({ "items": [
            { "bucketId": "bkt_feed", "crashSig": "sig_a", "message": "RangeError in feed" },
            { "bucketId": "bkt_login", "crashSig": "sig_b", "message": "Null check" },
            { "bucketId": "bkt_cart", "crashSig": "checkout_sig", "message": "Payment failed" },
        ]});
        let out = filter_buckets(v, Some("checkout"));
        let arr = out["items"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["bucketId"], "bkt_cart");
    }
}
