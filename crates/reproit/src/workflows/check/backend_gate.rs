//! `reproit check` for backend projects: the CI gate scan, the kept-guard
//! replay, and the per-service repo aggregate. Split from check.rs at the
//! backend/app boundary so each side stays reviewable.

use super::CheckArgs;
use crate::domain::repro;
use crate::interface::cli::context::{Ctx, Exit};
use crate::workflows::{backend_headless, backend_learn, backend_target};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Replay one backend finding/guard by id, booting the configured backend
/// service first when the project is a backend one and nothing names a live
/// target (the same zero-flag resolution `find` and the gate use).
pub(super) async fn backend_replay_with_boot(
    ctx: &Ctx,
    config_path: Option<&Path>,
    id: &str,
) -> Result<Option<ExitCode>> {
    let backend_reference = repro::raw_finding_id(id)
        .or_else(|| repro::raw_repro_id(id))
        .is_some();
    let mut booted = false;
    if backend_reference {
        if let Some(project) = backend_target::find(config_path)? {
            if let Ok(schemas) = project.schema_paths() {
                booted = backend_target::ensure_live_target(
                    ctx,
                    &project.root,
                    None,
                    project.config.target.as_deref(),
                    &schemas,
                )
                .await?;
            }
        }
    }
    let result = backend_headless::try_replay(ctx, id).await;
    if booted {
        backend_learn::boot::shutdown_process_reset().await;
    }
    result
}

/// Backend CI gate: run a scan with lifecycle-gate exit semantics (block on new
/// or regressed findings only), optional JUnit, and optional baseline
/// recording, then replay every kept guard.
pub(super) async fn run_backend_gate(
    ctx: &Ctx,
    config_path: Option<&Path>,
    args: &CheckArgs,
) -> Result<ExitCode> {
    let root = backend_target::find(config_path)?.map(|project| project.root);
    let Some((schemas, config)) = backend_target::resolve(config_path)? else {
        anyhow::bail!(
            "the backend schema for this check is still to-configure: list your schema \
             file(s) under backend.schemas in reproit.yaml, or run `reproit init` to \
             derive a draft from source"
        );
    };
    let root_path = match &root {
        Some(root) => root.clone(),
        None => std::env::current_dir()?,
    };
    // With nothing naming a live target, boot the service the same way bare
    // `reproit find` does, so the CI wiring keep writes is one command with no
    // target plumbing. The boot also installs the restart-reset used by the
    // guard replays below.
    let booted = backend_target::ensure_live_target(
        ctx,
        &root_path,
        args.target.as_deref(),
        config.target.as_deref(),
        &schemas,
    )
    .await?;
    let result = run_gate_and_guards(ctx, args, &root_path, root, schemas, config).await;
    if booted {
        backend_learn::boot::shutdown_process_reset().await;
    }
    result
}

/// The gate scan (lifecycle exit semantics) followed by a replay of every
/// kept backend guard. The guard replay is what makes `reproit check` the
/// regression test for a KEPT bug: the gate scan is GET-only by design, so a
/// stateful bug can only be re-proven by replaying its saved artifact.
async fn run_gate_and_guards(
    ctx: &Ctx,
    args: &CheckArgs,
    root_path: &Path,
    root: Option<PathBuf>,
    schemas: Vec<PathBuf>,
    config: crate::domain::backend::BackendConfig,
) -> Result<ExitCode> {
    backend_target::apply_target_precedence(args.target.as_deref(), config.target.as_deref())?;
    let mut vars = vec![("REPROIT_GATE".to_string(), "1".to_string())];
    if let Some(junit) = &args.junit {
        vars.push((
            "REPROIT_GATE_JUNIT".to_string(),
            junit.to_string_lossy().into_owned(),
        ));
    }
    if args.update_baseline {
        vars.push(("REPROIT_GATE_BASELINE".to_string(), "1".to_string()));
    }
    let gate = {
        let _env = crate::adapters::scoped_env::ScopedEnv::set(vars);
        backend_headless::run_configured_target(ctx, &schemas, "scan", 1, 1, config, root).await?
    };
    let guards = backend_headless::replay_kept_guards(ctx, root_path).await?;
    Ok(match guards {
        Some(code) if gate == ExitCode::SUCCESS => code,
        _ => gate,
    })
}

/// Gate every named service and aggregate into one exit code.
///
/// A repo with more than one service needed `reproit check` per config plus
/// hand-written `&&` in CI, which loses a failure the moment someone adds a
/// third service and forgets to extend the chain. This runs each in turn,
/// reports a per-service line, and fails if ANY service fails: the aggregate is
/// pessimistic by construction, so a service that could not even be resolved
/// counts as a failure rather than being skipped.
pub(super) async fn run_repo_gate(ctx: &Ctx, args: &CheckArgs) -> Result<ExitCode> {
    let mut failures = Vec::new();
    let mut outcomes = Vec::new();
    for service in &args.service {
        if !service.is_file() {
            failures.push(service.display().to_string());
            outcomes.push((service.clone(), "config not found".to_string()));
            continue;
        }
        ctx.say(format!("=== {} ===", service.display()));
        // Each service resolves its OWN target. `apply_target_precedence`
        // publishes the winner through REPROIT_BACKEND_URL, and env beats
        // config, so without clearing it here service 2 would silently be
        // scanned against service 1's URL and report its schema as violated.
        let outcome = {
            let _scoped = crate::adapters::scoped_env::ScopedEnv::cleared(&["REPROIT_BACKEND_URL"]);
            run_backend_gate(ctx, Some(service), args).await
        };
        let label = match &outcome {
            Ok(code) if *code == ExitCode::SUCCESS => "pass".to_string(),
            Ok(_) => "FAIL".to_string(),
            // A service whose gate could not run at all is a failure, never a
            // skip: an unreachable service must not silently widen the merge.
            Err(error) => format!("ERROR {error}"),
        };
        if label != "pass" {
            failures.push(service.display().to_string());
        }
        outcomes.push((service.clone(), label));
    }
    ctx.say(format!(
        "repo gate: {}/{} service(s) passed",
        outcomes.len() - failures.len(),
        outcomes.len()
    ));
    for (service, label) in &outcomes {
        ctx.say(format!("  {label:<6} {}", service.display()));
    }
    if failures.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(Exit::Regression.code())
    }
}
