//! The finding -> PR delivery pipeline (the "CodeRabbit moment").
//!
//! When fuzz/gate produces a finding, this module turns the run's evidence into
//! a reviewer-ready pull-request comment:
//!
//!   1. `record_repro_clip` annotates the minimized-repro video (sim tier only)
//!      into an MP4 + GIF via the web runner's annotate.mjs tool (caption bars are
//!      rendered with headless Chrome since this ffmpeg has no drawtext).
//!   2. `publish` uploads those artifacts to the cloud evidence endpoint
//!      (POST /v1/errors/:app/:idx/evidence) and returns the served URLs.
//!   3. `comment` builds the PR-comment markdown (summary, suspected file:line,
//!      minimized repro, cohort "who it hits", inline GIF, dashboard link) and
//!      posts it to the GitHub PR (or prints it with --dry-run).
//!
//! The cloud client mirrors modes/triage.rs: base URL/key from flags, then
//! REPROIT_CLOUD_URL / REPROIT_CLOUD_KEY, then https://cloud.reproit.com.

use crate::config::Config;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Resolve a run dir under evidence.outDir: the named one, or the latest.
pub fn resolve_run(cfg: &Config, root: &Path, run: Option<&str>) -> Result<PathBuf> {
    let runs_dir = root.join(&cfg.evidence.out_dir);
    if let Some(name) = run {
        let p = runs_dir.join(name);
        if p.is_dir() {
            return Ok(p);
        }
        anyhow::bail!("no such run dir: {}", p.display());
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&runs_dir)
        .with_context(|| format!("no runs under {} (run a journey first)", runs_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.pop()
        .with_context(|| format!("no runs under {}", runs_dir.display()))
}

/// `reproit publish`: annotate the run's repro clip and upload MP4 + GIF to the
/// finding's cloud evidence. Returns the uploaded evidence records.
#[allow(clippy::too_many_arguments)]
pub async fn publish(
    cfg: &Config,
    root: &Path,
    app: &str,
    idx: usize,
    run: Option<&str>,
    label: Option<String>,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let run_dir = resolve_run(cfg, root, run)?;
    let info = input_from_run(&run_dir)?;
    let bug_label = label.unwrap_or_else(|| {
        let s = info.summary.trim();
        if s.is_empty() {
            "finding".to_string()
        } else {
            s.chars().take(60).collect()
        }
    });
    let action_label = if info.repro.is_empty() {
        "minimized repro".to_string()
    } else {
        info.repro.join(" -> ")
    };

    let web_runner_dir = cfg
        .app
        .web_runner_dir
        .as_ref()
        .map(|d| root.join(d))
        .unwrap_or_else(|| root.join("runners/web"));

    println!("publish: run {}", run_dir.display());
    let clip = record_repro_clip(&run_dir, &web_runner_dir, &bug_label, &action_label).await?;
    let Some((mp4, gif)) = clip else {
        println!(
            "  no repro video in this run (headless tier?). Re-run the finding with \
             `reproit fuzz --confirm-on-sim` (or --sim) to record the minimized repro."
        );
        return Ok(());
    };
    println!("  annotated: {}", mp4.display());
    println!("  gif:       {}", gif.display());

    let c = Cloud::new(cloud, key);
    let stored = c
        .post_evidence(app, idx, &[mp4.clone(), gif.clone()])
        .await?;
    println!("  uploaded {} artifact(s) to {}", stored.len(), c.base());
    for ev in &stored {
        println!(
            "    {} {}",
            ev["kind"].as_str().unwrap_or("?"),
            ev["url"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

/// `reproit comment`: build the PR-comment markdown for a finding and post it to
/// GitHub (or print it with dry_run). Pulls cohort + uploaded-evidence URLs from
/// the cloud, the summary/suspected/repro from the run dir.
#[allow(clippy::too_many_arguments)]
pub async fn comment(
    cfg: &Config,
    root: &Path,
    app: &str,
    idx: usize,
    run: Option<&str>,
    dry_run: bool,
    repo: Option<String>,
    pr: Option<u64>,
    token: Option<String>,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let run_dir = resolve_run(cfg, root, run)?;
    let mut input = input_from_run(&run_dir)?;
    input.confirmed_on_sim = run_dir.join("exceptions.jsonl").exists()
        && !read_findings(&run_dir).is_empty()
        && run_dir.join("manifest.json").exists();

    let c = Cloud::new(cloud, key);
    input.dashboard_url = Some(dashboard_url(c.base(), app, idx));

    // Cohort: best-effort. The finding's production signature is matched by the
    // suspected source / message; we look the sig up from the cloud error list.
    if let Ok(sig) = sig_for_idx(&c, app, idx).await {
        if let Ok((ds, count)) = c.cohort_for_sig(app, &sig).await {
            input.cohort = ds;
            input.cohort_count = count;
        }
    }

    // Uploaded-evidence URLs: GET the finding's evidence list (gif + mp4). The
    // local-fs backend returns cloud-relative `/v1/blob/...` urls; absolutize
    // them against the cloud base so the GitHub-rendered image/link resolves.
    // (R2 deployments return absolute presigned urls, left untouched.)
    let absolutize = |u: &str| -> String {
        if u.starts_with("http://") || u.starts_with("https://") {
            u.to_string()
        } else {
            format!("{}{}", c.base(), u)
        }
    };
    if let Ok(evidence) = c.get_evidence(app, idx).await {
        for ev in evidence {
            let Some(url) = ev["url"].as_str() else {
                continue;
            };
            match ev["kind"].as_str() {
                Some("gif") => input.gif_url = Some(absolutize(url)),
                Some("mp4") => input.video_url = Some(absolutize(url)),
                _ => {}
            }
        }
    }

    let md = render_comment(&input);
    if dry_run {
        println!("--- PR comment (dry-run, NOT posted) ---");
        println!("{md}");
        println!("--- end (dry-run) ---");
        return Ok(());
    }
    let gh = GitHubTarget::resolve(repo, pr, token)?;
    let url = gh.post_comment(&md).await?;
    println!("posted comment to {}#{}: {}", gh.repo, gh.pr, url);
    Ok(())
}

/// Look up the production-error signature at `idx` (so cohort/discriminators can
/// be fetched). Best-effort: errors out only if the cloud is unreachable.
async fn sig_for_idx(c: &Cloud, app: &str, idx: usize) -> Result<String> {
    let errors = c.get(&format!("/v1/errors/{app}")).await?;
    errors["errors"]
        .as_array()
        .and_then(|a| a.get(idx))
        .and_then(|e| e["sig"].as_str())
        .map(String::from)
        .context("no error at idx")
}

// ---- cloud client ---------------------------------------------------------

pub struct Cloud {
    base: String,
    key: Option<String>,
}

impl Cloud {
    pub fn new(cloud: Option<String>, key: Option<String>) -> Self {
        let base = cloud
            .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
            .unwrap_or_else(|| "https://cloud.reproit.com".to_string());
        let key = key.or_else(|| std::env::var("REPROIT_CLOUD_KEY").ok());
        Cloud {
            base: base.trim_end_matches('/').to_string(),
            key,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
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

    /// POST one or more evidence files (multipart) to a finding. Returns the
    /// stored evidence records (each with `kind`, `key`, `url`).
    pub async fn post_evidence(
        &self,
        app: &str,
        idx: usize,
        files: &[PathBuf],
    ) -> Result<Vec<Value>> {
        let mut form = reqwest::multipart::Form::new();
        for path in files {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading evidence file {}", path.display()))?;
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "evidence.bin".to_string());
            let mime = mime_for(path);
            let part = reqwest::multipart::Part::bytes(bytes)
                .file_name(name.clone())
                .mime_str(mime)
                .with_context(|| format!("mime for {name}"))?;
            // The server reads each part's filename/content-type to classify the
            // evidence kind; the field name itself is not significant.
            form = form.part("file", part);
        }
        let url = format!("{}/v1/errors/{app}/{idx}/evidence", self.base);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        let mut req = client.post(&url).multipart(form);
        if let Some(k) = &self.key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("evidence upload {} -> {}: {}", url, status, body.trim());
        }
        let v: Value = serde_json::from_str(&body).with_context(|| format!("parsing {url}"))?;
        Ok(v["evidence"].as_array().cloned().unwrap_or_default())
    }

    /// List a finding's already-uploaded evidence (each with `kind` + `url`).
    pub async fn get_evidence(&self, app: &str, idx: usize) -> Result<Vec<Value>> {
        let v = self
            .get(&format!("/v1/errors/{app}/{idx}/evidence"))
            .await?;
        Ok(v["evidence"].as_array().cloned().unwrap_or_default())
    }

    /// Fetch the cohort discriminators for a signature, if the app has cohort
    /// data. Returns the discriminator list (possibly empty) and the occurrence
    /// count for that signature.
    pub async fn cohort_for_sig(&self, app: &str, sig: &str) -> Result<(Vec<Value>, u64)> {
        let cohorts = self.get(&format!("/v1/errors/{app}/cohorts")).await?;
        let cluster = cohorts["errors"]
            .as_array()
            .and_then(|cs| cs.iter().find(|cl| cl["sig"].as_str() == Some(sig)));
        let Some(cl) = cluster else {
            return Ok((Vec::new(), 0));
        };
        let ds = cl["discriminators"].as_array().cloned().unwrap_or_default();
        let count = cl["count"].as_u64().unwrap_or(0);
        Ok((ds, count))
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4") => "video/mp4",
        Some("gif") => "image/gif",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    }
}

// ---- finding model (parsed from a run dir) --------------------------------

/// The inputs the PR-comment formatter needs, gathered from a run dir plus the
/// uploaded evidence + cohort. Kept as plain data so the formatter is pure and
/// unit-testable without any I/O.
#[derive(Default, Clone)]
pub struct CommentInput {
    /// One-line bug summary (e.g. invariant + kind + first line of message).
    pub summary: String,
    /// Suspected source `file:line`, parsed from the finding's frames/message.
    pub suspected: Option<String>,
    /// The minimized repro: the shrunk action list.
    pub repro: Vec<String>,
    /// Cohort discriminators (from the cloud /cohorts for this sig), if any.
    pub cohort: Vec<Value>,
    /// Occurrence count for this signature in production, if known.
    pub cohort_count: u64,
    /// URL of the inline GIF (uploaded evidence), if available.
    pub gif_url: Option<String>,
    /// URL of the full repro video (uploaded evidence), if available.
    pub video_url: Option<String>,
    /// Dashboard link for the full evidence bundle.
    pub dashboard_url: Option<String>,
    /// Whether the finding was confirmed on the real-runtime (sim) tier.
    pub confirmed_on_sim: bool,
}

/// Read a run dir's fuzz.md + the parsed findings to assemble a CommentInput
/// (everything except the uploaded URLs + cohort, which the caller fills in).
pub fn input_from_run(run_dir: &Path) -> Result<CommentInput> {
    // Findings: exceptions.jsonl (sim) or parsed from the report's repro block.
    // We reuse the same exception parsing the fuzz mode does by reading the run's
    // findings file if present; otherwise fall back to the fuzz.md content.
    let findings = read_findings(run_dir);
    let summary = findings
        .first()
        .map(summary_of_finding)
        .unwrap_or_else(|| "fuzz finding".to_string());
    let suspected = findings.iter().find_map(suspected_of_finding);
    let repro = repro_from_report(run_dir);
    Ok(CommentInput {
        summary,
        suspected,
        repro,
        ..Default::default()
    })
}

/// Best-effort: read findings from a sim run's exceptions.jsonl. Headless runs
/// keep them in the drive log; for those the caller passes findings explicitly
/// or relies on the report's repro block (summary degrades gracefully).
fn read_findings(run_dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(run_dir.join("exceptions.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            !v.get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("TEST FRAMEWORK")
        })
        .collect()
}

fn summary_of_finding(f: &Value) -> String {
    let kind = f.get("kind").and_then(Value::as_str).unwrap_or("EXCEPTION");
    let msg = f
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim();
    if msg.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}: {msg}")
    }
}

/// Pull a `file.ext:line` from a finding's frames or message.
fn suspected_of_finding(f: &Value) -> Option<String> {
    if let Some(frames) = f.get("frames").and_then(Value::as_array) {
        for fr in frames {
            if let Some(s) = fr.as_str().and_then(suspected_source) {
                return Some(s);
            }
        }
    }
    f.get("message")
        .and_then(Value::as_str)
        .and_then(suspected_source)
}

/// Extract `file.ext:line[:col]` -> `file.ext:line` from arbitrary text.
pub fn suspected_source(s: &str) -> Option<String> {
    let re =
        regex::Regex::new(r"([\w./-]+\.(?:dart|kt|swift|ts|tsx|js|rs|py|java|cs)):(\d+)").ok()?;
    re.captures(s).map(|c| format!("{}:{}", &c[1], &c[2]))
}

/// Read the minimized repro action list out of the run's fuzz.md repro block.
fn repro_from_report(run_dir: &Path) -> Vec<String> {
    let md = std::fs::read_to_string(run_dir.join("fuzz.md")).unwrap_or_default();
    let mut in_block = false;
    let mut out = Vec::new();
    for line in md.lines() {
        if line.starts_with("## repro") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim_start().starts_with("```") {
                if out.is_empty() {
                    continue; // opening fence
                }
                break; // closing fence
            }
            let t = line.trim();
            if !t.is_empty() && !t.starts_with("Replay:") {
                out.push(t.to_string());
            }
        }
    }
    out
}

// ---- markdown formatter (pure, unit-tested) -------------------------------

/// Build the PR-comment markdown. Pure: given a fully-populated CommentInput it
/// returns deterministic markdown, so the format is testable without any run dir,
/// cloud, or GitHub access. This is the artifact `--dry-run` prints.
pub fn render_comment(input: &CommentInput) -> String {
    let mut md = String::new();
    md.push_str("## reproit found a bug\n\n");

    // Inline GIF first so the failure is visible above the fold.
    if let Some(gif) = &input.gif_url {
        md.push_str(&format!(
            "![minimized repro]({gif})\n\n*Minimized repro, recorded on the iOS simulator.*\n\n"
        ));
    }

    md.push_str(&format!("**{}**\n\n", input.summary.trim()));

    if let Some(src) = &input.suspected {
        md.push_str(&format!("- **Suspected:** `{src}`\n"));
    }
    if input.confirmed_on_sim {
        md.push_str("- **Confirmed:** reproduced on the real runtime (iOS simulator)\n");
    }
    md.push('\n');

    // Minimized repro as a fenced action list.
    md.push_str(&format!(
        "### Minimized repro ({} action{})\n\n```\n{}\n```\n\n",
        input.repro.len(),
        if input.repro.len() == 1 { "" } else { "s" },
        if input.repro.is_empty() {
            "(no executable actions)".to_string()
        } else {
            input.repro.join("\n")
        }
    ));

    // Cohort: who it hits in production.
    md.push_str("### Who it hits\n\n");
    if input.cohort.is_empty() {
        md.push_str(
            "_No production cohort data for this signature yet (not data-specific, or telemetry not wired)._\n\n",
        );
    } else {
        if input.cohort_count > 0 {
            md.push_str(&format!(
                "Seen **{}x** in production, concentrated in:\n\n",
                input.cohort_count
            ));
        }
        for d in input.cohort.iter().take(3) {
            let key = d["key"].as_str().unwrap_or("?");
            let val = d["value"].as_str().unwrap_or("?");
            let share = d["cohortShare"].as_f64().unwrap_or(0.0) * 100.0;
            let lift = d["lift"].to_string().replace('"', "");
            md.push_str(&format!(
                "- `{key}={val}`: {share:.0}% of affected users ({lift}x baseline)\n"
            ));
        }
        md.push('\n');
    }

    // Links: full video + dashboard.
    let mut links: Vec<String> = Vec::new();
    if let Some(v) = &input.video_url {
        links.push(format!("[full repro video]({v})"));
    }
    if let Some(d) = &input.dashboard_url {
        links.push(format!("[evidence + dashboard]({d})"));
    }
    if !links.is_empty() {
        md.push_str(&format!("{}\n\n", links.join(" · ")));
    }

    md.push_str("<sub>Posted by reproit · deterministic, replayable repro</sub>\n");
    md
}

// ---- artifact generation (sim tier only) ----------------------------------

/// Locate the run's repro .mov: the confirm-on-sim replay records `device-a.mov`.
fn find_repro_video(run_dir: &Path) -> Option<PathBuf> {
    for name in ["device-a.mov", "device-A.mov", "composite.mp4"] {
        let p = run_dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    // Fall back to any .mov/.mp4 in the dir.
    std::fs::read_dir(run_dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
        (ext == "mov" || ext == "mp4").then_some(p)
    })
}

/// Annotate the run's repro video into an MP4 + GIF under <run>/evidence/, via
/// the web runner's annotate.mjs tool. Sim-tier only: headless runs have no video,
/// so this returns None for them. `web_runner_dir` is where annotate.mjs lives.
pub async fn record_repro_clip(
    run_dir: &Path,
    web_runner_dir: &Path,
    bug_label: &str,
    action_label: &str,
) -> Result<Option<(PathBuf, PathBuf)>> {
    let Some(video) = find_repro_video(run_dir) else {
        return Ok(None); // headless (no video) or recording disabled
    };
    let out_dir = run_dir.join("evidence");
    std::fs::create_dir_all(&out_dir)?;
    let script = web_runner_dir.join("annotate.mjs");
    if !script.exists() {
        anyhow::bail!(
            "annotate.mjs not found at {} (set app.webRunnerDir)",
            script.display()
        );
    }
    let status = tokio::process::Command::new("node")
        .arg(&script)
        .arg(&video)
        .arg(&out_dir)
        .arg(bug_label)
        .arg(action_label)
        .status()
        .await
        .context("spawning node annotate.mjs")?;
    if !status.success() {
        anyhow::bail!("annotate.mjs failed (exit {:?})", status.code());
    }
    let mp4 = out_dir.join("repro.mp4");
    let gif = out_dir.join("repro.gif");
    if mp4.exists() && gif.exists() {
        Ok(Some((mp4, gif)))
    } else {
        anyhow::bail!("annotate.mjs did not produce repro.mp4 + repro.gif");
    }
}

// ---- GitHub poster --------------------------------------------------------

/// Resolve the GitHub repo + PR number + token from flags, falling back to the
/// standard GitHub Actions environment.
pub struct GitHubTarget {
    pub repo: String,  // owner/name
    pub pr: u64,       // issue/PR number
    pub token: String, // GITHUB_TOKEN
    pub api: String,   // API base (api.github.com or GHES)
}

impl GitHubTarget {
    /// flags first, then GITHUB_REPOSITORY + the PR number from the event.
    pub fn resolve(repo: Option<String>, pr: Option<u64>, token: Option<String>) -> Result<Self> {
        let repo = repo
            .or_else(|| std::env::var("GITHUB_REPOSITORY").ok())
            .context("no repo: pass --repo owner/name or set GITHUB_REPOSITORY")?;
        let pr = match pr.or_else(pr_from_event) {
            Some(n) => n,
            None => anyhow::bail!(
                "no PR number: pass --pr N or run in a pull_request GitHub Action \
                 (GITHUB_EVENT_PATH with a .pull_request.number)"
            ),
        };
        let token = token
            .or_else(|| std::env::var("GITHUB_TOKEN").ok())
            .or_else(|| std::env::var("GH_TOKEN").ok())
            .context("no token: pass --token or set GITHUB_TOKEN")?;
        let api = std::env::var("GITHUB_API_URL")
            .unwrap_or_else(|_| "https://api.github.com".to_string());
        Ok(GitHubTarget {
            repo,
            pr,
            token,
            api: api.trim_end_matches('/').to_string(),
        })
    }

    /// POST the comment to the PR (issues/:number/comments). Returns the
    /// created comment's html_url.
    pub async fn post_comment(&self, body: &str) -> Result<String> {
        let url = format!(
            "{}/repos/{}/issues/{}/comments",
            self.api, self.repo, self.pr
        );
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        let resp = client
            .post(&url)
            .bearer_auth(&self.token)
            .header("User-Agent", "reproit")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&json!({ "body": body }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("github {} -> {}: {}", url, status, text.trim());
        }
        let v: Value = serde_json::from_str(&text).unwrap_or_default();
        Ok(v["html_url"].as_str().unwrap_or("").to_string())
    }
}

/// The PR number lives in the pull_request event payload at GITHUB_EVENT_PATH.
fn pr_from_event() -> Option<u64> {
    let path = std::env::var("GITHUB_EVENT_PATH").ok()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v["pull_request"]["number"]
        .as_u64()
        .or_else(|| v["number"].as_u64())
}

/// Build a dashboard URL for a finding from the cloud base.
pub fn dashboard_url(cloud_base: &str, app: &str, idx: usize) -> String {
    format!("{cloud_base}/app/{app}/errors/{idx}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discr(key: &str, val: &str, share: f64, lift: f64) -> Value {
        json!({ "key": key, "value": val, "cohortShare": share, "lift": lift })
    }

    #[test]
    fn renders_full_comment_deterministically() {
        let input = CommentInput {
            summary: "EXCEPTION CAUGHT BY WIDGETS LIBRARY: Ticker disposed with active controller"
                .to_string(),
            suspected: Some("lib/main.dart:210".to_string()),
            repro: vec!["tap:Compose".to_string(), "tap:New post".to_string()],
            cohort: vec![
                discr("locale", "tr", 1.0, 2.67),
                discr("plan", "free", 1.0, 2.67),
            ],
            cohort_count: 3,
            gif_url: Some("https://cloud.reproit.com/v1/blob/bugzoo/0/x.gif".to_string()),
            video_url: Some("https://cloud.reproit.com/v1/blob/bugzoo/0/x.mp4".to_string()),
            dashboard_url: Some("https://cloud.reproit.com/app/bugzoo/errors/0".to_string()),
            confirmed_on_sim: true,
        };
        let md = render_comment(&input);
        // Structure: header, inline GIF, summary, suspected, repro, cohort, links.
        assert!(md.starts_with("## reproit found a bug"));
        assert!(md.contains("![minimized repro](https://cloud.reproit.com/v1/blob/bugzoo/0/x.gif)"));
        assert!(md.contains("**EXCEPTION CAUGHT BY WIDGETS LIBRARY: Ticker disposed"));
        assert!(md.contains("**Suspected:** `lib/main.dart:210`"));
        assert!(md.contains("**Confirmed:** reproduced on the real runtime"));
        assert!(md.contains("### Minimized repro (2 actions)"));
        assert!(md.contains("tap:Compose\ntap:New post"));
        assert!(md.contains("Seen **3x** in production"));
        assert!(md.contains("`locale=tr`: 100% of affected users (2.67x baseline)"));
        assert!(md.contains("[full repro video](https://cloud.reproit.com/v1/blob/bugzoo/0/x.mp4)"));
        assert!(
            md.contains("[evidence + dashboard](https://cloud.reproit.com/app/bugzoo/errors/0)")
        );
    }

    #[test]
    fn singular_action_and_no_cohort() {
        let input = CommentInput {
            summary: "PERF: jank 54.5%".to_string(),
            repro: vec!["tap:Animate".to_string()],
            ..Default::default()
        };
        let md = render_comment(&input);
        assert!(md.contains("### Minimized repro (1 action)\n"));
        assert!(md.contains("No production cohort data"));
        // No GIF/links lines when URLs are absent.
        assert!(!md.contains("!["));
        assert!(!md.contains("full repro video"));
    }

    #[test]
    fn empty_repro_is_handled() {
        let input = CommentInput {
            summary: "x".to_string(),
            ..Default::default()
        };
        let md = render_comment(&input);
        assert!(md.contains("(no executable actions)"));
        assert!(md.contains("### Minimized repro (0 actions)"));
    }

    #[test]
    fn suspected_source_pulls_file_line() {
        // The `package:` scheme prefix is stripped (the regex matches the path
        // portion), leaving a usable file:line.
        assert_eq!(
            suspected_source("#0 main (package:bugzoo/main.dart:210:45)"),
            Some("bugzoo/main.dart:210".to_string())
        );
        assert_eq!(
            suspected_source("NullPointer in settings (lib/settings.dart:88)"),
            Some("lib/settings.dart:88".to_string())
        );
        assert_eq!(suspected_source("no source here"), None);
    }

    #[test]
    fn repro_block_parsed_from_report() {
        let dir = std::env::temp_dir().join(format!("reproit-deliver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let md = "# fuzz finding (seed 1)\n\n## findings\n\n- boom\n\n## repro (2 actions, shrunk from 7)\n\n```\ntap:Compose\ntap:New post\n```\n\nReplay: write {\"replay\": [...]}\n";
        std::fs::write(dir.join("fuzz.md"), md).unwrap();
        let got = repro_from_report(&dir);
        assert_eq!(got, vec!["tap:Compose", "tap:New post"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dashboard_url_shape() {
        assert_eq!(
            dashboard_url("https://cloud.reproit.com", "bugzoo", 3),
            "https://cloud.reproit.com/app/bugzoo/errors/3"
        );
    }
}
