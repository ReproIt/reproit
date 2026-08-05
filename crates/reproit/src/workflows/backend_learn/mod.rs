//! `reproit init`: derive a draft schema for a backend project that
//! has none. Routes are extracted statically from the framework's source
//! patterns; a live target enriches them with one observed response per
//! operation, from a request synthesized out of the parsed params (probe_plan
//! decides what may honestly be sent). The target is the --target flag or
//! REPROIT_BACKEND_URL when given, else init resolves one itself (boot.rs):
//! a verified already-running server, or a bounded boot of the package.json
//! start script. The result is an honestly-marked draft `openapi.yaml` plus
//! the standard backend `reproit.yaml`.

use crate::adapters::project_scaffold::{self, backend_detect};
use crate::interface::cli::context::Ctx;
use anyhow::{bail, Result};
use std::path::Path;
use std::process::ExitCode;

pub(super) mod boot;
pub(crate) mod boot_recipe;
pub(crate) use boot_recipe::suggested_exec as inferred_exec;
mod discovery;
mod django_urls;
mod dotnet_ast;
mod dotnet_types;
pub(super) mod drift;
mod emit;
mod enrich;
mod extract;
mod field_facts;
mod go_ast;
mod go_types;
mod grammar;
mod java_ast;
mod java_types;
mod node_ast;
mod node_body;
mod php_ast;
mod probe_plan;
mod python_ast;
mod report;
mod response_facts;
mod route_path;
mod ruby_ast;
mod rust_ast;
mod rust_router;
mod rust_types;

#[cfg(test)]
mod probe_tests;
#[cfg(test)]
mod tests;

pub(super) const DRAFT_SCHEMA_NAME: &str = "openapi.yaml";

/// `reproit surface`: what a backend serves, read from source, writing nothing.
pub(super) fn surface(ctx: &Ctx, root: &Path) -> Result<ExitCode> {
    report::run(ctx, root)
}

pub(super) async fn run(
    ctx: &Ctx,
    root: &Path,
    target_flag: Option<&str>,
    exec_flag: Option<&str>,
    force: bool,
) -> Result<ExitCode> {
    // Deriving one schema from a root that holds several services merges their
    // routes into a contract no single service serves. Same reason the doctor
    // contract check abstains there. Degrade honestly instead of erroring:
    // state what was found, write nothing, and name the exact next input.
    if let drift::SourceRoot::Ambiguous(services) = drift::source_root(root, None) {
        ctx.say(format!(
            "  found {} services under this root: {}.\n  One derived schema would merge \
             routes no single service serves, so nothing was scaffolded.\n  Next: run \
             `reproit init` inside the service you want (e.g. `cd {} && reproit init`)",
            services.len(),
            services.join(", "),
            services[0]
        ));
        return Ok(ExitCode::SUCCESS);
    }
    let Some(framework) = backend_detect::detect_backend_framework(root) else {
        bail!(
            "init could not detect a backend framework from the project manifests \
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
        ctx.say(format!(
            "  detected {} (from {}), which init cannot extract routes for yet.\n{}",
            framework.name,
            framework.manifest,
            project_scaffold::backend_schema_guide(root)
        ));
        return scaffold_empty(ctx, root, force);
    };
    if derived.routes.is_empty() {
        // Naming the unreadable count is the difference between "this service
        // declares no routes" and "the reader could not read it". Reporting
        // only "0 files scanned" hid the reason at the one moment it decides
        // what the user should do next: a TypeScript service read as empty
        // because the wrong grammar rejected every annotated file.
        let mut reasons = Vec::new();
        if derived.unreadable > 0 {
            reasons.push(format!("{} the reader could not parse", derived.unreadable));
        }
        if derived.unscanned > 0 {
            reasons.push(format!(
                "{} excluded by a size or depth limit",
                derived.unscanned
            ));
        }
        let blind = if reasons.is_empty() {
            String::new()
        } else {
            format!(
                ", {} file(s) unread ({}) -- an absence over those is not evidence",
                derived.unreadable + derived.unscanned,
                reasons.join(", ")
            )
        };
        ctx.say(format!(
            "  detected {} (from {}) but no routes could be derived from its source \
             ({} files read, {} unconfident matches skipped{}).\n{}",
            framework.name,
            framework.manifest,
            derived.files_scanned,
            derived.skipped,
            blind,
            project_scaffold::backend_schema_guide(root)
        ));
        return scaffold_empty(ctx, root, force);
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

    // Live enrichment. A --target flag or REPROIT_BACKEND_URL wins; with
    // neither, init resolves a target itself: a verified already-running
    // server, or a bounded build-and-boot of the inferred recipe (torn down
    // after the probe pass on every exit path).
    let env = std::env::var("REPROIT_BACKEND_URL").ok();
    let target = super::backend_target::pick_target(target_flag, env.as_deref(), None);
    // Every parameterless GET route is a verify signal, most distinctive
    // first: `/` answers on nearly any server, so a match on it alone says
    // nothing about whose server it is. Capped at three to bound the scan.
    let mut verify_paths: Vec<String> = derived
        .routes
        .iter()
        .filter(|(path, methods)| methods.contains("get") && !path.contains('{'))
        .map(|(path, _)| path.clone())
        .collect();
    verify_paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
    verify_paths.truncate(3);
    // The boot candidate: a --exec flag is the user's answer and wins; with
    // none, inference reads the manifests, and a tie is said out loud with
    // the exact rerun instead of guessed at.
    let recipe = match exec_flag {
        Some(exec) => Some(boot_recipe::BootRecipe {
            build: None,
            exec: exec.to_string(),
            boot: exec.to_string(),
            evidence: "your --exec flag".to_string(),
        }),
        None => match boot_recipe::infer(root, framework.name) {
            boot_recipe::Inference::Recipe(recipe) => Some(recipe),
            boot_recipe::Inference::Ambiguous { candidates, hint } => {
                ctx.say(format!(
                    "  {hint}; init will not guess which one to boot:\n{}\n  rerun with \
                     the winner, e.g. `reproit init --exec {:?}`",
                    candidates
                        .iter()
                        .map(|candidate| format!("    {candidate}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    candidates[0]
                ));
                None
            }
            boot_recipe::Inference::None => None,
        },
    };
    let mut booted = None;
    // The exec recorded in reproit.yaml, set only after a boot this run
    // proved it serves the derived routes.
    let mut proven_exec = None;
    let resolved = match target {
        Some((url, source)) => {
            super::backend_target::validate_target_url(url)?;
            // A user-named target is worth recording as backend.target.
            Some((url.to_string(), source.to_string(), true))
        }
        None => match boot::auto_target(ctx, root, &verify_paths, recipe.as_ref()).await {
            Some(auto) => {
                // An init-booted server dies with init, so its ephemeral URL
                // must not be recorded as the project's target. A verified
                // already-running server is the user's own and is recorded.
                let record = auto.server.is_none();
                if auto.server.is_some() {
                    proven_exec = recipe.as_ref().map(|recipe| recipe.exec.clone());
                }
                booted = auto.server;
                Some((auto.url, auto.source, record))
            }
            None => None,
        },
    };
    let mut plan = probe_plan::ProbePlan::default();
    let mut observations = std::collections::BTreeMap::new();
    let mut target_url = None;
    if let Some((url, source, record)) = resolved {
        // Mutating probes exist only for a server init booted itself and
        // tears down afterwards; a server that was already running (or one
        // the user named) is theirs, and only safe requests may touch it.
        plan = probe_plan::plan(&derived, booted.is_some(), enrich::MAX_PROBED_ROUTES);
        let outcome = enrich::probe(&url, &plan.probes).await;
        let skipped = if plan.skipped.is_empty() {
            String::new()
        } else {
            let reasons: Vec<String> = plan
                .skipped
                .iter()
                .take(3)
                .map(|skip| {
                    format!(
                        "{} {}: {}",
                        skip.method.to_uppercase(),
                        skip.path,
                        skip.reason
                    )
                })
                .collect();
            let more = plan.skipped.len().saturating_sub(3);
            format!(
                "; {} skipped ({}{})",
                plan.skipped.len(),
                reasons.join("; "),
                if more > 0 {
                    format!("; and {more} more")
                } else {
                    String::new()
                }
            )
        };
        ctx.say(format!(
            "  probed {} of {} derived operations at {url} ({source}): {} answered{}{skipped}",
            outcome.attempted,
            derived.operation_count(),
            outcome.observations.len(),
            if outcome.adapter {
                ", adapter effect trail recorded"
            } else {
                ", no adapter detected (black-box observations)"
            }
        ));
        observations = outcome.observations;
        if record {
            target_url = Some(url);
        }
    } else {
        ctx.say(
            "  no live enrichment this run (start your service, or pass --target <url> or \
             set REPROIT_BACKEND_URL, to also record observed responses)",
        );
    }
    if let Some(server) = booted {
        server.shutdown().await;
    }

    let title = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("backend-service");
    let yaml = emit::draft_yaml(title, framework.name, &derived, &plan, &observations)?;
    project_scaffold::init_backend_learned(
        root,
        DRAFT_SCHEMA_NAME,
        &yaml,
        target_url.as_deref(),
        proven_exec.as_deref(),
        force,
    )?;
    if let Some(exec) = &proven_exec {
        ctx.say(format!(
            "  recorded backend.exec (`{exec}`): this run built, booted, and verified \
             it, so replay needs no --exec flag"
        ));
    }
    // The counts repeat the derivation line's own scheme (operations on
    // paths); a second scheme here ("N routes") misread as a contradiction.
    ctx.say(format!(
        "\n  reproit initialized from a DERIVED DRAFT schema ({} operations on {} paths \
         from source, {} enriched live).",
        derived.operation_count(),
        derived.routes.len(),
        observations.len()
    ));
    ctx.say(format!(
        "  1. review {DRAFT_SCHEMA_NAME}: it is a draft, not your service's contract"
    ));
    ctx.say("  2. tighten param/body/response types for the routes you rely on");
    ctx.say("  3. reproit doctor         # schema, target, and adapter tier");
    ctx.say("  4. reproit find           # find bugs (surface scan, then deep fuzz)");
    Ok(ExitCode::SUCCESS)
}

/// The degrade path: nothing derivable still ends in a usable scaffold. The
/// draft is structurally valid with zero claims, and the caller has already
/// said why derivation came up empty; this names the exact next input.
fn scaffold_empty(ctx: &Ctx, root: &Path, force: bool) -> Result<ExitCode> {
    let yaml = project_scaffold::empty_draft_schema(root);
    project_scaffold::init_backend_learned(root, DRAFT_SCHEMA_NAME, &yaml, None, None, force)?;
    ctx.say("\n  reproit initialized with an EMPTY draft schema (0 routes derived).");
    ctx.say(format!(
        "  1. add the routes your service serves to {DRAFT_SCHEMA_NAME} (paths, methods, \
         params)"
    ));
    ctx.say("  2. reproit find           # find bugs once a target is running");
    Ok(ExitCode::SUCCESS)
}
