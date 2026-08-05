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

/// Whether this directory is a backend whose useful init is reading the
/// source: no schema yet, or only artifacts a prior init derived itself.
///
/// An existing `reproit.yaml` disqualifies it even when no conventional schema
/// file is present, because that config may declare schemas under other names.
/// Deriving over a project that already has a contract would replace a real one
/// with a draft, which is the opposite of helpful.
///
/// `--force` re-enters derivation over reproit's OWN outputs (the config, and
/// a schema marked `x-reproit-derived`), so a rerun with a corrected `--exec`
/// is not a silent no-op. A hand-written schema is a real contract; force
/// never rederives over it.
///
/// A UI project is left alone: the web and mobile workflows own those, and a
/// repo holding both must not have its frontend init hijacked.
pub(crate) fn needs_derivation(root: &Path, force: bool) -> bool {
    if project_scaffold::backend_detect::detect_backend_framework(root).is_none() {
        return false;
    }
    match project_scaffold::detect_backend_schema(root) {
        Some(schema) => force && is_derived_draft(&schema),
        None => force || !root.join("reproit.yaml").exists(),
    }
}

fn is_derived_draft(schema: &Path) -> bool {
    std::fs::read_to_string(schema).is_ok_and(|content| content.contains("x-reproit-derived: true"))
}

/// Schemas beyond this size are rejected rather than truncated.
const MAX_SCHEMA_BYTES: usize = 8 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) async fn run(
    ctx: &Ctx,
    target: Option<String>,
    platform: Option<String>,
    learn_target: Option<String>,
    exec: Option<String>,
    force: bool,
) -> Result<ExitCode> {
    let root = std::env::current_dir()?;
    let backend_platform = matches!(platform.as_deref(), None | Some("backend"));
    // Deriving from source is what `init` should DO when a backend has no
    // schema, not something to ask for. The dead end here used to tell people
    // to go hand-write an OpenAPI document while the reader could produce a
    // draft from their code in the time the message took to print, and it did
    // not even mention the flag that does it.
    if target.is_none() && backend_platform && needs_derivation(&root, force) {
        return super::backend_learn::run(
            ctx,
            &root,
            learn_target.as_deref(),
            exec.as_deref(),
            force,
        )
        .await;
    }
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
            let (classified, bytes, introspected) = classify_url(&url, backend_only).await?;
            match classified {
                Classified::Schema { snapshot_name } => {
                    ctx.say(format!("  {url} is a service schema"));
                    project_scaffold::init_backend_url(
                        &root,
                        snapshot_name,
                        &bytes,
                        &url_origin(&url)?,
                        force,
                    )?;
                }
                Classified::EmptySchema { kind } => bail!(
                    "{url} parses as {kind} but declares 0 executable operations so far; \
                     point init at the schema that lists your operations (e.g. \
                     /openapi.json), or run bare `reproit init` from the service's \
                     source root to derive one"
                ),
                Classified::Html if backend_only => bail!(
                    "{url} returned an HTML page, not a backend schema. For the web UI workflow \
                     run `reproit init {url} --platform web`; for the backend workflow point at \
                     the schema URL (e.g. /openapi.json)"
                ),
                Classified::Html => init_web_url(ctx, &root, &url, force)?,
                Classified::Ambiguous => {
                    let attempted = if introspected {
                        ", and a GraphQL introspection POST returned no schema either"
                    } else {
                        ""
                    };
                    bail!(
                        "{url} is neither a parseable backend schema (OpenAPI, GraphQL SDL, \
                         GraphQL introspection, protobuf descriptor) nor an HTML \
                         page{attempted}; pass --platform backend or --platform web to say \
                         which workflow you mean"
                    )
                }
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

/// GET the init URL and classify the body. When the GET finds no schema (a
/// fetch error, or an Ambiguous body) and the URL or --platform hints GraphQL,
/// retry as an introspection POST: live GraphQL endpoints usually answer GET
/// with an error status or a playground page. The bool reports whether an
/// introspection POST was attempted, so bail messages can say so.
async fn classify_url(url: &str, backend_only: bool) -> Result<(Classified, Vec<u8>, bool)> {
    let fetched = match fetch(url).await {
        Ok(fetched) => fetched,
        Err(error) => {
            if !should_try_introspection(url, backend_only) {
                return Err(error);
            }
            return match introspect_schema(url).await {
                Some((classified, bytes)) => Ok((classified, bytes, true)),
                None => Err(error.context(
                    "a GraphQL introspection POST was also attempted and returned no schema",
                )),
            };
        }
    };
    let classified = classify_fetched(&fetched.content_type, &fetched.bytes);
    if classified == Classified::Ambiguous && should_try_introspection(url, backend_only) {
        if let Some((classified, bytes)) = introspect_schema(url).await {
            return Ok((classified, bytes, true));
        }
        return Ok((Classified::Ambiguous, fetched.bytes, true));
    }
    Ok((classified, fetched.bytes, false))
}

/// Whether a schemaless GET result warrants a GraphQL introspection POST:
/// only when the URL names a graphql path segment or the user explicitly
/// asked for the backend workflow. Anything else stays a single GET.
fn should_try_introspection(url: &str, backend_only: bool) -> bool {
    if backend_only {
        return true;
    }
    url.parse::<reqwest::Url>().is_ok_and(|parsed| {
        parsed
            .path_segments()
            .into_iter()
            .flatten()
            .any(|segment| segment.eq_ignore_ascii_case("graphql"))
    })
}

/// POST the standard introspection query; Some only when the response body
/// classifies as a schema (empty schemas included, so their honest bail with
/// the 0-operations message still fires).
async fn introspect_schema(url: &str) -> Option<(Classified, Vec<u8>)> {
    let fetched = introspect(url).await.ok()?;
    let classified = classify_fetched(&fetched.content_type, &fetched.bytes);
    matches!(
        classified,
        Classified::Schema { .. } | Classified::EmptySchema { .. }
    )
    .then_some((classified, fetched.bytes))
}

/// Bounded fetch of an init URL: capped size, capped time, limited redirects.
async fn fetch(url: &str) -> Result<Fetched> {
    let response = client()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()
        .with_context(|| format!("fetching {url}"))?;
    read_bounded(url, response).await
}

/// Bounded introspection POST, with the same caps as `fetch`.
async fn introspect(url: &str) -> Result<Fetched> {
    let response = client()?
        .post(url)
        .json(&serde_json::json!({ "query": INTROSPECTION_QUERY }))
        .send()
        .await
        .with_context(|| format!("introspecting {url}"))?
        .error_for_status()
        .with_context(|| format!("introspecting {url}"))?;
    read_bounded(url, response).await
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?)
}

async fn read_bounded(url: &str, mut response: reqwest::Response) -> Result<Fetched> {
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

/// The standard introspection document, shaped to the fields the schema
/// importer reads (roots, fields, args, input fields, enum and union members).
const INTROSPECTION_QUERY: &str = "\
query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types {
      kind
      name
      fields(includeDeprecated: true) {
        name
        args { name type { ...TypeRef } }
        type { ...TypeRef }
      }
      inputFields { name type { ...TypeRef } }
      enumValues(includeDeprecated: true) { name }
      possibleTypes { name }
    }
  }
}
fragment TypeRef on __Type {
  kind
  name
  ofType { kind name ofType { kind name ofType { kind name ofType {
    kind name ofType { kind name ofType { kind name ofType { kind name } } }
  } } } }
}";

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
    // A served schema.graphql: GraphQL SDL is neither JSON nor an HTML page,
    // so it lands here. The parser accepts an empty document, so require at
    // least one type definition before believing the body is SDL at all.
    if let Some(document) = std::str::from_utf8(bytes)
        .ok()
        .and_then(|raw| backend::graphql_sdl_document(raw).ok())
    {
        let has_types = document
            .pointer("/data/__schema/types")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|types| !types.is_empty());
        if has_types {
            return if backend::import_service_schema(&document).is_empty() {
                Classified::EmptySchema {
                    kind: "GraphQL SDL",
                }
            } else {
                Classified::Schema {
                    snapshot_name: "schema.graphql",
                }
            };
        }
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
    fn sdl_bodies_route_to_backend_with_empty_schema_honesty() {
        let sdl = b"type Query {\n  order(id: ID!): String\n}\n";
        assert_eq!(
            classify_fetched("text/plain", sdl),
            Classified::Schema {
                snapshot_name: "schema.graphql"
            }
        );
        // Parseable SDL with types but no executable root operations.
        assert_eq!(
            classify_fetched("text/plain", b"scalar DateTime\n"),
            Classified::EmptySchema {
                kind: "GraphQL SDL"
            }
        );
        // An empty body parses as an empty SDL document; that is not evidence.
        assert_eq!(classify_fetched("text/plain", b""), Classified::Ambiguous);
        assert_eq!(
            classify_fetched("text/plain", b"just some prose, not a schema"),
            Classified::Ambiguous
        );
    }

    #[test]
    fn introspection_attempt_needs_a_graphql_hint() {
        assert!(should_try_introspection("http://api.local/graphql", false));
        assert!(should_try_introspection(
            "http://api.local/api/GraphQL",
            false
        ));
        assert!(should_try_introspection("http://api.local/graphql/", false));
        // --platform backend is an explicit hint on its own.
        assert!(should_try_introspection("http://api.local/orders", true));
        assert!(!should_try_introspection("http://api.local/orders", false));
        // A graphql query parameter or fragment is not a path segment.
        assert!(!should_try_introspection(
            "http://api.local/docs?tab=graphql",
            false
        ));
        assert!(!should_try_introspection("not a url", false));
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
