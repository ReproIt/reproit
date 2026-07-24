//! `reproit init` dispatch: project detection, and URL init routing between
//! the backend schema workflow and the web zero-config workflow.

use crate::adapters::{config, project_scaffold};
use crate::domain::backend;
use crate::interface::cli::context::Ctx;
use crate::interface::cli::target::target_as_url;
use crate::VERSION;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

/// Schemas beyond this size are rejected rather than truncated.
const MAX_SCHEMA_BYTES: usize = 8 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) async fn run(
    ctx: &Ctx,
    target: Option<String>,
    platform: Option<String>,
    force: bool,
) -> Result<ExitCode> {
    let root = std::env::current_dir()?;
    let Some(target) = target else {
        project_scaffold::init(&root, platform.as_deref(), force)?;
        return Ok(ExitCode::SUCCESS);
    };
    let url = target_as_url(&target)
        .ok_or_else(|| anyhow::anyhow!("init target must be a URL, got {target:?}"))?;
    match platform.as_deref() {
        Some("web") => init_web_url(ctx, &root, &url, force)?,
        None | Some("backend") => {
            let backend_only = platform.is_some();
            let fetched = fetch(&url).await?;
            match classify_fetched(&fetched.content_type, &fetched.bytes) {
                Classified::Schema { snapshot_name } => {
                    ctx.say(format!("  {url} is a service schema"));
                    project_scaffold::init_backend_url(
                        &root,
                        snapshot_name,
                        &fetched.bytes,
                        &url_origin(&url)?,
                        force,
                    )?;
                }
                Classified::EmptySchema { kind } => bail!(
                    "{url} parses as {kind} but declares no executable operations; nothing to \
                     scan or fuzz"
                ),
                Classified::Html if backend_only => bail!(
                    "{url} returned an HTML page, not a backend schema. For the web UI workflow \
                     run `reproit init {url} --platform web`; for the backend workflow point at \
                     the schema URL (e.g. /openapi.json)"
                ),
                Classified::Html => init_web_url(ctx, &root, &url, force)?,
                Classified::Ambiguous => bail!(
                    "{url} is neither a parseable backend schema (OpenAPI, GraphQL \
                     introspection, protobuf descriptor) nor an HTML page; pass --platform \
                     backend or --platform web to say which workflow you mean"
                ),
            }
        }
        Some(other) => bail!(
            "a URL initializes the web UI or backend workflow; use --platform web or --platform \
             backend (got {other:?})"
        ),
    }
    Ok(ExitCode::SUCCESS)
}

fn init_web_url(ctx: &Ctx, root: &Path, url: &str, force: bool) -> Result<()> {
    let runner = config::ensure_web_runner_dir(VERSION, &|message| ctx.say(message))?;
    project_scaffold::init_web_url(root, url, &runner, force)
}

struct Fetched {
    content_type: String,
    bytes: Vec<u8>,
}

/// Bounded fetch of an init URL: capped size, capped time, limited redirects.
async fn fetch(url: &str) -> Result<Fetched> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let mut response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()
        .with_context(|| format!("fetching {url}"))?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_SCHEMA_BYTES {
            bail!("{url} exceeded the {MAX_SCHEMA_BYTES} byte schema limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Fetched {
        content_type,
        bytes,
    })
}

#[derive(Debug, PartialEq)]
enum Classified {
    Schema { snapshot_name: &'static str },
    EmptySchema { kind: &'static str },
    Html,
    Ambiguous,
}

/// Decide what an init URL served: a supported backend schema (routed to the
/// backend workflow), an HTML page (routed to the web zero-config workflow),
/// or neither (fail closed and ask for --platform).
fn classify_fetched(content_type: &str, bytes: &[u8]) -> Classified {
    let document = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .or_else(|| serde_yaml::from_slice::<serde_json::Value>(bytes).ok());
    if let Some(document) = document {
        let kind = if document.get("openapi").is_some() || document.get("swagger").is_some() {
            Some((
                "OpenAPI",
                if bytes.trim_ascii_start().starts_with(b"{") {
                    "openapi.json"
                } else {
                    "openapi.yaml"
                },
            ))
        } else if document.pointer("/data/__schema").is_some() || document.get("__schema").is_some()
        {
            Some(("a GraphQL introspection", "schema.graphql.json"))
        } else if document.get("file").is_some() || document.get("files").is_some() {
            Some(("a protobuf descriptor", "descriptor.json"))
        } else {
            None
        };
        if let Some((kind, snapshot_name)) = kind {
            return if backend::import_service_schema(&document).is_empty() {
                Classified::EmptySchema { kind }
            } else {
                Classified::Schema { snapshot_name }
            };
        }
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_lowercase();
    let head = head.trim_start();
    if content_type.contains("text/html")
        || head.starts_with("<!doctype")
        || head.starts_with("<html")
    {
        return Classified::Html;
    }
    Classified::Ambiguous
}

fn url_origin(url: &str) -> Result<String> {
    let parsed = url
        .parse::<reqwest::Url>()
        .with_context(|| format!("invalid init URL {url:?}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("init URL must be absolute HTTP or HTTPS: {url}");
    }
    Ok(parsed.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENAPI_JSON: &str = r#"{"openapi":"3.1.0","paths":{"/orders":{"post":{
        "operationId":"createOrder","responses":{"201":{"description":"created"}}}}}}"#;

    #[test]
    fn url_init_routes_schemas_to_backend_and_html_to_web() {
        assert_eq!(
            classify_fetched("application/json", OPENAPI_JSON.as_bytes()),
            Classified::Schema {
                snapshot_name: "openapi.json"
            }
        );
        let yaml = "openapi: 3.1.0\npaths:\n  /orders:\n    get:\n      operationId: \
                    listOrders\n      responses:\n        \"200\":\n          description: ok\n";
        assert_eq!(
            classify_fetched("text/yaml", yaml.as_bytes()),
            Classified::Schema {
                snapshot_name: "openapi.yaml"
            }
        );
        assert_eq!(
            classify_fetched("text/html; charset=utf-8", b"<!DOCTYPE html><html></html>"),
            Classified::Html
        );
        // Servers that mislabel HTML still route on the body shape.
        assert_eq!(
            classify_fetched(
                "application/octet-stream",
                b"  <html><body>app</body></html>"
            ),
            Classified::Html
        );
        assert_eq!(
            classify_fetched("application/json", br#"{"orders":[]}"#),
            Classified::Ambiguous
        );
        assert_eq!(
            classify_fetched("application/json", br#"{"openapi":"3.1.0","paths":{}}"#),
            Classified::EmptySchema { kind: "OpenAPI" }
        );
    }

    #[test]
    fn url_origin_is_scheme_host_port() {
        assert_eq!(
            url_origin("http://127.0.0.1:8000/openapi.json").unwrap(),
            "http://127.0.0.1:8000"
        );
        assert_eq!(
            url_origin("https://api.example.com/v3/api-docs").unwrap(),
            "https://api.example.com"
        );
        assert!(url_origin("ftp://x/openapi.json").is_err());
    }
}
