//! User-facing workflows and their command dispatch.

pub(crate) mod a2ui;
pub(crate) mod accessibility;
pub(crate) mod analyze;
pub(crate) mod backend_headless;
pub(crate) mod barrier;
pub(crate) mod bundle;
pub(crate) mod command_capture;
pub(crate) mod deliver;
pub(crate) mod fix;
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
mod backend_learn;
mod backend_target;
mod capture;
mod change_selection;
mod check;
mod cloud;
mod create_command;
mod device;
mod doctor;
mod find_command;
mod fuzz_command;
pub(crate) mod init_command;
mod inspect;
mod list;
mod map;
mod platforms;
mod proof;
mod record;
mod repro;
mod reset;
mod route_access;
mod scan_command;
mod tui_safety;

#[cfg(all(target_os = "linux", feature = "linux-atspi"))]
use crate::adapters::atspi;
use crate::adapters::scoped_env::ScopedEnv;
#[cfg(windows)]
use crate::adapters::uia;
use crate::adapters::{config, crash_reporter as crashreporter, project_scaffold, update};
use crate::adapters::{orchestrator, platform, simctl, tui};
use crate::domain::appmap;
use crate::domain::capsule;
use crate::interface::cli::args::{
    AuthAction, AuthStrategyArg, Cli, CloudAction, Cmd, DebugAction, JourneyAction, ListState,
    MapAction, ReproAction, SkillsAction,
};
use crate::interface::cli::context::{exit_with, Ctx, Exit};
use crate::interface::mcp;
use crate::runtime::{process as exec, project_layout as layout};
use crate::VERSION;
use anyhow::{Context, Result};
use auth::{auth_cmd, auth_prompt, discover_and_verify_login, verify_configured_login};
use authored_contract::run_vitest_contract;
use capture::{load_original, open_cloud_capture, show_original, upload_original, watch_original};
use check::CheckArgs;
#[cfg(test)]
use cloud::choose_cloud_project;
use cloud::{cloud_app_id, cloud_cmd, cloud_creds};
use create_command::CreateArgs;
#[cfg(test)]
use device::{is_web_engines, run_needs_device_pick};
use doctor::doctor;
use map::{debug_map, ensure_app_map, rebuild_app_map};
use proof::{list_candidates, show_proof};
#[cfg(test)]
use record::{minimize_record_replay, web_record_metadata};
use record::{open_in_player, resolve_repro_video};
#[cfg(test)]
use repro::{
    build_simplified_replay, find_finding_by_id, parse_fuzz_finding_id, parse_fuzz_oracle,
    parse_fuzz_report, Finding,
};
use repro::{keep_repro, repro_label, simplify_repro};
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
    if !matches!(&cli.command, Cmd::Update { .. } | Cmd::UpdateCheck) {
        update::notice_and_schedule(VERSION, cli.quiet, cli.json);
    }
    match cli.command {
        Cmd::Init {
            target,
            platform,
            learn,
            learn_target,
            force,
        } => init_command::run(&ctx, target, platform, learn, learn_target, force).await,
        Cmd::Find(args) => find_command::run(&ctx, cli.config.as_deref(), args).await,
        Cmd::List { state, query } => match state {
            ListState::Guards => {
                if query.is_some() {
                    anyhow::bail!("--query applies only to `reproit list --state bugs`");
                }
                let loaded = config::load(cli.config.as_deref())?;
                list::guards(&ctx, &loaded, "list")
            }
            ListState::Candidates => {
                if query.is_some() {
                    anyhow::bail!("--query applies only to `reproit list --state bugs`");
                }
                let loaded = config::load(cli.config.as_deref())?;
                list_candidates(&ctx, &loaded)?;
                Ok(ExitCode::SUCCESS)
            }
            ListState::Bugs => {
                let app = cloud_app_id(None)?;
                let (cloud, key) = cloud_creds(None, None);
                triage::buckets(&app, query.as_deref(), ctx.json, cloud, key).await?;
                Ok(ExitCode::SUCCESS)
            }
        },
        Cmd::Surface => backend_learn::surface(&ctx, &std::env::current_dir()?),
        Cmd::Reset {
            all,
            init: initialize,
            platform,
        } => reset::run(
            cli.config.as_deref(),
            &ctx,
            all,
            initialize,
            platform.as_deref(),
        ),
        Cmd::Update { check } => {
            update::run(VERSION, check).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::UpdateCheck => {
            let _ = update::refresh_cache(VERSION).await;
            Ok(ExitCode::SUCCESS)
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
        // Advanced graph diagnostics. Normal workflows call ensure_app_map and
        // never require users or agents to manage this lifecycle explicitly.
        Cmd::Debug {
            action: DebugAction::Map { action },
        } => debug_map(cli.config.as_deref(), action, &ctx).await,
        // Deterministic local re-evaluation of a production backend capture.
        Cmd::Debug {
            action: DebugAction::ReplayCapture { file },
        } => backend_headless::replay_capture(&ctx, &file),
        Cmd::VitestContract {
            cwd,
            test_path,
            test_name,
            pnpm_version,
        } => run_vitest_contract(&ctx, &cwd, &test_path, &test_name, &pnpm_version).await,
        Cmd::Create {
            cloud_tester,
            attach,
            title,
            actions_file,
            record_video,
            push,
            no_open,
            app,
            timeout,
            kind,
        } => {
            create_command::run(
                &ctx,
                CreateArgs {
                    config_path: cli.config,
                    cloud_tester,
                    attach,
                    title,
                    actions_file,
                    record_video,
                    push,
                    no_open,
                    app,
                    timeout_seconds: timeout,
                    kind,
                },
            )
            .await
        }
        Cmd::Push { capture, no_open } => {
            let capture = load_original(cli.config.as_deref(), &capture)?;
            upload_original(&capture, no_open, &ctx).await?;
            Ok(ExitCode::SUCCESS)
        }
        // `baseline`: the visual oracle. Diff the current capture against the
        // committed baseline (per-pixel tolerance + ignore regions); `--update`
        // accepts the current capture as the new baseline.
        Cmd::Baseline { update } => {
            let loaded = config::load(cli.config.as_deref())?;
            let Some(vis) = &loaded.config.visual else {
                anyhow::bail!("no `visual` section in reproit.yaml");
            };
            let ok = visual::diff(vis, &loaded.root, update)?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                exit_with(Exit::Regression)
            })
        }
        // `check`: run saved repros and classify each pass/fail/flaky/stale (the
        // four-outcome CI contract). With no name, runs the whole suite and
        // aggregates the worst outcome. Video evidence is an explicit option;
        // baseline diff remains its own operation.
        Cmd::Check {
            repro,
            reference,
            devices,
            kind,
            runs,
            junit,
            service,
            strict,
            locale,
            target,
            device,
            record_video,
            flicker,
            changed,
            update_baseline,
        } => {
            check::run(
                &ctx,
                cli.config.as_deref(),
                CheckArgs {
                    // The positional form exists for capture files; both spell
                    // the same reference and route through the same resolution.
                    repro: repro.or(reference),
                    devices,
                    kind,
                    runs,
                    junit,
                    service,
                    strict,
                    locale,
                    target,
                    device,
                    record_video,
                    flicker,
                    changed,
                    update_baseline,
                    inspect: false,
                },
            )
            .await
        }
        Cmd::Inspect { reference, offline } => {
            if reference.ends_with(".rpb") {
                if offline {
                    anyhow::bail!("support-bundle inspection is already offline");
                }
                bundle::inspect(&ctx, Path::new(&reference))?;
                Ok(ExitCode::SUCCESS)
            } else {
                inspect::run(&ctx, cli.config.as_deref(), &reference, offline).await
            }
        }
        Cmd::Collect {
            output,
            product,
            component,
            platform,
            summary,
            artifacts,
            exportable,
            retention_class,
        } => {
            let args = bundle::CollectArgs {
                output,
                product,
                component,
                platform,
                summary,
                artifacts,
                exportable,
                retention_class,
            };
            bundle::collect(&ctx, args)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::CaptureCommand {
            bundle,
            project,
            component,
            timeout_ms,
            include_output,
            local_only,
            attach,
            title,
            actions_file,
            record_video,
            push,
            no_open,
            kind,
            command,
        } => {
            if let Some(bundle_path) = bundle {
                if !command.is_empty()
                    || project.is_some()
                    || component.is_some()
                    || include_output
                    || local_only
                    || attach
                    || title.is_some()
                    || actions_file.is_some()
                    || record_video
                    || push
                    || no_open
                    || kind.is_some()
                {
                    anyhow::bail!("--bundle cannot be combined with another capture source");
                }
                bundle::import(&ctx, &bundle_path)?;
                return Ok(ExitCode::SUCCESS);
            }
            if command.is_empty() {
                if project.is_some() || component.is_some() || include_output || local_only {
                    anyhow::bail!(
                        "--project, --component, --include-output, and --local-only require \
                         `reproit capture -- <command>`"
                    );
                }
                return create_command::run(
                    &ctx,
                    CreateArgs {
                        config_path: cli.config,
                        cloud_tester: false,
                        attach,
                        title,
                        actions_file,
                        record_video,
                        push,
                        no_open,
                        app: None,
                        timeout_seconds: 1800,
                        kind,
                    },
                )
                .await;
            }
            if attach
                || title.is_some()
                || actions_file.is_some()
                || record_video
                || push
                || no_open
                || kind.is_some()
            {
                anyhow::bail!(
                    "application capture options cannot be combined with \
                     `reproit capture -- <command>`"
                );
            }
            command_capture::run(
                &ctx,
                command_capture::CommandCaptureArgs {
                    project,
                    component,
                    timeout_ms,
                    include_output,
                    local_only,
                    command,
                },
            )
            .await
        }
        Cmd::Occurrence { reference } => bundle::run_occurrence(&ctx, &reference).await,
        Cmd::Plan {
            occurrence,
            bindings,
            identity,
        } => bundle::compile(&ctx, &occurrence, &bindings, &identity).map(|_| ExitCode::SUCCESS),
        Cmd::Proof { reference } => {
            let loaded = config::load(cli.config.as_deref())?;
            show_proof(&ctx, &loaded, &reference)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Verify {
            ids,
            junit,
            prune_retracted,
        } => {
            backend_headless::backend_verify(
                &ctx,
                cli.config.as_deref(),
                &ids,
                junit.as_deref(),
                prune_retracted,
            )
            .await
        }
        Cmd::Accept {
            ids,
            reason,
            until,
            remove,
            list,
        } => {
            backend_headless::backend_accept(&ctx, &ids, &reason, until.as_deref(), remove, list)
                .await
        }
        Cmd::Candidates => {
            let loaded = config::load(cli.config.as_deref())?;
            list_candidates(&ctx, &loaded)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Keep {
            id,
            as_name,
            strict,
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            keep_repro(&ctx, &loaded, id.as_deref(), as_name.as_deref(), strict)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Repro {
            action: ReproAction::Simplify { repro, to },
        } => simplify_repro(&ctx, cli.config.as_deref(), &repro, &to).await,
        // `repro list` is an alias of the top-level `repros`: one match arm,
        // one implementation, identical output.
        Cmd::Repros
        | Cmd::Repro {
            action: ReproAction::List,
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            list::guards(&ctx, &loaded, "repros")
        }
        Cmd::Bugs { query } => {
            let app = cloud_app_id(None)?;
            let (cloud, key) = cloud_creds(None, None);
            triage::buckets(&app, query.as_deref(), ctx.json, cloud, key).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::ReplayBucket {
            issue,
            as_name,
            no_run,
            record_video,
            flicker,
            cloud,
            key,
        } => {
            let alias = as_name.unwrap_or_else(|| issue.clone());
            let (cloud, key) = cloud_creds(cloud, key);
            let loaded = config::load(cli.config.as_deref()).with_context(|| {
                "replaying a production bug needs a runnable app configuration. In a source \
                 checkout run `reproit init`; for a deployed web app run `reproit init \
                 https://app.example.com` in a workspace; from elsewhere pass \
                 `--config /path/to/reproit.yaml`"
            })?;
            triage::reproduce_bucket(
                &loaded.root,
                None,
                &issue,
                &alias,
                !no_run,
                None,
                record_video,
                flicker,
                ctx.json,
                cloud,
                key,
            )
            .await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::OriginalCapture {
            capture,
            watch,
            open,
        } => {
            let capture = load_original(cli.config.as_deref(), &capture)?;
            if watch {
                watch_original(&capture)?;
            } else if open {
                open_cloud_capture(&capture, &ctx).await?;
            } else {
                show_original(&capture, &ctx)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Triage {
            issue,
            status,
            fixed_in_build,
            assignee,
        } => {
            let (cloud, key) = cloud_creds(None, None);
            let app = triage::bucket_app(&issue, cloud.clone(), key.clone()).await?;
            triage::triage(
                &app,
                &issue,
                Some(&status),
                fixed_in_build.as_deref(),
                assignee,
                ctx.json,
                cloud,
                key,
            )
            .await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Timeline { issue } => {
            let (cloud, key) = cloud_creds(None, None);
            let app = triage::bucket_app(&issue, cloud.clone(), key.clone()).await?;
            triage::timeline(&app, &issue, ctx.json, cloud, key).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Diagnose { report, run } => {
            let app = cloud_app_id(None)?;
            let (cloud, key) = cloud_creds(None, None);
            triage::diagnose(&app, &report, run, cloud, key).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::ResolutionEvents => {
            let app = cloud_app_id(None)?;
            let (cloud, key) = cloud_creds(None, None);
            triage::resolution_events(&app, ctx.json, cloud, key).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Watch { repro } => {
            let loaded = config::load(cli.config.as_deref())?;
            let video = resolve_repro_video(&loaded, &repro)?;
            if ctx.json {
                ctx.emit(&serde_json::json!({
                    "command": "watch",
                    "id": repro,
                    "video": video.display().to_string(),
                }));
                return Ok(ExitCode::SUCCESS);
            }
            open_in_player(&video)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Fix { run } => {
            let loaded = config::load(cli.config.as_deref())?;
            fix::fix(&loaded.config, &loaded.root, run.as_deref()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Analyze { run } => {
            let loaded = config::load(cli.config.as_deref())?;
            analyze::analyze(&loaded.config, &loaded.root, run.as_deref()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Scan(args) => scan_command::run(&ctx, cli.config.as_deref(), args).await,
        Cmd::Fuzz(args) => fuzz_command::run(&ctx, cli.config.as_deref(), args).await,
        Cmd::Mcp => {
            mcp::serve(cli.config.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Platforms => {
            platforms::print();
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Skills { action } => {
            match action {
                SkillsAction::Install {
                    format,
                    global,
                    dir,
                } => skills::install(format, global, dir)?,
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Auth {
            account,
            strategy,
            email,
            phone,
            username,
            password,
            otp,
            totp_secret,
            session,
            user_id,
            validate_text,
            no_discover,
            discover,
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            let exists = loaded
                .config
                .auth
                .accounts
                .iter()
                .any(|a| a.name == account);
            let mut strategy = strategy;
            let mut email = email;
            let mut phone = phone;
            let mut password = password;
            let mut otp = otp;
            let has_new_values = strategy.is_some()
                || email.is_some()
                || phone.is_some()
                || username.is_some()
                || password.is_some()
                || otp.is_some()
                || totp_secret.is_some()
                || session.is_some();
            if exists && !has_new_values {
                if discover {
                    discover_and_verify_login(cli.config.as_deref(), &account).await?;
                } else {
                    verify_configured_login(cli.config.as_deref(), &account).await?;
                }
            } else {
                if !exists && !has_new_values {
                    if ctx.yes || !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                        anyhow::bail!(
                            "new account `{account}` needs credentials; pass \
                             --email/--phone/--session"
                        );
                    }
                    println!(
                        "  Setting up {account}. Login mapping and verification are automatic."
                    );
                    println!("  Sign-in type: [1] email/password  [2] phone/OTP");
                    match auth_prompt("choice", false)?.as_str() {
                        "1" => {
                            strategy = Some(AuthStrategyArg::Password);
                            email = Some(auth_prompt("email", false)?);
                            password = Some(auth_prompt("password", true)?);
                        }
                        "2" => {
                            strategy = Some(AuthStrategyArg::PhoneOtp);
                            phone = Some(auth_prompt("phone", false)?);
                            otp = Some(auth_prompt("test OTP", true)?);
                        }
                        other => anyhow::bail!("unknown sign-in type `{other}`"),
                    }
                }
                let strategy = strategy
                    .or_else(|| {
                        if session.is_some() {
                            Some(AuthStrategyArg::Session)
                        } else if phone.is_some() {
                            Some(AuthStrategyArg::PhoneOtp)
                        } else if otp.is_some() || totp_secret.is_some() {
                            Some(AuthStrategyArg::PasswordOtp)
                        } else if email.is_some() || username.is_some() || password.is_some() {
                            Some(AuthStrategyArg::Password)
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "cannot create `{account}` without credentials; pass \
                             --email/--phone/--session (strategy is inferred)"
                        )
                    })?;
                auth_cmd(
                    cli.config.as_deref(),
                    AuthAction::Add {
                        account,
                        strategy,
                        email,
                        phone,
                        username,
                        password,
                        otp,
                        totp_secret,
                        session,
                        user_id,
                        validate_text,
                        no_discover: no_discover && !discover,
                    },
                )
                .await?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Journey { action } => {
            if matches!(
                &action,
                JourneyAction::Create { .. } | JourneyAction::Run(_)
            ) {
                let loaded = config::load(cli.config.as_deref())?;
                ensure_app_map(&ctx, &loaded, "explore").await?;
            }
            if let JourneyAction::Run(args) = &action {
                let [name] = args.as_slice() else {
                    anyhow::bail!("usage: reproit journey <name>");
                };
                let loaded = config::load(cli.config.as_deref())?;
                let result = journey::run(
                    &loaded,
                    name,
                    loaded.config.gate.runs.max(1),
                    ctx.json || ctx.quiet,
                )
                .await?;
                if ctx.json {
                    ctx.emit(&serde_json::json!({
                        "command": "journey",
                        "journey": name,
                        "outcome": result.outcome.as_str(),
                        "rate": result.rate(),
                        "exit": result.outcome.exit_code(),
                    }));
                } else {
                    ctx.say(format!(
                        "\njourney: {} ({})  {name}",
                        result.outcome.as_str().to_uppercase(),
                        result.rate()
                    ));
                }
                return Ok(ExitCode::from(result.outcome.exit_code()));
            }
            journey_cmd(cli.config.as_deref(), action, &ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Screenshots {
            tour,
            out,
            locale,
            target,
            device,
            no_verify,
            path_template,
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            ensure_app_map(&ctx, &loaded, "explore").await?;
            let locales = locale
                .as_deref()
                .map(crate::domain::locale::parse_locales)
                .unwrap_or_default();
            let (targets, unknown) = match target.as_deref() {
                Some(t) => crate::domain::target::parse_run_targets(t),
                None => (Vec::new(), Vec::new()),
            };
            for u in unknown {
                ctx.say(format!("  warn: unknown target `{u}` (ignored)"));
            }
            let devices: Vec<String> = device
                .as_deref()
                .map(|s| {
                    s.split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let args = screenshots::Args {
                tour,
                out,
                locales,
                targets,
                devices,
                verify: if no_verify { Some(false) } else { None },
                path_template,
            };
            let passed = screenshots::run(&ctx, &loaded, args).await?;
            Ok(if passed {
                ExitCode::SUCCESS
            } else {
                exit_with(Exit::Regression)
            })
        }
        Cmd::Import {
            source,
            path,
            name,
            out,
        } => {
            if let Some(path) = path {
                let loaded = config::load(cli.config.as_deref())?;
                ensure_app_map(&ctx, &loaded, "explore").await?;
                import::run(&ctx, &source, &path, name, out.as_deref())?;
            } else {
                if name.is_some() || out.is_some() {
                    anyhow::bail!("--name and --out apply only to tool flow imports");
                }
                let bundle_path = Path::new(&source);
                if bundle_path
                    .extension()
                    .is_none_or(|extension| extension != "rpb")
                {
                    anyhow::bail!(
                        "single-argument import expects a `.rpb` support bundle; \
                         use `reproit import maestro FLOW` for a tool flow"
                    );
                }
                bundle::import(&ctx, bundle_path)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Cloud { action } => {
            // Cloud commands talk to a remote; an unreachable/erroring cloud is
            // a clean, non-panicking failure with a one-line message (the full
            // chain stays available under --json for scripts).
            match cloud_cmd(cli.config.as_deref(), action, ctx.json, ctx.yes).await {
                Ok(()) => Ok(ExitCode::SUCCESS),
                Err(e) => {
                    if ctx.json {
                        ctx.emit(&serde_json::json!({
                            "command": "cloud",
                            "ok": false,
                            "error": e.to_string(),
                        }));
                    } else {
                        eprintln!("cloud: {e}");
                        eprintln!(
                            "  (is the cloud reachable? check REPROIT_CLOUD_URL / `reproit login`)"
                        );
                    }
                    Ok(exit_with(Exit::Regression))
                }
            }
        }
        Cmd::TuiRun => {
            tui::run()?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::UiaRun => {
            #[cfg(windows)]
            {
                uia::run()?;
                Ok(ExitCode::SUCCESS)
            }
            #[cfg(not(windows))]
            {
                anyhow::bail!("__uia (Windows UI Automation) is unsupported on this platform")
            }
        }
        Cmd::AtspiRun => {
            #[cfg(all(target_os = "linux", feature = "linux-atspi"))]
            {
                atspi::run()?;
                Ok(ExitCode::SUCCESS)
            }
            #[cfg(not(all(target_os = "linux", feature = "linux-atspi")))]
            {
                anyhow::bail!(
                    "__atspi (Linux AT-SPI) is unavailable in this build or on this platform"
                )
            }
        }
        Cmd::Devices => {
            let loaded = config::load(cli.config.as_deref())?;
            let sims = simctl::list_sims(&loaded.config.devices.name_prefix).await;
            if sims.is_empty() {
                println!(
                    "no simulators named {}-*",
                    loaded.config.devices.name_prefix
                );
            }
            for (name, udid, booted) in sims {
                println!(
                    "{name}  {udid}  {}",
                    if booted { "booted" } else { "shutdown" }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Repro {
            action: ReproAction::Why { dir, top },
        } => {
            repro::why(&dir, top);
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn journey_cmd(
    config_path: Option<&std::path::Path>,
    action: JourneyAction,
    ctx: &Ctx,
) -> Result<()> {
    let loaded = config::load(config_path)?;
    match action {
        JourneyAction::Run(_) => unreachable!("journey runs are handled asynchronously"),
        JourneyAction::List => {
            let journeys = journey::list(&loaded.root)?;
            if ctx.json {
                ctx.emit(&serde_json::json!({ "journeys": journeys }));
            } else if journeys.is_empty() {
                ctx.say("no journeys yet (author one with `reproit journey create`)");
            } else {
                for j in &journeys {
                    match &j.error {
                        Some(e) => ctx.say(format!("  {:<16} (broken: {e})", j.name)),
                        None => {
                            let setup = j
                                .setup
                                .as_ref()
                                .map(|s| format!(", setup {s}"))
                                .unwrap_or_default();
                            ctx.say(format!("  {:<16} {} steps{setup}", j.name, j.steps));
                        }
                    }
                }
            }
        }
        JourneyAction::Create { name, spec } => {
            let spec = match spec {
                Some(s) => s,
                None => {
                    use std::io::Read;
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            let path = journey::save(&loaded.root, &name, &spec)?;
            let rel = path.strip_prefix(&loaded.root).unwrap_or(&path);
            if ctx.json {
                ctx.emit(&serde_json::json!({
                    "saved": name,
                    "path": rel.to_string_lossy(),
                    "next": format!("reproit journey {name}"),
                }));
            } else {
                ctx.say(format!("  saved {}", rel.display()));
                ctx.say(format!("  run it: reproit journey {name}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
