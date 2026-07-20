//! Scan command coordination across structured, configured, and zero-config targets.

use super::{backend_target, confirm_tui_fuzz};
use crate::adapters::config;
use crate::interface::cli::args::ScanArgs;
use crate::interface::cli::context::{exit_with, Ctx, Exit};
use crate::interface::cli::target::{target_as_executable, target_as_url};
use crate::workflows::map;
use crate::workflows::{a2ui, backend_headless, fuzz};
use crate::VERSION;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(super) async fn run(ctx: &Ctx, config_path: Option<&Path>, args: ScanArgs) -> Result<ExitCode> {
    if let Some(service) = &args.service {
        std::env::set_var("REPROIT_BACKEND_URL", service);
    }
    let configured_backend = if args.target.is_none() {
        backend_target::resolve(config_path)?
    } else {
        None
    };
    if let Some(path) = args.target.as_deref().map(PathBuf::from) {
        if path.is_file() && a2ui::looks_like_target(&path) {
            if args.record_video {
                anyhow::bail!(
                    "A2UI streams produce a minimal JSON reproduction, so \
                     `scan --record-video` does not apply"
                );
            }
            return a2ui::run_target(ctx, &path, "scan", 1, 1);
        }
    }
    set_extra_headers(&args.headers)?;
    if let Some(path) = args.target.as_deref().map(PathBuf::from) {
        if path.is_file() && backend_headless::looks_like_schema(&path) {
            if args.record_video {
                anyhow::bail!(
                    "backend streams produce a structural reproduction, so \
                     `scan --record-video` does not apply"
                );
            }
            return backend_headless::run_target(ctx, &path, "scan", 1, 1).await;
        }
    } else if let Some((path, config)) = configured_backend {
        if args.record_video {
            anyhow::bail!(
                "backend streams produce a structural reproduction, so `scan --record-video` \
                 does not apply"
            );
        }
        return backend_headless::run_configured_target(ctx, &path, "scan", 1, 1, config).await;
    }
    run_app_scan(ctx, config_path, args).await
}

fn set_extra_headers(headers: &[String]) -> Result<()> {
    if headers.is_empty() {
        return Ok(());
    }
    let mut values = serde_json::Map::new();
    for header in headers {
        let Some((name, value)) = header.split_once(':') else {
            anyhow::bail!("invalid --header {header:?}: expected \"Name: value\"");
        };
        let name = name.trim();
        if !name.is_empty() {
            values.insert(name.to_string(), serde_json::Value::from(value.trim()));
        }
    }
    std::env::set_var(
        "REPROIT_EXTRA_HEADERS",
        serde_json::Value::Object(values).to_string(),
    );
    Ok(())
}

async fn run_app_scan(ctx: &Ctx, config_path: Option<&Path>, args: ScanArgs) -> Result<ExitCode> {
    let target_url = args.target.as_deref().and_then(target_as_url);
    let mut synthesized = target_url.is_some();
    let loaded = if let Some(url) = &target_url {
        let runner = config::ensure_web_runner_dir(VERSION, &|message| ctx.say(message))?;
        ctx.say(format!("zero-config web run against {url}"));
        config::synthesize_web(url, &runner, std::env::current_dir()?)?
    } else {
        match config::load(config_path) {
            Ok(loaded) => loaded,
            Err(error) => match args.target.as_deref().and_then(target_as_executable) {
                Some(executable) => {
                    if !confirm_tui_fuzz(ctx, &executable) {
                        return Ok(ExitCode::SUCCESS);
                    }
                    ctx.say(format!("zero-config TUI run against `{executable}`"));
                    let loaded = config::synthesize_tui(&executable, std::env::current_dir()?)?;
                    synthesized = true;
                    loaded
                }
                None => return Err(error),
            },
        }
    };
    let journey = match args.target {
        Some(target) if !synthesized => target,
        _ => "explore".to_string(),
    };
    let freshness = crate::domain::map::map_freshness(&loaded.root)?;
    report_freshness(ctx, &freshness);
    let scan_args = fuzz::ScanArgs {
        journey,
        seed: 1,
        budget: args.budget,
        sim: args.sim,
        json: ctx.json,
        record_video: args.record_video,
        out: args.out,
    };
    let summary = fuzz::scan(&loaded.config, &loaded.root, &scan_args).await?;
    if let Some(replace_map) = map::scan_map_commit(&freshness, summary.complete) {
        let _ = crate::domain::map::commit_run(
            &loaded.root,
            &loaded.config,
            &summary.run_dir,
            replace_map,
            summary.complete,
        )?;
    }
    Ok(if summary.complete && summary.issues == 0 {
        ExitCode::SUCCESS
    } else {
        exit_with(Exit::Regression)
    })
}

fn report_freshness(ctx: &Ctx, freshness: &crate::domain::map::MapFreshness) {
    match freshness {
        crate::domain::map::MapFreshness::Missing => {
            ctx.say("  learning app structure from this scan...");
        }
        crate::domain::map::MapFreshness::Stale(reasons) => {
            ctx.say(format!(
                "  app model changed ({}); refreshing from this scan...",
                reasons.join(", ")
            ));
        }
        crate::domain::map::MapFreshness::Current => {}
    }
}
