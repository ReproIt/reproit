//! User-facing workflows and their command dispatch.
//!
//! Modules here group by TARGET CLASS (backend, UI-driving, process capsule),
//! not by bug lifecycle. The lifecycle is the axis each module is already
//! organized by internally, so regrouping by it cuts through the largest
//! modules rather than grouping them. `docs/decisions/architecture.md` records the
//! measurement behind that choice.

pub(crate) mod a2ui;
pub(crate) mod accessibility;
pub(crate) mod backend_headless;
pub(crate) mod barrier;
pub(crate) mod bundle;
pub(crate) mod command_capture;
pub(crate) mod deliver;
pub(crate) mod flicker;
pub(crate) mod fuzz;
pub(crate) mod graph;
pub(crate) mod import;
pub(crate) mod journey;
pub(crate) mod mapplan;
pub(crate) mod pwfuzz;
pub(crate) mod screenshots;
pub(crate) mod skills;
pub(crate) mod soak;
pub(crate) mod triage;
pub(crate) mod visual;

mod auth;
mod authored_contract;
mod backend_contracts;
mod backend_learn;
/// The boot command `reproit init` records as `backend.exec`. Exposed so the
/// scaffold writes the same recipe the live-enrichment boot already uses,
/// which is what removes `--exec` from the hermetic replay path.
pub(crate) use backend_learn::inferred_exec as inferred_backend_exec;
mod backend_target;
mod capture;
mod check;
mod checkpoint;
mod cloud;
mod create_command;
mod device;
mod doctor;
mod find_command;
mod fuzz_command;
pub(crate) mod init_command;
mod internal_dispatch;
mod keep_command;
mod list;
mod map;
mod plan_refresh;
mod platforms;
mod process_capsule;
mod proof;
mod record;
mod repro;
mod reset;
mod route_access;
mod scan_command;
mod tui_safety;
#[cfg(test)]
mod verdict_lattice;

use crate::adapters::scoped_env::ScopedEnv;
use crate::adapters::{config, crash_reporter as crashreporter, project_scaffold, update};
use crate::adapters::{orchestrator, platform};
use crate::domain::appmap;
use crate::domain::capsule;
use crate::interface::cli::args::{Cli, CloudAction, Cmd};
use crate::interface::cli::context::{exit_with, Ctx, Exit};
use crate::interface::cli::internal::InternalCmd;
use crate::runtime::{process as exec, project_layout as layout};
use crate::VERSION;
use anyhow::{Context, Result};
use auth::auth_prompt;
use check::CheckArgs;
#[cfg(test)]
use cloud::choose_cloud_project;
use cloud::{cloud_app_id, cloud_cmd, cloud_creds};
#[cfg(test)]
use device::{is_web_engines, run_needs_device_pick};
use doctor::doctor;
use map::rebuild_app_map;
#[cfg(test)]
use record::{minimize_record_replay, web_record_metadata};
use repro::repro_label;
#[cfg(test)]
use repro::{
    build_simplified_replay, find_finding_by_id, parse_fuzz_finding_id, parse_fuzz_oracle,
    parse_fuzz_report, Finding,
};
use std::path::Path;
use std::process::ExitCode;

/// Run the CLI from an explicit argument sequence.
///
/// Keeping argument acquisition outside dispatch makes parsing deterministic
/// and lets command-contract tests avoid mutating process-global arguments.
pub(crate) async fn run_from<I, T>(args: I) -> Result<ExitCode>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    let cli = Cli::parse_args(args);
    let ctx = cli.ctx();
    let updating = matches!(
        &cli.command,
        Cmd::Internal(InternalCmd::Update { .. } | InternalCmd::UpdateCheck)
    );
    if !updating {
        update::notice_and_schedule(VERSION, cli.quiet, cli.json);
    }
    match cli.command {
        Cmd::Init {
            target,
            platform,
            learn_target,
            force,
        } => init_command::run(&ctx, target, platform, learn_target, force).await,
        Cmd::Find(args) => find_command::run(&ctx, cli.config.as_deref(), args).await,
        // `check`: run saved repros and classify each pass/fail/flaky/stale (the
        // four-outcome CI contract). With no name, runs the whole suite and
        // aggregates the worst outcome. Video evidence is an explicit option;
        // baseline diff remains its own operation.
        Cmd::Check {
            repro,
            reference,
            kind,
            runs,
            target,
            record_video,
            flicker,
            update_baseline,
            exec,
        } => {
            // Project-shaped choices (device matrix, locale, repeat count)
            // come from reproit.yaml's `gate:` section, not flags; a missing
            // config keeps the defaults so capture files still check anywhere.
            let gate = config::load(cli.config.as_deref())
                .map(|loaded| loaded.config.gate.clone())
                .unwrap_or_default();
            check::run(
                &ctx,
                cli.config.as_deref(),
                CheckArgs {
                    // The positional form exists for capture files; both spell
                    // the same reference and route through the same resolution.
                    repro: repro.or(reference),
                    devices: gate.devices,
                    kind,
                    // The hidden contract flag wins; otherwise each path
                    // applies its own default (gate.runs under a config,
                    // one run for config-less suites).
                    runs,
                    locale: gate.locale,
                    target,
                    device: gate.device,
                    record_video,
                    flicker,
                    update_baseline,
                    exec,
                },
            )
            .await
        }
        Cmd::Keep {
            id,
            as_name,
            strict,
            exec,
            refresh,
        } => {
            keep_command::run(
                &ctx,
                cli.config.as_deref(),
                id.as_deref(),
                as_name.as_deref(),
                strict,
                exec.as_deref(),
                refresh,
            )
            .await
        }
        Cmd::Doctor => {
            doctor(cli.config.as_deref(), &ctx).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Login { cloud, key } => {
            match cloud_cmd(
                cli.config.as_deref(),
                CloudAction::Login {
                    cloud,
                    key,
                    app: None,
                },
                ctx.json,
                ctx.yes,
            )
            .await
            {
                Ok(()) => Ok(ExitCode::SUCCESS),
                Err(e) => {
                    if ctx.json {
                        ctx.emit(&serde_json::json!({
                            "command": "login",
                            "ok": false,
                            "error": e.to_string(),
                        }));
                    } else {
                        eprintln!("login: {e}");
                    }
                    Ok(exit_with(Exit::Regression))
                }
            }
        }
        Cmd::Internal(cmd) => internal_dispatch::run(&ctx, cli.config, cmd).await,
    }
}

#[cfg(test)]
mod tests;
