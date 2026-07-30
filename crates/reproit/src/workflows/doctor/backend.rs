//! Backend-project doctor: schema parse and operation count, the target
//! resolution plan (same precedence as scan/fuzz, plus what a zero-flag run
//! would boot), the adapter tier probe, and the schema-vs-source contract
//! check. Split from doctor.rs at the backend/app boundary so each side
//! stays reviewable, the same split check.rs uses.

use super::{cloud_checks, doctor_push, finish, DoctorCheck};
use crate::workflows::backend_learn::boot;
use crate::workflows::{backend_target, Ctx};
use anyhow::Result;

/// Backend-project doctor: schema parses (operation count), target resolves
/// (same precedence as scan/fuzz, minus the flag) and answers, and the
/// adapter tier: one read-only traced request decides effect-level vs
/// black-box verdicts, with the one-line adapter mount for the detected
/// framework when the trail is absent.
pub(super) async fn doctor_backend(
    ctx: &Ctx,
    project: &backend_target::BackendProject,
) -> Result<()> {
    use crate::domain::backend;
    let mut checks = Vec::new();
    doctor_push(
        &mut checks,
        "config",
        true,
        true,
        format!("backend project root {}", project.root.display()),
        None,
    );
    let mut document = None;
    // EVERY parsed schema: the drift check compares against the union, so a
    // service split across files does not read its own operations as undeclared.
    let mut documents: Vec<serde_json::Value> = Vec::new();
    match project.schema_paths() {
        Ok(paths) => {
            // Count operations across EVERY declared schema (deduped by id), not
            // just the first, so the reported coverage matches what scan/fuzz run.
            let mut ids = std::collections::BTreeSet::new();
            let mut duplicates = std::collections::BTreeSet::new();
            let mut labels = Vec::new();
            let mut parse_error = None;
            for path in &paths {
                match backend::load_service_document(path) {
                    Ok(parsed) => {
                        for operation in backend::import_service_schema(&parsed) {
                            if !ids.insert(operation.id.clone()) {
                                // Same operationId in more than one schema: the
                                // dedupe keeps the first and drops the rest, so
                                // flag it rather than silently losing coverage.
                                duplicates.insert(operation.id.clone());
                            }
                        }
                        // Show the schema relative to the project root: the root
                        // is now an absolute (canonicalized) path, so the joined
                        // schema paths would otherwise print in full.
                        labels.push(
                            path.strip_prefix(&project.root)
                                .unwrap_or(path.as_path())
                                .display()
                                .to_string(),
                        );
                        if document.is_none() {
                            document = Some(parsed.clone());
                        }
                        documents.push(parsed);
                    }
                    Err(e) => {
                        parse_error = Some(format!("{}: {e:#}", path.display()));
                        break;
                    }
                }
            }
            match parse_error {
                Some(message) => doctor_push(
                    &mut checks,
                    "schema",
                    false,
                    true,
                    message,
                    Some(
                        "the schema must parse as OpenAPI, GraphQL introspection, or a protobuf \
                         descriptor"
                            .into(),
                    ),
                ),
                None => {
                    let operations = ids.len();
                    let label = if paths.len() == 1 {
                        labels.join(", ")
                    } else {
                        format!("{} ({} schemas)", labels.join(", "), paths.len())
                    };
                    doctor_push(
                        &mut checks,
                        "schema",
                        operations > 0,
                        true,
                        format!("{label} ({operations} operation(s))"),
                        Some(
                            "the schema parses but declares 0 operations so far; add the \
                             routes your service serves, or rerun `reproit init` from the \
                             service's source root to derive them"
                                .into(),
                        ),
                    );
                    if !duplicates.is_empty() {
                        doctor_push(
                            &mut checks,
                            "schema-operations",
                            false,
                            false,
                            format!(
                                "operationId in more than one schema, only the first is used: {}",
                                duplicates.iter().cloned().collect::<Vec<_>>().join(", ")
                            ),
                            Some(
                                "give each operation a unique operationId across backend.schemas"
                                    .into(),
                            ),
                        );
                    }
                }
            }
        }
        Err(e) => doctor_push(
            &mut checks,
            "schema",
            false,
            true,
            e.to_string(),
            Some(
                "point backend.schemas at a schema file, or run `reproit init <schema url>`".into(),
            ),
        ),
    }

    if !documents.is_empty() {
        doctor_schema_drift(&mut checks, project, &documents);
    }

    let env = std::env::var("REPROIT_BACKEND_URL").ok();
    let picked =
        backend_target::pick_target(None, env.as_deref(), project.config.target.as_deref())
            .map(|(url, source)| (url.to_string(), source))
            .or_else(|| {
                document
                    .as_ref()
                    .and_then(schema_servers_url)
                    .map(|url| (url, "schema servers entry"))
            });
    match picked {
        Some((url, source)) => {
            let valid = backend_target::validate_target_url(&url);
            let ok = valid.is_ok();
            doctor_push(
                &mut checks,
                "target",
                ok,
                true,
                format!("{url} (from {source})"),
                valid
                    .err()
                    .map(|e| format!("{e:#}; targets are absolute http(s) URLs")),
            );
            if ok {
                adapter_checks(&mut checks, &url, document.as_ref(), &project.root).await;
            }
        }
        None => {
            // Nothing names a target explicitly. That is not a failure by
            // itself: find/check/scan boot the service themselves (or adopt a
            // verified already-running one), so doctor reports the plan the
            // next run will execute. Only when nothing here could produce a
            // live target does the check fail, naming the exact next input.
            let probe = document.as_ref().and_then(parameterless_get_path);
            match boot::auto_target_plan(&project.root, probe.as_deref()).await {
                Some(boot::AutoTargetPlan::Running(port)) => {
                    let url = format!("http://127.0.0.1:{port}");
                    doctor_push(
                        &mut checks,
                        "target",
                        true,
                        true,
                        format!(
                            "no explicit target; a server on port {port} answers {} and \
                             matches this schema, so runs will use it (override with \
                             --target <url>)",
                            probe.as_deref().unwrap_or("/")
                        ),
                        None,
                    );
                    adapter_checks(&mut checks, &url, document.as_ref(), &project.root).await;
                }
                Some(boot::AutoTargetPlan::Boot(script)) => doctor_push(
                    &mut checks,
                    "target",
                    true,
                    true,
                    format!(
                        "no explicit target; `reproit find` boots the package.json \
                         `{script}` script itself and tears it down after the run \
                         (override with --target <url>, REPROIT_BACKEND_URL, or \
                         backend.target)"
                    ),
                    None,
                ),
                None => doctor_push(
                    &mut checks,
                    "target",
                    false,
                    true,
                    "no target named yet, and nothing here to boot one from",
                    Some(
                        "start your service and name it: `reproit find --target <url>` \
                         (or set REPROIT_BACKEND_URL / backend.target, or add a servers \
                         entry to the schema)"
                            .into(),
                    ),
                ),
            }
        }
    }
    cloud_checks(&mut checks, Some("backend"));
    finish(ctx, checks)
}

/// One bounded read-only GET with the scan-time trace headers: reachability,
/// and the adapter tier from the `x-reproit-events` response header.
async fn adapter_checks(
    checks: &mut Vec<DoctorCheck>,
    base_url: &str,
    document: Option<&serde_json::Value>,
    project_root: &std::path::Path,
) {
    let path = document
        .and_then(parameterless_get_path)
        .unwrap_or_default();
    let url = format!("{}{path}", base_url.trim_end_matches('/'));
    let response = probe_traced(&url).await;
    match response {
        Err(e) => doctor_push(
            checks,
            "target reachable",
            false,
            false,
            format!("GET {url}: {e:#}"),
            Some("start the service, or point --target / REPROIT_BACKEND_URL at it".into()),
        ),
        Ok((status, adapter)) => {
            doctor_push(
                checks,
                "target reachable",
                true,
                false,
                format!("GET {url} -> {status}"),
                None,
            );
            if adapter {
                doctor_push(
                    checks,
                    "adapter",
                    true,
                    false,
                    "adapter detected: effect-level verdicts enabled",
                    None,
                );
            } else {
                let snippet =
                    crate::adapters::project_scaffold::backend_detect::detect_backend_framework(
                        project_root,
                    )
                    .map(|found| format!("{} ({})", found.adapter_snippet, found.name))
                    .unwrap_or_else(|| {
                        "mount the ReproIt backend adapter for your framework (see the sdk/ \
                         READMEs)"
                            .into()
                    });
                doctor_push(
                    checks,
                    "adapter",
                    false,
                    false,
                    "no adapter response: black-box tier (response-level checks only)",
                    Some(snippet),
                );
            }
        }
    }
}

/// Check the declared contract against the routes the source actually serves.
///
/// A schema is hand-written far more often than generated, and nothing verified
/// it against the code: a mistyped path 404s on every attempt while still
/// reporting as an exercised operation, and a route missing from the schema is
/// real surface nothing will ever test. `init`'s extractor already reads
/// routes from source, so this points it at validation.
///
/// Reports "not checked" rather than a pass whenever the comparison could not
/// actually run: no recognized framework, no readable source, or a non-OpenAPI
/// schema with no URL routes to compare. An extractor that found nothing must
/// never look like a schema that matched.
fn doctor_schema_drift(
    checks: &mut Vec<DoctorCheck>,
    project: &backend_target::BackendProject,
    documents: &[serde_json::Value],
) {
    use crate::workflows::backend_learn::drift;
    let declared = drift::declared_routes(documents);
    if declared.is_empty() {
        return;
    }
    // Which subtree serves THIS schema. In a repo with several services, scanning
    // the whole root compares one service's schema against a sibling's routes.
    let source = match drift::source_root(&project.root, project.config.source.as_deref()) {
        drift::SourceRoot::Scan(path) => path,
        drift::SourceRoot::Ambiguous(services) => {
            doctor_push(
                checks,
                "contract",
                true,
                false,
                format!(
                    "schema not checked against source: {} services under this root ({}). \
                     Set `backend.source: <dir>` to say which one serves this schema",
                    services.len(),
                    services.join(", ")
                ),
                Some("backend.source scopes init and this check to one service".into()),
            );
            return;
        }
    };
    let Some(framework) =
        crate::adapters::project_scaffold::backend_detect::detect_backend_framework(&source)
    else {
        return;
    };
    let Some(found) = drift::compare(&source, framework.name, &declared, documents) else {
        doctor_push(
            checks,
            "contract",
            true,
            false,
            format!(
                "schema not checked against source: no routes extracted from {} sources in {}",
                framework.name,
                source.display()
            ),
            None,
        );
        return;
    };
    if found.is_clean() {
        doctor_push(
            checks,
            "contract",
            true,
            false,
            format!(
                "all {} declared operation(s) match a route in {} source file(s){}",
                found.matched,
                found.files_scanned,
                // Say exactly which check ran, and over how much. An
                // operation whose handler did not resolve was not compared, and
                // a clean result must not speak for it.
                match (found.types_checked, found.bodies_compared) {
                    (false, _) => " (routes only for this framework)".to_string(),
                    (true, 0) => {
                        " (routes only: no request body could be traced to a handler)".to_string()
                    }
                    (true, compared) => format!(
                        ", and the {compared} request body/bodies traced to a handler agree"
                    ),
                }
            ),
            None,
        );
        return;
    }
    let report = drift::lines(&found);
    doctor_push(
        checks,
        "contract",
        false,
        false,
        format!(
            "schema and source disagree ({} declared operation(s) matched)\n    {}",
            found.matched,
            report.join("\n    ")
        ),
        Some(
            "reproit init rewrites the draft from source. A route the extractor \
             cannot read looks the same as one that does not exist, so confirm before \
             changing a path"
                .into(),
        ),
    );
}

/// Send one GET with `x-reproit-trace` and report (status, adapter present).
/// The body is never read; only the response head matters here.
async fn probe_traced(url: &str) -> Result<(u16, bool)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let response = client
        .get(url)
        .header("x-reproit-trace", "doctor")
        .header("x-reproit-action", "1")
        .send()
        .await?;
    let adapter = response.headers().contains_key("x-reproit-events");
    Ok((response.status().as_u16(), adapter))
}

/// The schema `servers` fallback (first entry, as written; scan/fuzz resolve
/// variables at run time, doctor only reports the address).
fn schema_servers_url(document: &serde_json::Value) -> Option<String> {
    document
        .pointer("/servers/0/url")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// The least intrusive probe: the first OpenAPI GET path with no template
/// parameters, else the service root.
fn parameterless_get_path(document: &serde_json::Value) -> Option<String> {
    document
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .and_then(|paths| {
            paths
                .iter()
                .find(|(path, item)| !path.contains('{') && item.get("get").is_some())
                .map(|(path, _)| path.clone())
        })
}
