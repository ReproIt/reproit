//! `reproit keep` dispatch: hermetic capture guards, Cloud occurrences, and
//! local findings all land through the one command.

use crate::interface::cli::context::Ctx;
use anyhow::{Context, Result};
use std::path::Path;
use std::process::ExitCode;

/// `backend.exec` from the project's reproit.yaml, the repo-local boot recipe
/// for hermetic replay. A capture never supplies a command; only config does.
fn configured_exec(config_path: Option<&Path>) -> Option<String> {
    super::backend_target::find(config_path)
        .ok()
        .flatten()
        .and_then(|project| project.config.exec.clone())
}

pub(super) async fn run(
    ctx: &Ctx,
    config_path: Option<&Path>,
    id: Option<&str>,
    as_name: Option<&str>,
    strict: bool,
    exec: Option<&str>,
    refresh: bool,
) -> Result<ExitCode> {
    if refresh {
        let reference = id.context("keep --refresh needs the guard to re-record")?;
        return super::backend_headless::refresh_capture_guard(ctx, reference).await;
    }
    // A capture file lands as a hermetic guard: proven by re-execution at keep
    // time, replayed by every check. The boot recipe comes from --exec, or
    // from backend.exec in reproit.yaml when the flag is absent, so an
    // initialized project keeps a guard without repeating the command.
    if let Some(reference) = id {
        // A process capsule is a sibling format of the backend capture: same
        // guard directory, same four-way verdict, different trigger. Without
        // this route a capsule could reproduce a failure and never be kept as
        // a test, which breaks find-keep-check for exactly the programs the
        // capsule exists to serve.
        if super::process_capsule::is_process_capsule(Path::new(reference)) {
            let exec = exec.context(
                "keeping a process capsule as a guard needs the command that runs the program: \
                 pass --exec \"<command>\". A capsule may never supply its own command",
            )?;
            return super::process_capsule::keep_capsule_guard(
                ctx,
                Path::new(reference),
                exec,
                as_name,
                strict,
            )
            .await;
        }
        if super::backend_headless::is_capture_file(Path::new(reference)) {
            let resolved = match exec {
                Some(exec) => exec.to_string(),
                None => configured_exec(config_path).context(
                    "keeping a capture as a hermetic guard needs a boot command: pass --exec, or \
                     set backend.exec in reproit.yaml (reproit init records it when it can infer \
                     one)",
                )?,
            };
            return super::backend_headless::keep_capture_guard(
                ctx,
                Path::new(reference),
                &resolved,
                as_name,
                strict,
            )
            .await;
        }
    }
    if exec.is_some() {
        anyhow::bail!(
            "--exec keeps a hermetic capture guard, so the reference must be a \
             reproit-backend-capture file"
        );
    }
    if id.is_some_and(|reference| reference.starts_with("occ_")) {
        return super::bundle::keep_occurrence(
            ctx,
            id.expect("checked occurrence reference"),
            as_name,
            strict,
        )
        .await;
    }
    // The read view accepts backend-only configs too, so a backend finding
    // keeps with the same command as an app one.
    let loaded = super::list::load_read_view(config_path)?;
    super::repro::keep_repro(ctx, &loaded, id, as_name, strict)?;
    Ok(ExitCode::SUCCESS)
}
