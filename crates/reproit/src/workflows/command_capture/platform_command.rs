use crate::interface::cli::context::Ctx;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

pub(crate) struct PlatformCollectArgs {
    pub(crate) project: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) component: Option<String>,
    pub(crate) output: Option<PathBuf>,
    pub(crate) local_only: bool,
}

pub(crate) async fn collect_platform(ctx: &Ctx, args: PlatformCollectArgs) -> Result<ExitCode> {
    let root = std::env::current_dir()?.canonicalize()?;
    let selected_cloud_project = super::super::cloud::cloud_app_id(None).ok();
    let project_id = super::token(
        args.project
            .as_deref()
            .or(selected_cloud_project.as_deref())
            .or_else(|| root.file_name().and_then(|name| name.to_str()))
            .unwrap_or("local-project"),
    );
    let session_id = args
        .session
        .or_else(|| std::env::var("REPROIT_SESSION_ID").ok())
        .context("platform collection needs --session or REPROIT_SESSION_ID")?;
    let component = super::token(args.component.as_deref().unwrap_or("application"));
    let collected = super::platform::collect(&component).await;
    let observed_at = chrono::Utc::now().to_rfc3339();
    let body = serde_json::to_vec(&(
        &project_id,
        &session_id,
        &component,
        &observed_at,
        &collected.evidence,
        &collected.gaps,
    ))?;
    let mut digest = Sha256::new();
    digest.update(&body);
    let batch_id = format!("platform_{}", &super::hex_digest(digest.finalize())[..16]);
    let mut gaps = collected.gaps;
    if collected.evidence.is_empty() && gaps.is_empty() {
        gaps.push("no supported platform metadata surface was detected".into());
    }
    let batch = reproit_protocol::PlatformEvidenceBatch {
        version: reproit_protocol::PLATFORM_EVIDENCE_BATCH_VERSION,
        batch_id,
        project_id,
        session_id,
        emitter_id: format!("platform-{}", std::process::id()),
        observed_at,
        evidence: collected.evidence,
        gaps,
    };
    batch
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid platform evidence: {error}"))?;
    if let Some(path) = &args.output {
        super::write_json(path, &batch)?;
    }
    let uploaded = if args.local_only {
        false
    } else {
        upload_platform_if_configured(&batch).await?
    };
    ctx.emit(&serde_json::json!({
        "command": "platform-collect",
        "batch": &batch,
        "output": &args.output,
        "uploaded": uploaded,
    }));
    ctx.say(format!("Collected platform evidence {}", batch.batch_id));
    ctx.say(format!("  platform identities: {}", batch.evidence.len()));
    ctx.say(format!("  missing capabilities: {}", batch.gaps.len()));
    ctx.say(format!(
        "  Cloud: {}",
        if uploaded { "uploaded" } else { "not uploaded" }
    ));
    Ok(ExitCode::SUCCESS)
}

async fn upload_platform_if_configured(
    batch: &reproit_protocol::PlatformEvidenceBatch,
) -> Result<bool> {
    let (cloud, key) = super::super::cloud::cloud_creds(None, None);
    let Some(key) = key else {
        return Ok(false);
    };
    let cloud = cloud.unwrap_or_else(|| "https://cloud.reproit.com".into());
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("creating Cloud platform collector client")?
        .post(format!(
            "{}/v1/platform-evidence",
            cloud.trim_end_matches('/')
        ))
        .bearer_auth(key)
        .json(batch)
        .send()
        .await
        .with_context(|| format!("uploading platform evidence {}", batch.batch_id))?;
    let status = response.status();
    let value = super::bounded_response_json(response).await?;
    if !status.is_success() {
        let detail = value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Cloud rejected the platform evidence");
        anyhow::bail!("Cloud platform evidence upload failed ({status}): {detail}");
    }
    Ok(true)
}
