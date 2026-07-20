//! Coordination for human-authored bug creation.

use super::capture::upload_original;
use super::record::{exploratory_create_session, human_create_session};
use crate::interface::cli::context::Ctx;
use anyhow::Result;
use std::path::PathBuf;
use std::process::ExitCode;

pub(super) struct CreateArgs {
    pub(super) config_path: Option<PathBuf>,
    pub(super) cloud_tester: bool,
    pub(super) attach: bool,
    pub(super) title: Option<String>,
    pub(super) actions_file: Option<PathBuf>,
    pub(super) record_video: bool,
    pub(super) push: bool,
    pub(super) no_open: bool,
    pub(super) app: Option<String>,
    pub(super) timeout_seconds: u64,
    pub(super) kind: Option<String>,
}

pub(super) async fn run(ctx: &Ctx, args: CreateArgs) -> Result<ExitCode> {
    if args.cloud_tester {
        return exploratory_create_session(
            args.config_path.as_deref(),
            args.app.clone(),
            args.timeout_seconds,
            args.kind.as_deref(),
            ctx,
        )
        .await;
    }
    validate_options(&args)?;
    let capture_video = args.record_video || args.actions_file.is_none();
    let capture = human_create_session(
        args.config_path.as_deref(),
        args.attach,
        args.title.as_deref(),
        args.actions_file.as_deref(),
        capture_video,
        ctx,
    )
    .await?;
    if args.push {
        upload_original(&capture, args.no_open, ctx).await?;
    } else if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "create",
            "status": "captured",
            "capture": capture.id,
            "path": capture.path,
            "verified": false,
            "oracle": null,
            "immutableOriginal": true,
        }));
    } else {
        ctx.say(format!(
            "  local only; push explicitly with `reproit push {}`",
            capture.id
        ));
    }
    Ok(ExitCode::SUCCESS)
}

fn validate_options(args: &CreateArgs) -> Result<()> {
    if args.app.is_some() || args.kind.is_some() || args.timeout_seconds != 1_800 {
        anyhow::bail!("--app, --timeout, and --kind apply only to --cloud-tester");
    }
    Ok(())
}
