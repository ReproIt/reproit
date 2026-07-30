//! `reproit keep` dispatch: hermetic capture guards, Cloud occurrences, and
//! local findings all land through the one command.

use crate::interface::cli::context::Ctx;
use anyhow::Result;
use std::path::Path;
use std::process::ExitCode;

pub(super) async fn run(
    ctx: &Ctx,
    config_path: Option<&Path>,
    id: Option<&str>,
    as_name: Option<&str>,
    strict: bool,
    exec: Option<&str>,
) -> Result<ExitCode> {
    // A capture file with an exec recipe lands as a hermetic guard: proven by
    // re-execution at keep time, replayed by every check.
    if let (Some(reference), Some(exec)) = (id, exec) {
        if super::backend_headless::is_capture_file(Path::new(reference)) {
            return super::backend_headless::keep_capture_guard(
                ctx,
                Path::new(reference),
                exec,
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
