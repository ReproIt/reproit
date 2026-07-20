//! Direct original-capture commands and explicit Cloud upload.

use super::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;

pub(super) fn load_original(
    config_path: Option<&Path>,
    id: &str,
) -> Result<record::OriginalCapture> {
    if !valid_capture_id(id) {
        anyhow::bail!("invalid original capture id `{id}`");
    }
    let loaded = config::load(config_path).with_context(|| {
        "capture commands need reproit.yaml; run them in the app checkout or pass --config"
    })?;
    let path = layout::captures_dir(&loaded.root).join(id);
    if !path.join("manifest.json").is_file() {
        anyhow::bail!("no local original capture `{id}`");
    }
    Ok(record::OriginalCapture {
        id: id.to_string(),
        path,
    })
}

pub(super) fn show_original(capture: &record::OriginalCapture, ctx: &Ctx) -> Result<()> {
    let manifest = manifest(capture)?;
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "capture",
            "status": "local",
            "capture": manifest,
            "path": capture.path,
        }));
        return Ok(());
    }
    println!("{}", capture.id);
    println!(
        "  title:    {}",
        manifest
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Captured issue")
    );
    println!(
        "  target:   {} ({})",
        manifest
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        manifest
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!("  original: {}", capture.path.display());
    println!("  status:   local immutable capture");
    Ok(())
}

pub(super) fn watch_original(capture: &record::OriginalCapture) -> Result<()> {
    let video = capture.path.join("original.mov");
    if !video.is_file() {
        anyhow::bail!(
            "capture {} has no video; its structural evidence is in {}",
            capture.id,
            capture.path.display()
        );
    }
    record::open_in_player(&video)
}

pub(super) async fn upload_original(
    capture: &record::OriginalCapture,
    no_open: bool,
    ctx: &Ctx,
) -> Result<String> {
    let (cloud, key) = cloud_creds(None, None);
    let base = cloud.unwrap_or_else(|| "https://cloud.reproit.com".into());
    let base = base.trim_end_matches('/');
    let key = key.ok_or_else(|| anyhow::anyhow!("not signed in; run `reproit login` first"))?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()?;
    let manifest = manifest(capture)?;
    let selected_app =
        std::env::var("REPROIT_CLOUD_APP").ok().or_else(|| {
            crate::adapters::cloud_profile::load_cloud_app(
                &crate::adapters::cloud_profile::token_path(),
            )
        });

    let status_url = format!("{base}/v1/captures/{}", capture.id);
    let mut status = get_status(&client, &status_url, &key).await?;
    if status.as_ref().and_then(capture_status) == Some("complete") {
        let url = capture_url(status.as_ref().expect("status exists"), base, &capture.id)?;
        emit_upload_result(capture, &url, ctx);
        return Ok(url);
    }

    if status.as_ref().and_then(capture_status).is_none()
        || status.as_ref().and_then(capture_status) == Some("pending_review")
    {
        let response = client
            .post(format!("{base}/v1/captures"))
            .bearer_auth(&key)
            .json(&serde_json::json!({
                "id": capture.id,
                "manifest": manifest,
                "appId": selected_app,
            }))
            .send()
            .await
            .context("creating the Cloud capture review")?;
        let response = response_json(response, "create capture review").await?;
        let review = response
            .get("reviewUrl")
            .and_then(Value::as_str)
            .context("Cloud did not return a review URL")?;
        let review = absolute_url(base, review)?;
        if ctx.json {
            ctx.emit(&serde_json::json!({
                "command": "capture",
                "status": "pending_review",
                "capture": capture.id,
                "reviewUrl": review,
            }));
        } else {
            println!("Review this capture before upload:");
            println!("  {review}");
        }
        if !no_open && !open_browser(&review) && !ctx.json {
            println!("Could not open a browser. Open the link above manually.");
        }
        status = Some(wait_for_approval(&client, &status_url, &key, ctx).await?);
    }

    let current = status
        .as_ref()
        .and_then(capture_status)
        .unwrap_or("unknown");
    if !matches!(current, "approved" | "uploading") {
        anyhow::bail!("capture upload cannot continue from Cloud status `{current}`");
    }
    upload_files(&client, base, &key, capture, &manifest, ctx).await?;
    let completed = response_json(
        client
            .post(format!("{status_url}/complete"))
            .bearer_auth(&key)
            .send()
            .await
            .context("finalizing the Cloud capture")?,
        "finalize capture",
    )
    .await?;
    let url = capture_url(&completed, base, &capture.id)?;
    emit_upload_result(capture, &url, ctx);
    Ok(url)
}

pub(super) async fn open_cloud_capture(capture: &record::OriginalCapture, ctx: &Ctx) -> Result<()> {
    let (cloud, key) = cloud_creds(None, None);
    let base = cloud.unwrap_or_else(|| "https://cloud.reproit.com".into());
    let key = key.ok_or_else(|| anyhow::anyhow!("not signed in; run `reproit login` first"))?;
    let client = reqwest::Client::new();
    let status = get_status(
        &client,
        &format!("{}/v1/captures/{}", base.trim_end_matches('/'), capture.id),
        &key,
    )
    .await?
    .context("capture has not been pushed; run `reproit push cap_...`")?;
    if capture_status(&status) != Some("complete") {
        anyhow::bail!("capture upload is not complete");
    }
    let url = capture_url(&status, base.trim_end_matches('/'), &capture.id)?;
    if ctx.json {
        ctx.emit(&serde_json::json!({ "capture": capture.id, "url": url }));
    } else if !open_browser(&url) {
        println!("Open capture: {url}");
    }
    Ok(())
}

async fn wait_for_approval(
    client: &reqwest::Client,
    status_url: &str,
    key: &str,
    ctx: &Ctx,
) -> Result<Value> {
    if !ctx.json {
        println!("Waiting for browser approval...");
    }
    for _ in 0..900 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if let Some(status) = get_status(client, status_url, key).await? {
            match capture_status(&status) {
                Some("approved" | "uploading" | "complete") => return Ok(status),
                Some("pending_review") => continue,
                Some(other) => anyhow::bail!("Cloud changed capture status to `{other}`"),
                None => anyhow::bail!("Cloud returned a capture without status"),
            }
        }
    }
    anyhow::bail!("capture review expired before approval; run the upload command again")
}

async fn upload_files(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    capture: &record::OriginalCapture,
    manifest: &Value,
    ctx: &Ctx,
) -> Result<()> {
    let mut names = manifest
        .get("fileSha256")
        .and_then(Value::as_object)
        .context("capture manifest has no file hashes")?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    names.push("manifest.json".into());
    names.sort();
    for name in names {
        let path = capture.path.join(&name);
        let hash = sha256_file(&path)?;
        if !ctx.json && !ctx.quiet {
            println!("  upload {name}");
        }
        let response = client
            .put(format!("{base}/v1/captures/{}/files/{}", capture.id, name))
            .bearer_auth(key)
            .header("x-reproit-sha256", &hash)
            .header(reqwest::header::CONTENT_TYPE, content_type(&name))
            .body(reqwest::Body::wrap_stream(
                tokio_util::io::ReaderStream::new(
                    tokio::fs::File::open(&path)
                        .await
                        .with_context(|| format!("opening {}", path.display()))?,
                ),
            ))
            .send()
            .await
            .with_context(|| format!("uploading {name}"))?;
        response_json(response, &format!("upload {name}")).await?;
    }
    Ok(())
}

async fn get_status(client: &reqwest::Client, url: &str, key: &str) -> Result<Option<Value>> {
    let response = client
        .get(url)
        .bearer_auth(key)
        .send()
        .await
        .context("reading Cloud capture status")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(response_json(response, "read capture status").await?))
}

async fn response_json(response: reqwest::Response, action: &str) -> Result<Value> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| serde_json::json!({}));
    if !status.is_success() {
        let detail = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or(body.trim());
        anyhow::bail!("Cloud could not {action} ({status}): {detail}");
    }
    Ok(value)
}

fn capture_status(value: &Value) -> Option<&str> {
    value
        .get("capture")
        .and_then(|capture| capture.get("status"))
        .and_then(Value::as_str)
}

fn capture_url(value: &Value, base: &str, id: &str) -> Result<String> {
    if let Some(path) = value.get("captureUrl").and_then(Value::as_str) {
        return absolute_url(base, path);
    }
    absolute_url(base, &format!("/captures/{id}"))
}

fn absolute_url(base: &str, value: &str) -> Result<String> {
    if value.starts_with("http://") || value.starts_with("https://") {
        return Ok(value.to_string());
    }
    Ok(format!("{}{}", base.trim_end_matches('/'), value))
}

fn emit_upload_result(capture: &record::OriginalCapture, url: &str, ctx: &Ctx) {
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "capture",
            "status": "uploaded",
            "capture": capture.id,
            "path": capture.path,
            "url": url,
            "immutableOriginal": true,
        }));
    } else {
        println!("UPLOADED {}", capture.id);
        println!("  {url}");
    }
}

fn manifest(capture: &record::OriginalCapture) -> Result<Value> {
    let path = capture.path.join("manifest.json");
    serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing {}", path.display()))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn content_type(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("json") => "application/json",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

fn open_browser(url: &str) -> bool {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    };
    result.is_ok_and(|status| status.success())
}

fn valid_capture_id(value: &str) -> bool {
    value.strip_prefix("cap_").is_some_and(|suffix| {
        suffix.len() == 16 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_capture_ids_are_strict() {
        assert!(valid_capture_id("cap_0123456789abcdef"));
        assert!(!valid_capture_id("cap_0123"));
        assert!(!valid_capture_id("rep_0123456789abcdef"));
    }

    #[test]
    fn relative_cloud_urls_use_the_logged_in_origin() {
        assert_eq!(
            absolute_url("https://cloud.example/", "/captures/cap_1").unwrap(),
            "https://cloud.example/captures/cap_1"
        );
    }
}
