//! `reproit init --learn --report`: what a service serves, read from source,
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
    for service in services.iter().take(MAX_SERVICES) {
        report_service(ctx, root, service);
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
            if backend_detect::detect_backend_framework(&path).is_some() {
                vec![path]
            } else {
                Vec::new()
            }
        }
        drift::SourceRoot::Ambiguous(names) => names.iter().map(|name| root.join(name)).collect(),
    }
}

/// One service: what it serves, and where that disagrees with a schema it
/// already declares.
fn report_service(ctx: &Ctx, root: &Path, service: &Path) {
    let label = service
        .strip_prefix(root)
        .ok()
        .filter(|rest| !rest.as_os_str().is_empty())
        .map(|rest| rest.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    let Some(framework) = backend_detect::detect_backend_framework(service) else {
        return;
    };
    let Some(derived) = extract::derive(service, framework.name) else {
        ctx.say(format!(
            "{label}: detected {}, which has no source reader yet",
            framework.name
        ));
        return;
    };
    ctx.say(format!(
        "\n{label}  ({}, {} file(s) read{})",
        framework.name,
        derived.files_scanned,
        blind_spot(&derived)
    ));
    if derived.routes.is_empty() {
        ctx.say("  no routes read from source".to_string());
        return;
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
    if schemas.is_empty() {
        ctx.say(
            "  no schema in this service: nothing declares this surface, so nothing tests it"
                .to_string(),
        );
    } else {
        compare_declared(ctx, service, &schemas, framework.name);
    }
}

/// The schemas this service declares. `backend.schemas` in a reproit.yaml when
/// there is one, since a project that splits its contract across files is
/// exactly the case a conventional single-filename lookup misses. Otherwise the
/// conventional file.
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
        .into_iter()
        .collect()
}

/// The blind-spot clause. An absence over an unreadable file is not evidence,
/// and a report that omits the count invites the reader to treat it as one.
fn blind_spot(derived: &extract::Derived) -> String {
    if derived.unreadable == 0 {
        String::new()
    } else {
        format!(
            ", {} UNREADABLE -- absences below are not reliable",
            derived.unreadable
        )
    }
}

fn compare_declared(ctx: &Ctx, service: &Path, schemas: &[PathBuf], framework: &str) {
    let documents: Vec<serde_json::Value> = schemas
        .iter()
        .filter_map(|path| crate::domain::backend::load_service_document(path).ok())
        .collect();
    let declared = drift::declared_routes(&documents);
    if declared.is_empty() {
        return;
    }
    let name = schemas
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(", ");
    let Some(found) = drift::compare(service, framework, &declared, &documents) else {
        ctx.say(format!("  {name}: not checked against source"));
        return;
    };
    if found.is_clean() {
        ctx.say(format!(
            "  {name}: all {} declared operation(s) match a route in source",
            declared.len()
        ));
        return;
    }
    ctx.say(format!("  {name}:"));
    for line in drift::lines(&found) {
        ctx.say(format!("    {line}"));
    }
}
