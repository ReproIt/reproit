use super::*;

/// `triage`: READ or SET a bucket's triage status (the management state a
/// dev/agent acts on, distinct from prod-truth resolution).
///
/// With no `status`, GETs `/v1/apps/:app/buckets/:bucket/triage` and renders
/// the current state. With a `status`, POSTs the same endpoint with the body
/// the cloud's `post_triage` expects (`{status, fixedInBuild?, assignee?}`) and
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
        println!("\nSet it with: reproit triage {bucket} fixed --fixed-in-build <ver>");
    } else {
        println!("\nMonitor prod-truth: reproit resolution-events");
    }
    Ok(())
}

/// `resolution-events`: list recent prod-truth TRANSITIONS the background
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

/// `timeline`: the per-bucket OCCURRENCE time-series (segmented by build)
/// plus the computed prod-truth resolution. GETs
/// `/v1/apps/:app/buckets/:bucket/timeline`. The series shows whether
/// occurrences dropped (resolving/resolved) or returned (regressed) after a fix
/// anchor.
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
            "\n`cloud diagnose --run` resolved the bucket. Reproduce it with:\n  reproit {bucket}"
        );
    }
    Ok(())
}

pub(super) fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s).trim()
}
