//! Coordination for original capture and saved-repro recording.

use super::capture::upload_original;
use super::cloud::cloud_creds;
use super::record::{exploratory_record_session, human_record_session, minimize_record_replay};
use super::repro::resolve_repro_journey;
use crate::backends::orchestrator;
use crate::cli::context::{exit_with, Ctx, Exit};
use crate::modes::{flicker, triage};
use crate::{config, layout};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::ExitCode;

pub(super) struct RecordArgs {
    pub(super) config_path: Option<PathBuf>,
    pub(super) repro: Option<String>,
    pub(super) cloud_tester: bool,
    pub(super) attach: bool,
    pub(super) title: Option<String>,
    pub(super) actions_file: Option<PathBuf>,
    pub(super) no_video: bool,
    pub(super) upload: bool,
    pub(super) no_open: bool,
    pub(super) app: Option<String>,
    pub(super) timeout_seconds: u64,
    pub(super) kind: Option<String>,
    pub(super) devices: usize,
    pub(super) warm: bool,
    pub(super) shots_dir: Option<PathBuf>,
    pub(super) profile: bool,
    pub(super) flicker: bool,
}

pub(super) async fn run(ctx: &Ctx, args: RecordArgs) -> Result<ExitCode> {
    let Some(reference) = args.repro.as_deref() else {
        return record_original(ctx, &args).await;
    };
    validate_saved_repro_options(&args)?;
    record_saved_repro(ctx, &args, reference).await
}

async fn record_original(ctx: &Ctx, args: &RecordArgs) -> Result<ExitCode> {
    if args.cloud_tester {
        return exploratory_record_session(
            args.config_path.as_deref(),
            args.app.clone(),
            args.timeout_seconds,
            args.kind.as_deref(),
            ctx,
        )
        .await;
    }
    validate_original_options(args)?;
    let capture = human_record_session(
        args.config_path.as_deref(),
        args.attach,
        args.title.as_deref(),
        args.actions_file.as_deref(),
        args.no_video,
        ctx,
    )
    .await?;
    if args.upload {
        upload_original(&capture, args.no_open, ctx).await?;
    } else if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "record",
            "status": "captured",
            "capture": capture.id,
            "path": capture.path,
            "verified": false,
            "oracle": null,
            "immutableOriginal": true,
        }));
    } else {
        ctx.say("  local only; upload explicitly with `reproit cap_... --upload`");
    }
    Ok(ExitCode::SUCCESS)
}

fn validate_original_options(args: &RecordArgs) -> Result<()> {
    if args.app.is_some()
        || args.kind.is_some()
        || args.devices != 1
        || args.warm
        || args.shots_dir.is_some()
        || args.profile
        || args.flicker
        || args.timeout_seconds != 1_800
    {
        anyhow::bail!(
            "--app, --timeout, --kind, --devices, --warm, --shots-dir, --profile, and \
             --flicker apply only to --cloud-tester or an existing repro id"
        );
    }
    Ok(())
}

fn validate_saved_repro_options(args: &RecordArgs) -> Result<()> {
    if args.cloud_tester
        || args.attach
        || args.title.is_some()
        || args.actions_file.is_some()
        || args.no_video
        || args.upload
        || args.no_open
    {
        anyhow::bail!("capture options cannot be combined with an existing repro id");
    }
    Ok(())
}

async fn record_saved_repro(ctx: &Ctx, args: &RecordArgs, reference: &str) -> Result<ExitCode> {
    let loaded = config::load(args.config_path.as_deref()).with_context(|| {
        "recording a production bug needs a runnable app configuration. In a source checkout run \
         `reproit init`; for a deployed web app run `reproit init https://app.example.com` in a \
         workspace; from elsewhere pass `--config /path/to/reproit.yaml`"
    })?;
    if reference.starts_with("bkt_")
        && crate::model::repro::resolve(&loaded.root, reference).is_none()
    {
        let (cloud, key) = cloud_creds(None, None);
        triage::pull_global(&loaded.root, reference, reference, ctx.json, cloud, key).await?;
    }
    let journey = resolve_repro_journey(&loaded.root, reference)?;
    let meta = crate::model::repro::resolve(&loaded.root, reference)
        .ok_or_else(|| anyhow::anyhow!("no repro `{reference}` (by id or alias)"))?;
    let replay_path = crate::model::repro::repro_dir(&loaded.root, &meta.id).join("replay.json");
    let mut replay: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&replay_path).map_err(|error| {
            anyhow::anyhow!(
                "reading replay for `{reference}` ({}): {error}",
                replay_path.display()
            )
        })?)?;
    let saved_meta = crate::model::repro::load_meta(&loaded.root, &meta.id);
    if let Some(saved) = saved_meta.as_ref() {
        minimize_record_replay(&mut replay, saved);
    }
    if let Some(oracle) = saved_meta.and_then(|saved| saved.oracle) {
        if let Some(object) = replay.as_object_mut() {
            object.insert("highlight".to_string(), serde_json::Value::String(oracle));
        }
    }
    let cfg_path = layout::fuzz_config_path(&loaded.root);
    std::fs::create_dir_all(cfg_path.parent().expect("fuzz config has a parent"))?;
    std::fs::write(&cfg_path, replay.to_string())?;
    let extra_defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    let outcome = orchestrator::run_journey(
        &loaded.config,
        &loaded.root,
        &journey,
        &orchestrator::RunOpts {
            kind: args.kind.as_deref(),
            devices: args.devices,
            warm: args.warm,
            shots_dir: args.shots_dir.as_deref(),
            profile: args.profile,
            extra_defines: &extra_defines,
            record_video: true,
            ..Default::default()
        },
    )
    .await?;
    if args.flicker {
        let events =
            flicker::analyze_run(&outcome.run_dir, &flicker::FlickerCfg::default()).await?;
        return Ok(if flicker::report(&events) {
            ExitCode::SUCCESS
        } else {
            exit_with(Exit::Regression)
        });
    }
    Ok(if outcome.passed {
        ExitCode::SUCCESS
    } else {
        exit_with(Exit::Regression)
    })
}
