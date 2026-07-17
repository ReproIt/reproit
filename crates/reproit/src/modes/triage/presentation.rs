use super::*;

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

/// A one-line discriminator summary like `locale=tr (100% of cohort, 8.3x
/// baseline)`.
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

/// `bugs`: the IMPACT-RANKED bug list, each with its content-addressed
/// `bucketId` -- the id the rest of the loop (`pull`/`triage`/`timeline`) keys
/// off. GETs `/v1/apps/:app/buckets` (already impact-sorted server-side). This
/// is the entry point the agent loop starts from: it's the ONLY place the
/// `bkt_...` id is surfaced. Distinct from `find` (the cohort "who's affected"
/// lens over `/v1/errors/:app/cohorts`, which carries sig/count/who but no
/// bucket id).
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
        let bug_id = it["bugId"].as_str().unwrap_or("?");
        let count = it["count"].as_u64().unwrap_or(0);
        let score = it["impact"]["score"].as_f64().unwrap_or(0.0);
        let severity = it["impact"]["severity"].as_str().unwrap_or("?");
        let resolution = it["resolution"]["status"].as_str().unwrap_or("?");
        // One tight, agent-readable row: the id (the loop key) leads, then the
        // ranking signals, then the message.
        println!("\n  [{id}]  impact {score:.2} ({severity})  resolution {resolution}  x{count}");
        println!("    structural bug: {bug_id}");
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
    println!("\nReproduce a bucket: reproit <bkt_...>");
    Ok(())
}

/// Resolve the current top bucket id from the impact-ranked bucket list. This
/// is intentionally small and shares the same server ordering as `cloud
/// buckets`.
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
    let top = items
        .first()
        .context("no bugs available yet; run `reproit bugs` after production data arrives")?;
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
            .with_context(|| format!("no bucket `{bucket}` in app `{app}`; run `reproit bugs`"))?,
        (None, Some(sig)) => list
            .iter()
            .find(|b| b["crashSig"].as_str() == Some(sig))
            .with_context(|| {
                format!("no bucket with crash signature `{sig}`; run `reproit bugs`")
            })?,
        (None, None) => list
            .first()
            .with_context(|| format!("no buckets available for `{app}`; run `reproit bugs`"))?,
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

    println!("\nReproduce: reproit {bucket}");
    Ok(())
}
