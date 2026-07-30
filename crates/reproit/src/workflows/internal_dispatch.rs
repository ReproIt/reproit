//! Dispatch for the `reproit internal <sub>` implementation surface.
//!
//! Every arm here used to be a hidden top-level verb. The behavior and argv
//! semantics are unchanged; only the spelling moved under one multiplex so
//! the CLI vocabulary stays the six words a human needs.

#[cfg(all(target_os = "linux", feature = "linux-atspi"))]
use crate::adapters::atspi;
#[cfg(windows)]
use crate::adapters::uia;
use crate::adapters::{config, tui, update};
use crate::interface::cli::args::{
    AuthAction, AuthStrategyArg, DebugAction, JourneyAction, ListState, ReproAction, SkillsAction,
};
use crate::interface::cli::context::{exit_with, Ctx, Exit};
use crate::interface::cli::internal::InternalCmd;
use crate::interface::mcp;
use crate::VERSION;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::auth::{auth_cmd, auth_prompt, discover_and_verify_login, verify_configured_login};
use super::authored_contract::run_vitest_contract;
use super::capture::{
    load_original, open_cloud_capture, show_original, upload_original, watch_original,
};
use super::cloud::{cloud_app_id, cloud_cmd, cloud_creds};
use super::create_command::{self, CreateArgs};
use super::map::{debug_map, ensure_app_map};
use super::proof::{list_candidates, show_proof};
use super::repro::simplify_repro;
use super::{
    backend_headless, backend_learn, bundle, command_capture, fuzz_command, import, journey, list,
    platforms, repro, reset, scan_command, screenshots, skills, triage, visual,
};

pub(super) async fn run(
    ctx: &Ctx,
    config_path: Option<PathBuf>,
    cmd: InternalCmd,
) -> Result<ExitCode> {
    match cmd {
        InternalCmd::List { state, query } => match state {
            ListState::Guards => {
                if query.is_some() {
                    anyhow::bail!("--query applies only to `--state bugs`");
                }
                let loaded = list::load_read_view(config_path.as_deref())?;
                list::guards(ctx, &loaded, "list")
            }
            ListState::Candidates => {
                if query.is_some() {
                    anyhow::bail!("--query applies only to `--state bugs`");
                }
                let loaded = list::load_read_view(config_path.as_deref())?;
                list_candidates(ctx, &loaded)?;
                Ok(ExitCode::SUCCESS)
            }
            ListState::Bugs => {
                let app = cloud_app_id(None)?;
                let (cloud, key) = cloud_creds(None, None);
                triage::buckets(&app, query.as_deref(), ctx.json, cloud, key).await?;
                Ok(ExitCode::SUCCESS)
            }
        },
        InternalCmd::ProcessCapture { out, command } => {
            super::process_capsule::capture(ctx, &out, &command)
        }
        InternalCmd::Surface => backend_learn::surface(ctx, &std::env::current_dir()?),
        InternalCmd::Reset {
            all,
            init: initialize,
            platform,
        } => reset::run(
            config_path.as_deref(),
            ctx,
            all,
            initialize,
            platform.as_deref(),
        ),
        InternalCmd::Update { check } => {
            update::run(VERSION, check).await?;
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::UpdateCheck => {
            let _ = update::refresh_cache(VERSION).await;
            Ok(ExitCode::SUCCESS)
        }
        // Advanced graph diagnostics. Normal workflows call ensure_app_map and
        // never require users or agents to manage this lifecycle explicitly.
        InternalCmd::Debug {
            action: DebugAction::Map { action },
        } => debug_map(config_path.as_deref(), action, ctx).await,
        // Deterministic local re-evaluation of a production backend capture.
        InternalCmd::Debug {
            action: DebugAction::ReplayCapture { file },
        } => backend_headless::replay_capture(ctx, &file),
        InternalCmd::VitestContract {
            cwd,
            test_path,
            test_name,
            pnpm_version,
        } => run_vitest_contract(ctx, &cwd, &test_path, &test_name, &pnpm_version).await,
        InternalCmd::Create {
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
                ctx,
                CreateArgs {
                    config_path,
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
        InternalCmd::Push { capture, no_open } => {
            let capture = load_original(config_path.as_deref(), &capture)?;
            upload_original(&capture, no_open, ctx).await?;
            Ok(ExitCode::SUCCESS)
        }
        // The visual oracle: diff the current capture against the committed
        // baseline; `--update` accepts the current capture as the baseline.
        InternalCmd::Baseline { update } => {
            let loaded = config::load(config_path.as_deref())?;
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
        // Support-bundle verification: manifest, digest, and signature checks
        // without decrypting artifacts (docs/support-bundle-security.md).
        InternalCmd::Inspect { reference } => {
            if !reference.ends_with(".rpb") {
                anyhow::bail!(
                    "internal inspect verifies `.rpb` support bundles; reproduce a bug with \
                     `reproit <id>` (interactive by default)"
                );
            }
            bundle::inspect(ctx, Path::new(&reference))?;
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::Collect {
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
            bundle::collect(ctx, args)?;
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::CaptureCommand {
            bundle: bundle_file,
            project,
            component,
            identity,
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
            if let Some(bundle_path) = bundle_file {
                if !command.is_empty()
                    || project.is_some()
                    || component.is_some()
                    || identity.is_some()
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
                bundle::import(ctx, &bundle_path)?;
                return Ok(ExitCode::SUCCESS);
            }
            if command.is_empty() {
                if project.is_some()
                    || component.is_some()
                    || identity.is_some()
                    || include_output
                    || local_only
                {
                    anyhow::bail!("command capture options require `-- <command>`");
                }
                return create_command::run(
                    ctx,
                    CreateArgs {
                        config_path,
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
                anyhow::bail!("application capture options cannot be combined with `-- <command>`");
            }
            command_capture::run(
                ctx,
                command_capture::CommandCaptureArgs {
                    project,
                    component,
                    identity,
                    timeout_ms,
                    include_output,
                    local_only,
                    command,
                },
            )
            .await
        }
        InternalCmd::Occurrence { reference, no_run } => {
            bundle::run_occurrence(ctx, config_path.as_deref(), &reference, no_run).await
        }
        InternalCmd::Proof { reference } => {
            let loaded = config::load(config_path.as_deref())?;
            show_proof(ctx, &loaded, &reference)?;
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::Verify {
            ids,
            junit,
            prune_retracted,
        } => {
            backend_headless::backend_verify(
                ctx,
                config_path.as_deref(),
                &ids,
                junit.as_deref(),
                prune_retracted,
            )
            .await
        }
        InternalCmd::Accept {
            ids,
            reason,
            until,
            remove,
            list,
        } => {
            backend_headless::backend_accept(ctx, &ids, &reason, until.as_deref(), remove, list)
                .await
        }
        InternalCmd::Repro {
            action: ReproAction::Simplify { repro, to },
        } => simplify_repro(ctx, config_path.as_deref(), &repro, &to).await,
        InternalCmd::Repro {
            action: ReproAction::Why { dir, top },
        } => {
            repro::why(&dir, top);
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::ReplayBucket {
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
            let loaded = config::load(config_path.as_deref()).with_context(|| {
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
        InternalCmd::OriginalCapture {
            capture,
            watch,
            open,
        } => {
            let capture = load_original(config_path.as_deref(), &capture)?;
            if watch {
                watch_original(&capture)?;
            } else if open {
                open_cloud_capture(&capture, ctx).await?;
            } else {
                show_original(&capture, ctx)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::Triage {
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
        InternalCmd::Timeline { issue } => {
            let (cloud, key) = cloud_creds(None, None);
            let app = triage::bucket_app(&issue, cloud.clone(), key.clone()).await?;
            triage::timeline(&app, &issue, ctx.json, cloud, key).await?;
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::ResolutionEvents => {
            let app = cloud_app_id(None)?;
            let (cloud, key) = cloud_creds(None, None);
            triage::resolution_events(&app, ctx.json, cloud, key).await?;
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::Scan(args) => scan_command::run(ctx, config_path.as_deref(), args).await,
        InternalCmd::Fuzz(args) => fuzz_command::run(ctx, config_path.as_deref(), args).await,
        InternalCmd::Mcp => {
            mcp::serve(config_path.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::Platforms => {
            platforms::print();
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::Skills { action } => {
            match action {
                SkillsAction::Install {
                    format,
                    global,
                    dir,
                } => skills::install(format, global, dir)?,
            }
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::Auth {
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
            let loaded = config::load(config_path.as_deref())?;
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
                    discover_and_verify_login(config_path.as_deref(), &account).await?;
                } else {
                    verify_configured_login(config_path.as_deref(), &account).await?;
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
                    config_path.as_deref(),
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
        InternalCmd::Journey { action } => {
            if matches!(
                &action,
                JourneyAction::Create { .. } | JourneyAction::Run(_)
            ) {
                let loaded = config::load(config_path.as_deref())?;
                ensure_app_map(ctx, &loaded, "explore").await?;
            }
            if let JourneyAction::Run(args) = &action {
                let [name] = args.as_slice() else {
                    anyhow::bail!("usage: reproit internal journey <name>");
                };
                let loaded = config::load(config_path.as_deref())?;
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
            journey_cmd(config_path.as_deref(), action, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::Screenshots {
            tour,
            out,
            locale,
            target,
            device,
            no_verify,
            path_template,
        } => {
            let loaded = config::load(config_path.as_deref())?;
            ensure_app_map(ctx, &loaded, "explore").await?;
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
            let passed = screenshots::run(ctx, &loaded, args).await?;
            Ok(if passed {
                ExitCode::SUCCESS
            } else {
                exit_with(Exit::Regression)
            })
        }
        InternalCmd::Import {
            source,
            path,
            name,
            out,
        } => {
            if let Some(path) = path {
                let loaded = config::load(config_path.as_deref())?;
                ensure_app_map(ctx, &loaded, "explore").await?;
                import::run(ctx, &source, &path, name, out.as_deref())?;
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
                         use `reproit internal import maestro FLOW` for a tool flow"
                    );
                }
                bundle::import(ctx, bundle_path)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::Cloud { action } => {
            // Cloud commands talk to a remote; an unreachable/erroring cloud is
            // a clean, non-panicking failure with a one-line message (the full
            // chain stays available under --json for scripts).
            match cloud_cmd(config_path.as_deref(), action, ctx.json, ctx.yes).await {
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
        InternalCmd::TuiRun => {
            tui::run()?;
            Ok(ExitCode::SUCCESS)
        }
        InternalCmd::UiaRun => {
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
        InternalCmd::AtspiRun => {
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
    }
}

fn journey_cmd(config_path: Option<&Path>, action: JourneyAction, ctx: &Ctx) -> Result<()> {
    let loaded = config::load(config_path)?;
    match action {
        JourneyAction::Run(_) => unreachable!("journey runs are handled asynchronously"),
        JourneyAction::List => {
            let journeys = journey::list(&loaded.root)?;
            if ctx.json {
                ctx.emit(&serde_json::json!({ "journeys": journeys }));
            } else if journeys.is_empty() {
                ctx.say("no journeys yet (author one with `reproit internal journey create`)");
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
                    "next": format!("reproit internal journey {name}"),
                }));
            } else {
                ctx.say(format!("  saved {}", rel.display()));
                ctx.say(format!("  run it: reproit internal journey {name}"));
            }
        }
    }
    Ok(())
}
