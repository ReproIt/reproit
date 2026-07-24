//! `reproit init --learn`: derive a draft schema for a backend project that
//! has none. Routes are extracted statically from the framework's source
//! patterns; a resolvable running target optionally enriches parameterless GET
//! routes with one observed response each. The result is an honestly-marked
//! draft `openapi.yaml` plus the standard backend `reproit.yaml`.

use crate::adapters::project_scaffold::{self, backend_detect};
use crate::interface::cli::context::Ctx;
use anyhow::{bail, Result};
use std::path::Path;
use std::process::ExitCode;

mod emit;
mod enrich;
mod extract;

#[cfg(test)]
mod tests;

pub(super) const DRAFT_SCHEMA_NAME: &str = "openapi.yaml";

pub(super) async fn run(
    ctx: &Ctx,
    root: &Path,
    target_flag: Option<&str>,
    force: bool,
) -> Result<ExitCode> {
    let Some(framework) = backend_detect::detect_backend_framework(root) else {
        bail!(
            "--learn could not detect a backend framework from the project manifests \
             (Cargo.toml, package.json, pyproject/requirements, pom/gradle, Gemfile, \
             composer.json, go.mod); run it from the service's root directory"
        );
    };
    if let Some(existing) = project_scaffold::detect_backend_schema(root) {
        if !force {
            bail!(
                "{} already exists; run `reproit init` to use it, or `--force` to overwrite \
                 it with a derived draft",
                existing.strip_prefix(root).unwrap_or(&existing).display()
            );
        }
    }
    let Some(derived) = extract::derive(root, framework.name) else {
        bail!(
            "detected {} (from {}), which --learn cannot extract routes for yet.\n{}",
            framework.name,
            framework.manifest,
            project_scaffold::backend_schema_guide(root)
        );
    };
    if derived.routes.is_empty() {
        bail!(
            "detected {} (from {}) but no routes could be derived from its source \
             ({} files scanned, {} unconfident matches skipped).\n{}",
            framework.name,
            framework.manifest,
            derived.files_scanned,
            derived.skipped,
            project_scaffold::backend_schema_guide(root)
        );
    }
    ctx.say(format!(
        "  derived {} operations on {} paths from {} source ({} files scanned{})",
        derived.operation_count(),
        derived.routes.len(),
        framework.name,
        derived.files_scanned,
        if derived.skipped > 0 {
            format!(", {} unconfident matches skipped", derived.skipped)
        } else {
            String::new()
        }
    ));

    // Live enrichment only when a target is resolvable now: the --target flag
    // or REPROIT_BACKEND_URL (there is no reproit.yaml yet to consult).
    let env = std::env::var("REPROIT_BACKEND_URL").ok();
    let target = super::backend_target::pick_target(target_flag, env.as_deref(), None);
    let mut observations = std::collections::BTreeMap::new();
    let mut target_url = None;
    if let Some((url, source)) = target {
        super::backend_target::validate_target_url(url)?;
        let probe_paths: Vec<String> = derived
            .routes
            .iter()
            .filter(|(path, methods)| methods.contains("get") && !path.contains('{'))
            .map(|(path, _)| path.clone())
            .collect();
        let outcome = enrich::probe(url, &probe_paths).await;
        ctx.say(format!(
            "  probed {} of {} parameterless GET routes at {url} ({source}): {} answered{}",
            outcome.attempted,
            probe_paths.len(),
            outcome.observations.len(),
            if outcome.adapter {
                ", adapter effect trail recorded"
            } else {
                ", no adapter detected (black-box observations)"
            }
        ));
        observations = outcome.observations;
        target_url = Some(url.to_string());
    } else {
        ctx.say(
            "  no running target (pass --target <url> or set REPROIT_BACKEND_URL to also \
             record observed responses)",
        );
    }

    let title = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("backend-service");
    let yaml = emit::draft_yaml(title, framework.name, &derived, &observations)?;
    project_scaffold::init_backend_learned(
        root,
        DRAFT_SCHEMA_NAME,
        &yaml,
        target_url.as_deref(),
        force,
    )?;
    ctx.say(format!(
        "\n  reproit initialized from a DERIVED DRAFT schema ({} routes from source, {} \
         enriched live).",
        derived.routes.len(),
        observations.len()
    ));
    ctx.say(format!(
        "  1. review {DRAFT_SCHEMA_NAME}: it is a draft, not your service's contract"
    ));
    ctx.say("  2. tighten param/body/response types for the routes you rely on");
    ctx.say("  3. reproit doctor         # schema, target, and adapter tier");
    ctx.say("  4. reproit scan           # read-only contract checks");
    ctx.say("     reproit fuzz           # stateful interaction bugs");
    Ok(ExitCode::SUCCESS)
}
