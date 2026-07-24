//! User-facing workflows and their command dispatch.

pub(crate) mod a2ui;
pub(crate) mod accessibility;
pub(crate) mod analyze;
pub(crate) mod backend_headless;
pub(crate) mod barrier;
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
mod backend_target;
mod capture;
mod change_selection;
mod check;
mod cloud;
mod create_command;
mod device;
mod doctor;
mod fuzz_command;
mod init_command;
mod inspect;
mod map;
mod proof;
mod record;
mod repro;
mod reset;
mod route_access;
mod scan_command;

#[cfg(all(target_os = "linux", feature = "linux-atspi"))]
use crate::adapters::atspi;
use crate::adapters::scoped_env::ScopedEnv;
#[cfg(windows)]
use crate::adapters::uia;
use crate::adapters::{config, crash_reporter as crashreporter, project_scaffold, update};
use crate::adapters::{orchestrator, platform, simctl, tui};
use crate::domain::capsule;
use crate::domain::{appmap, fault};
use crate::interface::cli::args::{
    AuthAction, AuthStrategyArg, Cli, CloudAction, Cmd, DebugAction, JourneyAction, MapAction,
    ReproAction, SkillsAction,
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
use repro::{keep_repro, load_repro_actions, repro_label, simplify_repro};
use std::path::Path;
use std::process::ExitCode;

/// SAFETY gate for a zero-config TUI fuzz: it drives a REAL process with REAL
/// side effects (synthetic keystrokes can send messages, run shell commands,
/// write/delete files), so confirm before launching. Always warns; proceeds on
/// `--yes`, else prompts on a TTY, else refuses (CI must pass `--yes`).
fn confirm_tui_fuzz(ctx: &Ctx, exe: &str) -> bool {
    eprintln!(
        "  WARNING: reproit will drive `{exe}` in a PTY by sending SYNTHETIC KEYSTROKES.\n  A \
         real terminal app can have real side effects (send messages, run shell\n  commands, \
         write or delete files). Point it at a THROWAWAY / sandboxed instance."
    );
    if ctx.yes {
        return true;
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("  Refusing without confirmation. Re-run with --yes to proceed.");
        return false;
    }
    use std::io::Write;
    eprint!("  Proceed? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Run the CLI from an explicit argument sequence.
///
/// Keeping argument acquisition outside dispatch makes parsing deterministic
/// and lets command-contract tests avoid mutating process-global arguments.
pub(crate) async fn run_from<I, T>(args: I) -> Result<ExitCode>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    use crate::domain::repro;

    let cli = Cli::parse_args(args);
    let ctx = cli.ctx();
    if !matches!(&cli.command, Cmd::Update { .. } | Cmd::UpdateCheck) {
        update::notice_and_schedule(VERSION, cli.quiet, cli.json);
    }
    match cli.command {
        Cmd::Init {
            target,
            platform,
            force,
        } => init_command::run(&ctx, target, platform, force).await,
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
            strict,
            locale,
            target,
            device,
            record_video,
            flicker,
            changed,
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
                    strict,
                    locale,
                    target,
                    device,
                    record_video,
                    flicker,
                    changed,
                    inspect: false,
                },
            )
            .await
        }
        Cmd::Inspect { reference, offline } => {
            inspect::run(&ctx, cli.config.as_deref(), &reference, offline).await
        }
        Cmd::Proof { reference } => {
            let loaded = config::load(cli.config.as_deref())?;
            show_proof(&ctx, &loaded, &reference)?;
            Ok(ExitCode::SUCCESS)
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
            let metas = repro::list(&loaded.root);
            if ctx.json {
                let items: Vec<serde_json::Value> = metas
                    .iter()
                    .map(|m| {
                        // The action sequence too, so an agent can see what to
                        // simplify (reproit_simplify) without a second call.
                        let actions = load_repro_actions(&loaded, &m.id).unwrap_or_default();
                        serde_json::json!({
                            "id": repro::display_repro_id(&m.id),
                            "kind": "repro",
                            "alias": m.alias,
                            "status": m.status.as_str(),
                            "seed": m.seed,
                            "created": m.created,
                            "last_checked": m.last_checked,
                            "last_result": m.last_result,
                            "actions": actions,
                        })
                    })
                    .collect();
                ctx.emit(&serde_json::json!({ "command": "repros", "repros": items }));
                return Ok(ExitCode::SUCCESS);
            }
            if metas.is_empty() {
                ctx.say("no saved repros. Find some with `reproit fuzz`, then `reproit keep`.");
            } else {
                ctx.say(format!(
                    "  {:<14} {:<18} {:<12} {}",
                    "ID", "ALIAS", "STATUS", "LAST CHECK"
                ));
                for m in &metas {
                    ctx.say(format!(
                        "  {:<14} {:<18} {:<12} {}",
                        repro::display_repro_id(&m.id),
                        m.alias.as_deref().unwrap_or("-"),
                        m.status.as_str(),
                        m.last_result.as_deref().unwrap_or("never"),
                    ));
                }
            }
            Ok(ExitCode::SUCCESS)
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
        Cmd::Capture {
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
            print_platforms();
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
            tool,
            path,
            name,
            out,
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            ensure_app_map(&ctx, &loaded, "explore").await?;
            import::run(&ctx, &tool, &path, name, out.as_deref())?;
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
            let mut files = Vec::new();
            collect_cov_files(std::path::Path::new(&dir), &mut files);
            let runs: Vec<fault::RunCoverage> = files
                .iter()
                .filter_map(|p| {
                    let v: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
                    Some(fault::RunCoverage {
                        passed: v.get("passed").and_then(|x| x.as_bool()).unwrap_or(true),
                        covered: v
                            .get("covered")
                            .and_then(|x| x.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|s| s.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect();
            let failed = runs.iter().filter(|r| !r.passed).count();
            println!(
                "fault localization over {} coverage snapshot(s) ({failed} failing):",
                runs.len()
            );
            let ranked = fault::ochiai(&runs);
            if ranked.is_empty() {
                println!("  nothing to localize (no failing runs, or no coverage)");
            }
            for (elem, susp) in ranked.into_iter().take(top) {
                println!("  {susp:.3}  {elem}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Print the platform support matrix: every registered UI framework and the
/// backend it routes to.
fn print_platforms() {
    println!("Platform support matrix (UI framework -> introspection backend)\n");
    println!("  {:<16} {:<26} CAPABILITY", "PLATFORM", "BACKEND");
    for p in platform::all() {
        println!("  {:<16} {:<26} {}", p.id, p.backend.as_str(), p.note);
    }
    println!(
        "\n  All listed platform IDs are live. Local readiness still depends on `reproit doctor` \
         and host tooling.\n\n  The point: Qt/GTK/WinUI/Avalonia/wxWidgets share ONE backend per \
         OS\n(they publish to the OS accessibility API), Electron/Tauri reuse the\nweb backend, \
         Appium covers native mobile, and TUI uses a PTY."
    );
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

/// Recursively collect files ending in `.cov.json` under `dir` (coverage
/// snapshots written by instrumented runs). Best-effort: unreadable dirs are
/// skipped.
fn collect_cov_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_cov_files(&p, out);
        } else if p.to_string_lossy().ends_with(".cov.json") {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests;
