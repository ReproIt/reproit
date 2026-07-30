//! Reporting for source-derived backend schemas: what a service serves, read from source,
//! writing nothing.
//!
//! The setup path bails in exactly the situations where a first look is most
//! useful: a repo that already has a schema, and a monorepo holding several
//! services. Both refusals are right for generating a draft, because a draft
//! has to describe one service and must not silently replace a real contract.
//! Neither is right for answering "what does this serve", which is the question
//! someone will let you run on their repo before they will let you write to it.
//!
//! Nothing here creates, modifies or deletes a file.

use super::{drift, extract};
use crate::adapters::project_scaffold::{self, backend_detect};
use crate::interface::cli::context::Ctx;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Bound the services one report walks.
const MAX_SERVICES: usize = 32;

pub(super) fn run(ctx: &Ctx, root: &Path) -> Result<ExitCode> {
    let services = services_under(root);
    if services.is_empty() {
        bail!(
            "no backend framework detected under {} (looked for Cargo.toml, package.json, \
             pyproject/requirements, pom/gradle, Gemfile, composer.json, go.mod)",
            root.display()
        );
    }
    // Truncation is REPORTED. A cap that silently drops services turns "these
    // are your services" into a claim the tool cannot support: a fiber repo of
    // 82 modules and an axum repo of 56 examples both printed exactly 32 and
    // exited 0, with no sign that anything was left out.
    if services.len() >= super::discovery::MAX_SERVICE_SCAN {
        ctx.say(format!(
            "note: at least {} services found, reporting the first {MAX_SERVICES}. Run reproit \
             surface inside a service directory to see the rest.",
            services.len()
        ));
    } else if services.len() > MAX_SERVICES {
        ctx.say(format!(
            "note: {} services found, reporting the first {MAX_SERVICES}. Run reproit \
             surface inside a service directory to see the rest.",
            services.len()
        ));
    }
    let mut reported = Vec::new();
    for service in services.iter().take(MAX_SERVICES) {
        reported.push(report_service(ctx, root, service));
    }
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "services": reported,
            "found": services.len(),
            "foundIsLowerBound": services.len() >= super::discovery::MAX_SERVICE_SCAN,
            "reported": reported.len(),
        }));
    }
    // A report that found nothing is still a report, not a failure. The exit
    // code says the run completed; the text says what was read and what could
    // not be, and those are different facts.
    Ok(ExitCode::SUCCESS)
}

/// Every service under a root, so a monorepo reports each one instead of
/// abstaining. Abstaining is right when one contract is being GENERATED from
/// the union; it is wrong when each service is described separately.
fn services_under(root: &Path) -> Vec<PathBuf> {
    match drift::source_root(root, None) {
        drift::SourceRoot::Scan(path) => {
            if super::discovery::is_service_root(&path) {
                vec![path]
            } else {
                Vec::new()
            }
        }
        drift::SourceRoot::Ambiguous(names) => {
            let mut services: Vec<PathBuf> = names.iter().map(|name| root.join(name)).collect();
            // The root may be a service in its OWN right as well as the parent
            // of others: a Go repo with a root `go.mod` and nested module
            // directories serves both. Reporting only the children silently
            // dropped ~30 applications and 45 routes from one real repo, with
            // the count giving no hint that anything was missing.
            if super::discovery::is_service_root(root) {
                services.insert(0, root.to_path_buf());
            }
            services
        }
    }
}

/// One service: what it serves, and where that disagrees with a schema it
/// already declares. Returns the machine-readable form for `--json`.
fn report_service(ctx: &Ctx, root: &Path, service: &Path) -> serde_json::Value {
    let label = service
        .strip_prefix(root)
        .ok()
        .filter(|rest| !rest.as_os_str().is_empty())
        .map(|rest| rest.display().to_string())
        .unwrap_or_else(|| {
            // The root itself. Its own name reads better than a bare dot.
            service
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".to_string())
        });
    let Some(framework) = backend_detect::detect_backend_framework(service) else {
        return serde_json::json!({ "service": label, "framework": null });
    };
    let Some(derived) = extract::derive(service, framework.name) else {
        ctx.say(format!(
            "{label}: detected {}, which has no source reader yet",
            framework.name
        ));
        return serde_json::json!({ "service": label, "framework": framework.name });
    };
    ctx.say(format!(
        "\n{label}  ({}, {} file(s) read{})",
        framework.name,
        derived.files_scanned,
        blind_spot(&derived)
    ));
    if derived.routes.is_empty() {
        ctx.say("  no routes read from source".to_string());
        let schemas = declared_schemas(service);
        let schema = if schemas.is_empty() {
            schema_absent()
        } else {
            compare_declared(ctx, service, &schemas, framework.name)
        };
        return service_json(&label, framework.name, &derived, schema);
    }
    ctx.say(format!(
        "  {} operation(s) on {} path(s):",
        derived.operation_count(),
        derived.routes.len()
    ));
    for (path, methods) in &derived.routes {
        let verbs: Vec<String> = methods.iter().map(|m| m.to_uppercase()).collect();
        ctx.say(format!("    {:<7} {path}", verbs.join(",")));
    }
    let schemas = declared_schemas(service);
    let schema = if schemas.is_empty() {
        ctx.say(
            "  the schema for this service is still to-write: nothing declares this \
             surface yet (run `reproit init` here to derive a draft from these routes)"
                .to_string(),
        );
        schema_absent()
    } else {
        compare_declared(ctx, service, &schemas, framework.name)
    };
    service_json(&label, framework.name, &derived, schema)
}

/// The machine-readable form of one service, so `--json` carries the same
/// facts as the text: what was read, and what could not be.
fn service_json(
    label: &str,
    framework: &str,
    derived: &extract::Derived,
    schema: serde_json::Value,
) -> serde_json::Value {
    let routes: Vec<serde_json::Value> = derived
        .routes
        .iter()
        .map(|(path, methods)| {
            serde_json::json!({
                "path": path,
                "methods": methods.iter().map(|m| m.to_uppercase()).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "service": label,
        "framework": framework,
        "filesRead": derived.files_scanned,
        "filesUnreadable": derived.unreadable,
        "filesSkippedByLimit": derived.unscanned,
        "operations": derived.operation_count(),
        "routes": routes,
        "schema": schema,
    })
}

fn schema_absent() -> serde_json::Value {
    serde_json::json!({
        "status": "absent",
        "files": [],
        "filesRead": 0,
        "filesUnreadable": 0,
        "filesSkippedByLimit": 0,
    })
}

/// The schemas this service declares. `backend.schemas` in a reproit.yaml when
/// there is one, since a project that splits its contract across files is
/// exactly the case a conventional single-filename lookup misses. Otherwise the
/// conventional file at the service root or in the standard `contrib/`
/// contract directory.
fn declared_schemas(service: &Path) -> Vec<PathBuf> {
    let configured = crate::workflows::backend_target::find(Some(&service.join("reproit.yaml")))
        .ok()
        .flatten()
        .and_then(|project| project.schema_paths().ok())
        .unwrap_or_default();
    if !configured.is_empty() {
        return configured;
    }
    project_scaffold::detect_backend_schema(service)
        .or_else(|| project_scaffold::detect_backend_schema(&service.join("contrib")))
        .into_iter()
        .collect()
}

/// The blind-spot clause. An absence over an unreadable file is not evidence,
/// and a report that omits the count invites the reader to treat it as one.
fn blind_spot(derived: &extract::Derived) -> String {
    let mut parts = Vec::new();
    if derived.unreadable > 0 {
        parts.push(format!("{} UNREADABLE", derived.unreadable));
    }
    if derived.unscanned > 0 {
        parts.push(format!(
            "{} SKIPPED by a size/depth limit",
            derived.unscanned
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(
            ", {} -- absences below are not reliable",
            parts.join(" and ")
        )
    }
}

fn compare_declared(
    ctx: &Ctx,
    service: &Path,
    schemas: &[PathBuf],
    framework: &str,
) -> serde_json::Value {
    let loaded = load_declared_schemas(schemas);
    let declared = drift::declared_routes(&loaded.documents);
    let files: Vec<String> = schemas
        .iter()
        .map(|path| {
            path.strip_prefix(service)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();
    if declared.is_empty() {
        ctx.say(format!(
            "  schema not checked: {}",
            schema_blind_spot(&loaded)
        ));
        return serde_json::json!({
            "status": "notChecked",
            "files": files,
            "reason": "no declared HTTP operations were read",
            "filesRead": loaded.files_read,
            "filesUnreadable": loaded.files_unreadable,
            "filesSkippedByLimit": loaded.files_skipped_by_limit,
        });
    }
    let name = schemas
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(", ");
    let Some(found) = drift::compare(service, framework, &declared, &loaded.documents) else {
        ctx.say(format!("  {name}: not checked against source"));
        return serde_json::json!({
            "status": "notChecked",
            "files": files,
            "declaredOperations": declared.len(),
            "reason": "no routes were read from source",
            "filesRead": loaded.files_read,
            "filesUnreadable": loaded.files_unreadable,
            "filesSkippedByLimit": loaded.files_skipped_by_limit,
        });
    };
    if loaded.files_unreadable > 0 || loaded.files_skipped_by_limit > 0 {
        ctx.say(format!(
            "  schema blind spot: {}",
            schema_blind_spot(&loaded)
        ));
    }
    if found.is_clean() {
        ctx.say(format!(
            "  {name}: all {} declared operation(s) match a route in source",
            declared.len()
        ));
    } else {
        ctx.say(format!("  {name}:"));
        for line in drift::lines(&found) {
            ctx.say(format!("    {line}"));
        }
    }
    schema_json(files, declared.len(), &found, &loaded)
}

#[derive(Default)]
struct LoadedSchemas {
    documents: Vec<serde_json::Value>,
    files_read: usize,
    files_unreadable: usize,
    files_skipped_by_limit: usize,
}

fn load_declared_schemas(schemas: &[PathBuf]) -> LoadedSchemas {
    let mut loaded = LoadedSchemas::default();
    for path in schemas {
        let Ok(metadata) = std::fs::metadata(path) else {
            loaded.files_unreadable += 1;
            continue;
        };
        if metadata.len() > crate::domain::backend::MAX_SCHEMA_FILE_BYTES {
            loaded.files_skipped_by_limit += 1;
            continue;
        }
        match crate::domain::backend::load_service_document(path) {
            Ok(document) => {
                loaded.documents.push(document);
                loaded.files_read += 1;
            }
            Err(_) => loaded.files_unreadable += 1,
        }
    }
    loaded
}

fn schema_blind_spot(loaded: &LoadedSchemas) -> String {
    format!(
        "{} file(s) read, {} unreadable, {} skipped by the schema size limit",
        loaded.files_read, loaded.files_unreadable, loaded.files_skipped_by_limit
    )
}

fn schema_json(
    files: Vec<String>,
    declared: usize,
    found: &drift::Drift,
    loaded: &LoadedSchemas,
) -> serde_json::Value {
    let routes = |items: &[drift::Route]| {
        items
            .iter()
            .map(|(method, path)| serde_json::json!({"method": method, "path": path}))
            .collect::<Vec<_>>()
    };
    let fields: Vec<serde_json::Value> = found
        .field_mismatches
        .iter()
        .map(|mismatch| {
            serde_json::json!({
                "method": mismatch.operation.0,
                "path": mismatch.operation.1,
                "field": mismatch.field,
                "detail": mismatch.detail,
            })
        })
        .collect();
    serde_json::json!({
        "status": if found.is_clean() { "matched" } else { "drift" },
        "files": files,
        "filesRead": loaded.files_read,
        "filesUnreadable": loaded.files_unreadable,
        "filesSkippedByLimit": loaded.files_skipped_by_limit,
        "declaredOperations": declared,
        "matchedOperations": found.matched,
        "sourceFilesRead": found.files_scanned,
        "sourceFilesUnreadable": found.unreadable_sources,
        "typesChecked": found.types_checked,
        "bodiesCompared": found.bodies_compared,
        "declaredButNoRouteMatched": routes(&found.undeclared_by_source),
        "servedButNotDeclared": routes(&found.unserved_by_schema),
        "fieldMismatches": fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_service_report_carries_the_schema_drift_seen_in_text_mode() {
        let root =
            std::env::temp_dir().join(format!("reproit-report-schema-json-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("source root");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"schema-json\"\nversion = \"0.1.0\"\n\
             edition = \"2021\"\n[dependencies]\naxum = \"0.8\"\n",
        )
        .expect("manifest");
        std::fs::write(
            root.join("src/main.rs"),
            "async fn main() {\n\
             \x20   let app = Router::new().route(\"/served\", get(served));\n\
             \x20   axum::serve(listener, app).await.unwrap();\n}\n",
        )
        .expect("entry point");
        std::fs::write(
            root.join("openapi.yaml"),
            "openapi: 3.1.0\ninfo: {title: test, version: \"1\"}\npaths:\n\
             \x20 /declared:\n    get:\n      responses: {\"200\": {description: ok}}\n",
        )
        .expect("schema");

        let report = report_service(&Ctx::default(), &root, &root);
        let schema = report.get("schema").expect("schema report");
        assert_eq!(schema["status"], "drift");
        assert_eq!(
            schema["declaredButNoRouteMatched"],
            serde_json::json!([{"method": "GET", "path": "/declared"}])
        );
        assert_eq!(
            schema["servedButNotDeclared"],
            serde_json::json!([{"method": "GET", "path": "/served"}])
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn large_nested_schema_is_read_and_accounted_separately_from_source_limits() {
        let root = std::env::temp_dir().join(format!(
            "reproit-report-large-schema-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("source root");
        std::fs::create_dir_all(root.join("contrib")).expect("schema root");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"large-schema\"\nversion = \"0.1.0\"\n\
             edition = \"2021\"\n[dependencies]\naxum = \"0.8\"\n",
        )
        .expect("manifest");
        std::fs::write(
            root.join("src/main.rs"),
            "async fn main() {\n\
             \x20   let app = Router::new().route(\"/served\", get(served));\n\
             \x20   axum::serve(listener, app).await.unwrap();\n}\n",
        )
        .expect("entry point");
        std::fs::write(
            root.join("src/generated.rs"),
            " ".repeat(4 * 1024 * 1024 + 1),
        )
        .expect("oversized source");
        let mut schema = String::from(
            "{\"openapi\":\"3.1.0\",\"info\":{\"title\":\"test\",\"version\":\"1\"},\
             \"paths\":{\"/served\":{\"get\":{\"responses\":{\"200\":{\"description\":\"ok\"}}}}}}",
        );
        schema.push_str(&" ".repeat(4 * 1024 * 1024 + 1));
        std::fs::write(root.join("contrib/openapi.json"), schema).expect("large schema");

        let report = report_service(&Ctx::default(), &root, &root);
        assert_eq!(report["filesSkippedByLimit"], 1);
        assert_eq!(report["schema"]["status"], "matched");
        assert_eq!(
            report["schema"]["files"],
            serde_json::json!(["contrib/openapi.json"])
        );
        assert_eq!(report["schema"]["filesRead"], 1);
        assert_eq!(report["schema"]["filesUnreadable"], 0);
        assert_eq!(report["schema"]["filesSkippedByLimit"], 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn service_count_is_not_the_discovery_scan_cap() {
        let root = std::env::temp_dir().join(format!(
            "reproit-report-service-count-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for index in 0..70 {
            let service = root.join(format!("service-{index:02}"));
            std::fs::create_dir_all(service.join("src")).expect("service source root");
            std::fs::write(
                service.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"service-{index:02}\"\nversion = \"0.1.0\"\n\
                     edition = \"2021\"\n[dependencies]\naxum = \"0.8\"\n"
                ),
            )
            .expect("manifest");
            std::fs::write(
                service.join("src/main.rs"),
                "async fn main() {\n\
                 \x20   let app = Router::new().route(\"/health\", get(health));\n\
                 \x20   axum::serve(listener, app).await.unwrap();\n}\n",
            )
            .expect("entry point");
        }

        assert_eq!(services_under(&root).len(), 70);
        let _ = std::fs::remove_dir_all(root);
    }
}
