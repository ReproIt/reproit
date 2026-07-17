//! Application command dispatch and command-oriented workflows.

mod auth;
mod cloud;
mod device;
mod doctor;
mod map;
mod record;
mod repro;

#[cfg(target_os = "linux")]
use crate::backends::atspi;
#[cfg(windows)]
use crate::backends::uia;
use crate::backends::{orchestrator, platform, simctl, tui};
use crate::cli::args::{
    AuthAction, AuthStrategyArg, Cli, CloudAction, Cmd, DebugAction, JourneyAction, MapAction,
    ReproAction, SkillsAction,
};
use crate::cli::context::{exit_with, Ctx, Exit};
use crate::cli::target::{target_as_executable, target_as_url};
use crate::infra::ScopedEnv;
use crate::model::{accessibility, appmap, backend, fault};
use crate::modes::{
    a2ui, analyze, backend_headless, fix, flicker, fuzz, graph, import, journey, mapplan, pwfuzz,
    screenshots, soak, triage, visual,
};
use crate::{
    capsule, config, crashreporter, crosscut, exec, init, junit, layout, mcp, skills, update,
    VERSION,
};
use anyhow::{Context, Result};
use auth::{auth_cmd, auth_prompt, discover_and_verify_login};
#[cfg(test)]
use cloud::choose_cloud_project;
use cloud::{cloud_app_id, cloud_cmd, cloud_creds};
use device::{
    is_web_engines, pick_device_interactive, resolve_check_device, run_check_targets,
    run_needs_device_pick, run_targets,
};
use doctor::doctor;
use map::{debug_map, ensure_app_map, rebuild_app_map};
#[cfg(test)]
use record::web_record_metadata;
use record::{
    exploratory_record_session, minimize_record_replay, open_in_player, resolve_repro_video,
};
use repro::{
    adopt_simplified, check_label, check_repro, find_finding_by_id, keep_repro, latest_finding,
    load_repro_actions, public_json_id, public_json_kind, repro_label, resolve_repro_journey,
};
#[cfg(test)]
use repro::{
    build_simplified_replay, parse_fuzz_finding_id, parse_fuzz_oracle, parse_fuzz_report, Finding,
};
use std::path::{Path, PathBuf};
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
    use crate::model::repro;

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
        } => {
            let root = std::env::current_dir()?;
            if let Some(target) = target {
                if platform.as_deref().is_some_and(|value| value != "web") {
                    anyhow::bail!(
                        "a URL initializes the web UI workflow; remove --platform or use \
                         --platform web"
                    );
                }
                let url = target_as_url(&target).ok_or_else(|| {
                    anyhow::anyhow!("init target must be a web URL, got {target:?}")
                })?;
                let runner = config::ensure_web_runner_dir(VERSION, &|message| ctx.say(message))?;
                init::init_web_url(&root, &url, &runner, force)?;
            } else {
                init::init(&root, platform.as_deref(), force)?;
            }
            Ok(ExitCode::SUCCESS)
        }
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
        // `record`: run a repro ONCE with full evidence + annotated video,
        // REPLAYING the kept repro. The runner draws the annotated overlay (paced
        // action HUD + the red finding box) ONLY when it is fed a replay; without
        // one it explores freely and the "annotated video" is a random walk, not
        // the bug. So we write the repro's stored action sequence to the fuzz
        // config the runner reads and hand the path to the orchestrator as a
        // define, instead of running a bare `explore` journey. `--flicker` then
        // scans the recorded video for transient render glitches.
        Cmd::Record {
            repro,
            app,
            timeout,
            kind,
            devices,
            warm,
            shots_dir,
            profile,
            flicker,
        } => {
            if repro.is_none() {
                return exploratory_record_session(
                    cli.config.as_deref(),
                    app,
                    timeout,
                    kind.as_deref(),
                    &ctx,
                )
                .await;
            }
            let repro = repro.expect("checked above");
            let loaded = config::load(cli.config.as_deref()).with_context(|| {
                "recording a production bug needs a runnable app configuration. In a source \
                 checkout run `reproit init`; for a deployed web app run `reproit init \
                 https://app.example.com` in a workspace; from elsewhere pass \
                 `--config /path/to/reproit.yaml`"
            })?;
            if repro.starts_with("bkt_") && repro::resolve(&loaded.root, &repro).is_none() {
                let (cloud, key) = cloud_creds(None, None);
                triage::pull_global(&loaded.root, &repro, &repro, ctx.json, cloud, key).await?;
            }
            let journey = resolve_repro_journey(&loaded.root, &repro)?;
            let meta = repro::resolve(&loaded.root, &repro)
                .ok_or_else(|| anyhow::anyhow!("no repro `{repro}` (by id or alias)"))?;
            let replay_path = repro::repro_dir(&loaded.root, &meta.id).join("replay.json");
            let mut replay: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&replay_path).map_err(|e| {
                    anyhow::anyhow!(
                        "reading replay for `{repro}` ({}): {e}",
                        replay_path.display()
                    )
                })?)?;
            // Tell the runner which finding this clip is for, so the annotated box
            // is scoped to JUST that oracle's issue (one box), not every problem
            // on the page. The runner reads `highlight` from the config.
            let saved_meta = repro::load_meta(&loaded.root, &meta.id);
            if let Some(saved) = saved_meta.as_ref() {
                minimize_record_replay(&mut replay, saved);
            }
            if let Some(oracle) = saved_meta.and_then(|m| m.oracle) {
                if let Some(obj) = replay.as_object_mut() {
                    obj.insert("highlight".to_string(), serde_json::Value::String(oracle));
                }
            }
            let cfg_path = layout::fuzz_config_path(&loaded.root);
            std::fs::create_dir_all(cfg_path.parent().unwrap())?;
            std::fs::write(&cfg_path, replay.to_string())?;
            let extra = vec![(
                "REPROIT_FUZZ_CONFIG".to_string(),
                cfg_path.to_string_lossy().into_owned(),
            )];
            let outcome = orchestrator::run_journey(
                &loaded.config,
                &loaded.root,
                &journey,
                &orchestrator::RunOpts {
                    kind: kind.as_deref(),
                    devices,
                    warm,
                    shots_dir: shots_dir.as_deref(),
                    profile,
                    extra_defines: &extra,
                    // `record` produces an annotated video, so the runner must
                    // record even when evidence.video is off.
                    record_video: true,
                    ..Default::default()
                },
            )
            .await?;
            // `--flicker`: scan the just-recorded video frame-to-frame for
            // transient render glitches (a frame that diverges then snaps back).
            if flicker {
                let events =
                    flicker::analyze_run(&outcome.run_dir, &flicker::FlickerCfg::default()).await?;
                let clean = flicker::report(&events);
                return Ok(if clean {
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
        // aggregates the worst outcome. Recording and baseline diff are their own
        // verbs now (`record`/`baseline`).
        Cmd::Check {
            repro,
            devices,
            kind,
            runs,
            junit,
            strict,
            locale,
            target,
            device,
        } => {
            if let Some(id) = repro.as_deref() {
                if let Some(code) = backend_headless::try_replay(&ctx, id).await? {
                    return Ok(code);
                }
                if let Some(code) = a2ui::try_replay(&ctx, id)? {
                    return Ok(code);
                }
            }
            let loaded = config::load(cli.config.as_deref())?;
            ensure_app_map(&ctx, &loaded, "explore").await?;
            let locales = locale
                .as_deref()
                .map(crosscut::parse_locales)
                .unwrap_or_default();
            // MULTI-TARGET --target dispatch for `check`: when `--target` names
            // more than one run target (web engines chromium,firefox,webkit, or
            // platforms ios,android), run the saved suite on EACH target and diff
            // which repros are red on a SUBSET of targets (a divergence). The
            // single-target / no-target path below stays the rich locale+junit+
            // promotion flow unchanged.
            if let Some(raw) = target.as_deref() {
                let (rts, unknown_t) = crosscut::parse_run_targets(raw);
                for u in &unknown_t {
                    ctx.say(format!("  warn: unknown target `{u}` (ignored)"));
                }
                if rts.len() > 1 {
                    return run_check_targets(
                        &ctx,
                        &loaded,
                        &rts,
                        device.as_deref(),
                        &repro,
                        runs,
                        devices,
                        kind.as_deref(),
                    )
                    .await;
                }
            }
            // --target / --device device selection. When neither is given and a
            // TTY is present (and not --yes), pick interactively; non-interactive
            // falls back to the config default rather than hanging.
            let selected_device = resolve_check_device(
                &ctx,
                &loaded.config.app.platform,
                target.as_deref(),
                device.as_deref(),
            )
            .await;
            if let Some(d) = &selected_device {
                std::env::set_var("REPROIT_PLATFORM", d.target.as_str());
                std::env::set_var("REPROIT_DEVICE", &d.id);
                ctx.say(format!("  device: {} ({})", d.name, d.target.as_str()));
            }
            let times = runs.unwrap_or(loaded.config.gate.runs).max(1);
            // A scripted journey (journeys/<name>.yaml) is a first-class check
            // target. If the name is not a saved repro or a pending finding but a
            // journey file exists, run it as a journey (repros win a name clash).
            if let Some(r) = &repro {
                if repro::resolve(&loaded.root, r).is_none()
                    && find_finding_by_id(&loaded, r).is_none()
                    && journey::exists(&loaded.root, r)
                {
                    let result = journey::run(&loaded, r, times, ctx.json || ctx.quiet).await?;
                    if ctx.json {
                        ctx.emit(&serde_json::json!({
                            "command": "check",
                            "journey": r,
                            "outcome": result.outcome.as_str(),
                            "rate": result.rate(),
                            "exit": result.outcome.exit_code(),
                        }));
                    } else {
                        ctx.say(format!(
                            "\ncheck: {} ({})  journey {r}",
                            result.outcome.as_str().to_uppercase(),
                            result.rate()
                        ));
                    }
                    return Ok(ExitCode::from(result.outcome.exit_code()));
                }
            }
            // `check` with no name = run the whole saved suite; aggregate worst.
            let metas = match &repro {
                // A kept repro (id or alias) first; failing that, a PENDING fuzz
                // finding by id, so you can `check <id>` to confirm a finding
                // replays before you `keep` it.
                Some(r) => vec![match repro::resolve(&loaded.root, r) {
                    Some(m) => m,
                    None => find_finding_by_id(&loaded, r)
                        .map(|f| f.pending_meta())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "no repro or finding `{r}` (by id or alias). List saved bugs with \
                                 `reproit bugs`, or find some with `reproit fuzz`."
                            )
                        })?,
                }],
                None => {
                    let all = repro::list(&loaded.root);
                    if all.is_empty() {
                        if ctx.json {
                            ctx.emit(&serde_json::json!({
                                "command": "check",
                                "repros": [],
                                "outcome": "pass",
                                "exit": 0,
                            }));
                            return Ok(ExitCode::SUCCESS);
                        }
                        anyhow::bail!(
                            "no repros to check. Find some with `reproit fuzz`, then `reproit \
                             keep`."
                        );
                    }
                    all
                }
            };

            let mut results = Vec::new();
            let mut worst = repro::Outcome::Pass;
            let mut cases: Vec<junit::Case> = Vec::new();
            // Locale runs: either one app-default pass (None) or one pass per
            // locale. For the cross-locale diff we record, per repro id, the set
            // of locales it FAILED in (fail/flaky/stale), so a repro red in one
            // locale and green in another is flagged as a locale-specific bug.
            let locale_runs: Vec<Option<&str>> = if locales.is_empty() {
                vec![None]
            } else {
                locales.iter().map(|l| Some(l.as_str())).collect()
            };
            let mut failed_by_id: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for loc in &locale_runs {
                if let Some(l) = loc {
                    ctx.say(format!("\n=== locale {l} ==="));
                }
                for meta in &metas {
                    let label = match loc {
                        Some(l) => format!("{} @{l}", check_label(meta)),
                        None => check_label(meta),
                    };
                    ctx.say(format!("check {label}"));
                    let (result, run_dir) = check_repro(
                        &loaded,
                        &meta.id,
                        times,
                        devices,
                        kind.as_deref(),
                        *loc,
                        ctx.json || ctx.quiet,
                        None,
                    )
                    .await?;
                    // Quarantined repros are "reported but non-blocking" in a
                    // WHOLE-SUITE check (no id): a fresh keep can't break CI before
                    // it has proven green once. But an EXPLICIT `check <id>` is the
                    // user verifying THAT bug -- if it still reproduces it must be
                    // RED (exit non-zero), so the find -> check(RED) -> fix ->
                    // check(GREEN) -> guard loop is honest. `--strict` blocks
                    // everywhere; required repros always gate.
                    let blocks =
                        strict || repro.is_some() || meta.status != repro::Status::Quarantined;
                    let effective = if blocks {
                        result.outcome
                    } else {
                        repro::Outcome::Pass
                    };
                    worst = worst.max(effective);
                    if result.outcome != repro::Outcome::Pass {
                        if let Some(l) = loc {
                            failed_by_id
                                .entry(meta.id.clone())
                                .or_default()
                                .push(l.to_string());
                        }
                    }
                    cases.push(junit::Case {
                        name: format!("check {label}"),
                        passed: result.outcome == repro::Outcome::Pass,
                        time_s: 0.0,
                        message: format!(
                            "{} ({}); evidence: {}",
                            result.outcome.as_str(),
                            result.rate(),
                            run_dir.display()
                        ),
                    });
                    // Auto-promote: the first time a quarantined repro passes, it
                    // becomes required (write meta).
                    let mut updated = meta.clone();
                    updated.last_checked = Some(chrono::Local::now().to_rfc3339());
                    updated.last_result = Some(result.outcome.as_str().to_string());
                    let mut promoted = false;
                    if result.outcome == repro::Outcome::Pass
                        && meta.status == repro::Status::Quarantined
                    {
                        updated.status = repro::Status::Required;
                        promoted = true;
                    }
                    repro::save_meta(&loaded.root, &updated)?;
                    ctx.say(format!(
                        "  {} {} ({}){}",
                        result.outcome.as_str().to_uppercase(),
                        label,
                        result.rate(),
                        if promoted {
                            "  promoted -> required"
                        } else {
                            ""
                        }
                    ));
                    results.push(serde_json::json!({
                        "id": public_json_id(meta),
                        "kind": public_json_kind(meta),
                        "alias": meta.alias,
                        "locale": loc,
                        "outcome": result.outcome.as_str(),
                        "rate": result.rate(),
                        "green": result.green,
                        "total": result.total,
                        "status": updated.status.as_str(),
                        "promoted": promoted,
                        "exit": result.outcome.exit_code(),
                        "evidence": run_dir.to_string_lossy(),
                    }));
                }
            }
            // Cross-locale diff: a repro that failed in SOME locales but not all
            // is a locale-specific (i18n) finding.
            if locale_runs.len() > 1 {
                let mut any = false;
                for meta in &metas {
                    if let Some(failed) = failed_by_id.get(&meta.id) {
                        if failed.len() < locale_runs.len() {
                            if !any {
                                ctx.say("\nlocale diff: locale-specific failures (i18n):");
                                any = true;
                            }
                            ctx.say(format!(
                                "  {} fails only in: {}",
                                check_label(meta),
                                failed.join(", ")
                            ));
                        }
                    }
                }
                if !any {
                    ctx.say("\nlocale diff: no locale-specific failures");
                }
            }
            if let Some(path) = junit.as_deref() {
                if let Err(e) = junit::write(path, "check", &cases) {
                    ctx.say(format!(
                        "  warn: could not write junit {}: {e}",
                        path.display()
                    ));
                } else {
                    ctx.say(format!("  junit: {}", path.display()));
                }
            }
            ctx.emit(&serde_json::json!({
                "command": "check",
                "repros": results,
                "outcome": worst.as_str(),
                "exit": worst.exit_code(),
            }));
            ctx.say(format!(
                "\ncheck: {} ({} repro(s))",
                worst.as_str().to_uppercase(),
                metas.len()
            ));
            Ok(exit_with(Exit::from(worst)))
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
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            // Reference repro (kept or a pending finding) for its oracle + meta.
            let meta = repro::resolve(&loaded.root, &repro)
                .or_else(|| find_finding_by_id(&loaded, &repro).map(|f| f.pending_meta()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no repro or finding `{repro}` (by id or alias). List them with `reproit \
                         repros`."
                    )
                })?;
            let current = load_repro_actions(&loaded, &meta.id)?;
            let parsed: Vec<String> = serde_json::from_str(&to)
                .map_err(|e| anyhow::anyhow!("--to must be a JSON array of action strings: {e}"))?;
            let candidate = repro::normalize_actions(&parsed);
            if candidate.is_empty() {
                anyhow::bail!("--to is empty");
            }
            // VERIFY the candidate reproduces the same finding (deterministic).
            let times = loaded.config.gate.runs.max(1);
            let (result, _) = check_repro(
                &loaded,
                &meta.id,
                times,
                1,
                None,
                None,
                ctx.json || ctx.quiet,
                Some(&candidate),
            )
            .await?;
            let reproduces = result.outcome == repro::Outcome::Fail;
            let new_id = repro::repro_id(meta.seed, &candidate);
            // Adopt only a verified, no-longer, genuinely-different candidate.
            let adopt = reproduces && candidate.len() <= current.len() && new_id != meta.id;
            if adopt {
                adopt_simplified(&loaded, &meta, &candidate, &new_id)?;
            }
            if ctx.json {
                ctx.emit(&serde_json::json!({
                    "command": "simplify",
                    "repro": public_json_id(&meta),
                    "kind": public_json_kind(&meta),
                    "reproduces": reproduces,
                    "verdict": result.outcome.as_str(),
                    "from_actions": current.len(),
                    "to_actions": candidate.len(),
                    "adopted": adopt,
                    "new_id": adopt.then(|| repro::display_repro_id(&new_id)),
                    "alias": meta.alias,
                }));
            } else if adopt {
                let tag = meta
                    .alias
                    .as_deref()
                    .map(|a| format!(" [{a}]"))
                    .unwrap_or_default();
                ctx.say(format!(
                    "  simplified {} ({} actions) -> {} ({} actions){tag}",
                    public_json_id(&meta),
                    current.len(),
                    repro::display_repro_id(&new_id),
                    candidate.len()
                ));
            } else if !reproduces {
                ctx.say(format!(
                    "  candidate did NOT reproduce (verdict: {}); kept {}",
                    result.outcome.as_str(),
                    public_json_id(&meta)
                ));
            } else {
                ctx.say(format!(
                    "  candidate reproduces but is not shorter ({} vs {}); kept {}",
                    candidate.len(),
                    current.len(),
                    public_json_id(&meta)
                ));
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Repros => {
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
            triage::reproduce_bucket_global(
                &loaded.root,
                &issue,
                &alias,
                !no_run,
                None,
                ctx.json,
                cloud,
                key,
            )
            .await?;
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
        Cmd::Scan {
            target_arg,
            service,
            budget,
            sim,
            record,
            out,
            header,
        } => {
            if let Some(service) = service {
                std::env::set_var("REPROIT_BACKEND_URL", service);
            }
            let configured_backend = if target_arg.is_none() {
                backend_config_target(cli.config.as_deref())?
            } else {
                None
            };
            if let Some(path) = target_arg.as_deref().map(PathBuf::from) {
                if path.is_file() && a2ui::looks_like_target(&path) {
                    if record {
                        anyhow::bail!(
                            "A2UI streams produce a minimal JSON reproduction, so `scan --record` \
                             does not apply"
                        );
                    }
                    return a2ui::run_target(&ctx, &path, "scan", 1, 1);
                }
            }
            // `--header "Name: value"` (repeatable) -> a JSON object the web runner
            // reads into the browser context's extraHTTPHeaders (clearance / auth /
            // preview tokens). Set before the runner is spawned so it is inherited.
            if !header.is_empty() {
                let mut map = serde_json::Map::new();
                for h in &header {
                    if let Some((name, value)) = h.split_once(':') {
                        let name = name.trim();
                        if !name.is_empty() {
                            map.insert(name.to_string(), serde_json::Value::from(value.trim()));
                        }
                    } else {
                        return Err(anyhow::anyhow!(
                            "invalid --header {h:?}: expected \"Name: value\""
                        ));
                    }
                }
                std::env::set_var(
                    "REPROIT_EXTRA_HEADERS",
                    serde_json::Value::Object(map).to_string(),
                );
            }
            if let Some(path) = target_arg.as_deref().map(PathBuf::from) {
                if path.is_file() && backend_headless::looks_like_schema(&path) {
                    if record {
                        anyhow::bail!(
                            "backend streams produce a structural reproduction, so `scan \
                             --record` does not apply"
                        );
                    }
                    return backend_headless::run_target(&ctx, &path, "scan", 1, 1).await;
                }
            } else if let Some((path, config)) = configured_backend {
                if record {
                    anyhow::bail!(
                        "backend streams produce a structural reproduction, so `scan --record` \
                         does not apply"
                    );
                }
                return backend_headless::run_configured_target(&ctx, &path, "scan", 1, 1, config)
                    .await;
            }
            // Zero-config targets: a URL synthesizes a web config; a bare terminal
            // EXECUTABLE (when there's no project config) synthesizes a TUI/PTY
            // config. Both auto-build the map. Anything else scopes to an
            // alias/journey/node in a reproit.yaml (which wins over an executable
            // of the same name, so a project is never hijacked).
            let target_url = target_arg.as_deref().and_then(target_as_url);
            let mut synthesized = target_url.is_some();
            let loaded = if let Some(u) = &target_url {
                let wrd = config::ensure_web_runner_dir(VERSION, &|m| ctx.say(m))?;
                ctx.say(format!("zero-config web run against {u}"));
                let l = config::synthesize_web(u, &wrd, std::env::current_dir()?)?;
                ensure_app_map(&ctx, &l, "explore").await?;
                l
            } else {
                match config::load(cli.config.as_deref()) {
                    Ok(l) => {
                        ensure_app_map(&ctx, &l, "explore").await?;
                        l
                    }
                    Err(e) => match target_arg.as_deref().and_then(target_as_executable) {
                        Some(exe) => {
                            if !confirm_tui_fuzz(&ctx, &exe) {
                                return Ok(ExitCode::SUCCESS);
                            }
                            ctx.say(format!("zero-config TUI run against `{exe}`"));
                            let l = config::synthesize_tui(&exe, std::env::current_dir()?)?;
                            ensure_app_map(&ctx, &l, "explore").await?;
                            synthesized = true;
                            l
                        }
                        None => return Err(e),
                    },
                }
            };
            let journey = match &target_arg {
                Some(t) if !synthesized => t.clone(),
                _ => "explore".to_string(),
            };
            let args = fuzz::ScanArgs {
                journey,
                seed: 1,
                budget,
                sim,
                json: ctx.json,
                record,
                out,
            };
            let summary = fuzz::scan(&loaded.config, &loaded.root, &args).await?;
            // A cut-short crawl (timeout/killed) checked only some screens; exit
            // non-zero so CI/agents never read an incomplete scan as a clean pass.
            // Confirmed scan findings are also regressions, matching A2UI scan.
            Ok(if summary.complete && summary.issues == 0 {
                ExitCode::SUCCESS
            } else {
                exit_with(Exit::Regression)
            })
        }
        Cmd::Fuzz {
            journey,
            seed,
            runs,
            budget,
            shrink: _shrink,
            no_confirm,
            all,
            frontier,
            from,
            uniform,
            seeds,
            batch,
            profile_timing,
            sim,
            confirm_on_sim,
            cloud,
            app,
            bucket,
            post_comment,
            soak,
            cycle,
            repeats,
            warm,
            target,
            url,
            headless,
            locale,
            only,
            no_oracles,
            device,
            target_arg,
            service,
            reset,
        } => {
            if let Some(service) = service {
                std::env::set_var("REPROIT_BACKEND_URL", service);
            }
            if let Some(reset) = reset {
                std::env::set_var("REPROIT_BACKEND_RESET_URL", reset);
            }
            let configured_backend = if target_arg.is_none() {
                backend_config_target(cli.config.as_deref())?
            } else {
                None
            };
            if let Some(path) = target_arg.as_deref().map(PathBuf::from) {
                if path.is_file() && a2ui::looks_like_target(&path) {
                    return a2ui::run_target(&ctx, &path, "fuzz", seed, runs);
                }
                if path.is_file() && backend_headless::looks_like_schema(&path) {
                    return backend_headless::run_target(&ctx, &path, "fuzz", seed, runs).await;
                }
            } else if let Some((path, config)) = configured_backend {
                return backend_headless::run_configured_target(
                    &ctx, &path, "fuzz", seed, runs, config,
                )
                .await;
            }
            // The positional TARGET is auto-classified. A URL (https://app.com,
            // or a bare google.com / localhost:3000) points reproit at a deployed
            // app with no reproit.yaml: synthesize a web config rooted at the cwd
            // (so `.reproit/` lands here) and auto-build the map so fuzz has a
            // graph. A bare terminal EXECUTABLE (when there's no project config)
            // synthesizes a TUI/PTY config the same way. Anything else (e.g.
            // "login") scopes the hunt to that alias/node in a reproit.yaml.
            // A Playwright TEST file (`.spec.ts/.spec.js/.test.*`) is detected
            // first: reproit RUNS the test under trace, reads its action sequence,
            // and fuzzes OUTWARD from the deep state the test reached. The pitch:
            // "you wrote the test; reproit finds the bugs you didn't" -- the test's
            // own actions become the per-seed replay prefix (incl. login fills, so
            // auth comes for free), and its first page.goto becomes the start URL.
            let pw_test: Option<PathBuf> = target_arg.as_deref().and_then(|t| {
                let p = PathBuf::from(t);
                let is_test = pwfuzz::looks_like_playwright_test(t)
                    || (pwfuzz::has_pw_test_ext(t) && p.is_file());
                is_test.then_some(p)
            });
            let mut pw_capture: Option<pwfuzz::Capture> = None;
            let target_url = if pw_test.is_some() {
                None
            } else {
                target_arg.as_deref().and_then(target_as_url)
            };
            let mut synthesized = target_url.is_some() || pw_test.is_some();
            let loaded = if let Some(test_path) = &pw_test {
                let wrd = config::ensure_web_runner_dir(VERSION, &|m| ctx.say(m))?;
                let cap = pwfuzz::capture(&wrd, test_path, &|m| ctx.say(m))?;
                let base = cap.base_url.clone().or_else(|| cap.goto_url.clone());
                let Some(base) = base else {
                    return Err(anyhow::anyhow!(
                        "the test never called page.goto, so reproit has no app URL to fuzz. Add \
                         a `await page.goto(...)` to the test."
                    ));
                };
                ctx.say(format!(
                    "zero-config web run from Playwright test against {base}"
                ));
                let l = config::synthesize_web(&base, &wrd, std::env::current_dir()?)?;
                ensure_app_map(&ctx, &l, &journey).await?;
                pw_capture = Some(cap);
                l
            } else if let Some(u) = &target_url {
                let wrd = config::ensure_web_runner_dir(VERSION, &|m| ctx.say(m))?;
                ctx.say(format!("zero-config web run against {u}"));
                let l = config::synthesize_web(u, &wrd, std::env::current_dir()?)?;
                ensure_app_map(&ctx, &l, &journey).await?;
                l
            } else {
                match config::load(cli.config.as_deref()) {
                    Ok(l) => {
                        let map_journey = if target_arg.is_some() {
                            "explore"
                        } else {
                            &journey
                        };
                        ensure_app_map(&ctx, &l, map_journey).await?;
                        l
                    }
                    Err(e) => match target_arg.as_deref().and_then(target_as_executable) {
                        Some(exe) => {
                            if !confirm_tui_fuzz(&ctx, &exe) {
                                return Ok(ExitCode::SUCCESS);
                            }
                            ctx.say(format!("zero-config TUI run against `{exe}`"));
                            let l = config::synthesize_tui(&exe, std::env::current_dir()?)?;
                            ensure_app_map(&ctx, &l, &journey).await?;
                            synthesized = true;
                            l
                        }
                        None => return Err(e),
                    },
                }
            };
            // A non-URL, non-executable positional scopes the hunt to that alias/node.
            let journey = match &target_arg {
                Some(t) if !synthesized => t.clone(),
                _ => journey,
            };
            // `--soak`: the leak oracle.
            if soak {
                let cycle = cycle
                    .ok_or_else(|| anyhow::anyhow!("--soak needs --cycle \"tap:A;tap:B;...\""))?;
                let args = soak::SoakArgs {
                    journey,
                    cycle,
                    repeats,
                    warm,
                };
                let leak = soak::soak(&loaded.config, &loaded.root, &args).await?;
                return Ok(if leak {
                    exit_with(Exit::Regression)
                } else {
                    ExitCode::SUCCESS
                });
            }
            // The web-engine cross-engine env (URL + headless) travels to the
            // web runner via process env, set per engine inside `run_targets`.
            if is_web_engines(target.as_deref().unwrap_or("")) {
                let url = url.or_else(|| loaded.config.app.url.clone());
                if let Some(u) = url {
                    std::env::set_var("REPROIT_URL", u);
                }
                std::env::set_var("REPROIT_HEADLESS", if headless { "1" } else { "0" });
            }
            // Oracle filter (--only/--no) and locale list (--locale), shared by
            // every target run below.
            let (oracle_filter, unknown) =
                crosscut::OracleFilter::build(only.as_deref(), no_oracles.as_deref());
            for u in &unknown {
                ctx.say(format!("  warn: unknown oracle category `{u}` (ignored)"));
            }
            let locales = locale
                .as_deref()
                .map(crosscut::parse_locales)
                .unwrap_or_default();

            // A multi-actor `--from` is a verified shared-state checkpoint, not
            // a linear replay prefix. The journey conductor keeps its authored
            // business setup immutable while the multi-user fuzzer appends,
            // confirms, and shrinks seeded cross-actor schedules.
            if let Some(name) = from.as_deref() {
                if journey::is_multi_actor_target(&loaded, name)? {
                    if !locales.is_empty() {
                        return Err(anyhow::anyhow!(
                            "multi-user checkpoint fuzzing does not yet fan out `--locale`; put \
                             the desired locale in the app configuration"
                        ));
                    }
                    ctx.say(format!("fuzz: multi-user checkpoint `{name}`"));
                    let summary = journey::fuzz_multi_checkpoint(
                        &loaded,
                        name,
                        seed,
                        runs,
                        budget,
                        !no_confirm,
                    )
                    .await?;
                    ctx.say(format!(
                        "multi-user fuzz: {} confirmed bug(s), {} candidate(s)",
                        summary.confirmed, summary.candidates
                    ));
                    return Ok(if summary.confirmed > 0 {
                        exit_with(Exit::Regression)
                    } else {
                        ExitCode::SUCCESS
                    });
                }
            }

            // `--from <journey>`: resolve the journey to its replay actions
            // host-side now, so a bad/multi-actor journey fails before any drive
            // (and the secret/map resolution happens once, not per seed).
            //
            // A Playwright-test target produces the SAME kind of replay prefix from
            // the captured trace: its mapped actions become the per-seed prefix the
            // runner replays before exploring, and its start URL pins the runner's
            // gotoUrl so every seed lands on the same page the test reached.
            let from_prefix = if let Some(cap) = &pw_capture {
                pwfuzz::report(cap, pw_test.as_deref().unwrap_or(Path::new("test")), &|m| {
                    ctx.say(m)
                });
                // The start URL already rode into the synthesized config's app.url
                // (-> REPROIT_URL -> the runner's APP_URL/START_URL), so every seed
                // lands on the page the test reached before replaying the prefix.
                let prefix = cap.replay_prefix();
                if prefix.is_empty() {
                    ctx.say(
                        "  no replayable actions captured from the test; fuzzing from the start \
                         URL only.",
                    );
                    None
                } else {
                    Some(prefix)
                }
            } else {
                match &from {
                    Some(name) => Some(journey::prefix_actions(&loaded, name)?),
                    None => None,
                }
            };

            let args = fuzz::FuzzArgs {
                journey,
                seed,
                runs,
                budget,
                shrink: !no_confirm,
                all,
                frontier,
                uniform,
                seeds_file: seeds,
                batch,
                profile_timing,
                sim,
                confirm_on_sim,
                cloud,
                app,
                app_bucket: bucket,
                post_comment,
                json: ctx.json,
                locales,
                oracle_filter,
                from_prefix,
            };

            // UNIFIED --target dispatch: ONE path for web ENGINES
            // (chromium/firefox/webkit -> cross-engine differential) AND
            // PLATFORMS (ios|android|web|all -> per-device run). A single target
            // routes the run to its driver; a list runs EACH and diffs for
            // divergence (a finding on a subset of targets, not all).
            let run_targets_parsed = target.as_deref().map(crosscut::parse_run_targets);
            if let Some((targets, unknown_t)) = run_targets_parsed {
                for u in &unknown_t {
                    ctx.say(format!("  warn: unknown target `{u}` (ignored)"));
                }
                if !targets.is_empty() {
                    return run_targets(&ctx, &loaded, &targets, device.as_deref(), args).await;
                }
            } else if device.is_none()
                && !ctx.yes
                && std::io::IsTerminal::is_terminal(&std::io::stdin())
                && run_needs_device_pick(&loaded.config.app.platform, sim)
            {
                // No --target / --device on a TTY, AND the run needs a device.
                // The headless tier (flutter `flutter test`, web CDP) uses none,
                // so prompting there is vestigial; fall through to the headless
                // default run instead. Offer the interactive picker.
                if let Some(dev) = pick_device_interactive(
                    None,
                    &crosscut::platform_targets(&loaded.config.app.platform),
                )
                .await
                {
                    ctx.say(format!("  selected {} ({})", dev.name, dev.target.as_str()));
                    return run_targets(
                        &ctx,
                        &loaded,
                        &[crosscut::RunTarget::Platform(dev.target)],
                        Some(&dev.id),
                        args,
                    )
                    .await;
                }
            }

            let fuzz_summary = fuzz::fuzz(&loaded.config, &loaded.root, &args).await?;
            // --json: surface the findings artifact (the discovered repro, by
            // content-hash id, plus its seed/actions) so the agent/MCP bridge
            // can keep it without re-parsing the human report.
            if ctx.json {
                match latest_finding(&loaded) {
                    Some(f) => ctx.emit(&serde_json::json!({
                        "command": "fuzz",
                        "complete": fuzz_summary.complete,
                        "seeds_run": fuzz_summary.seeds_run,
                        "seeds_requested": fuzz_summary.seeds_requested,
                        "found": true,
                        "id": repro::display_finding_id(&f.id()),
                        "kind": "finding",
                        "seed": f.seed,
                        "actions": f.actions,
                        "artifact": f.run_dir.to_string_lossy(),
                    })),
                    None => ctx.emit(&serde_json::json!({
                        "command": "fuzz",
                        "complete": fuzz_summary.complete,
                        "seeds_run": fuzz_summary.seeds_run,
                        "seeds_requested": fuzz_summary.seeds_requested,
                        "found": false,
                    })),
                }
            }
            Ok(if fuzz_summary.complete {
                ExitCode::SUCCESS
            } else {
                exit_with(Exit::Regression)
            })
        }
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
                discover_and_verify_login(cli.config.as_deref(), &account).await?;
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
                        no_discover,
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
                .map(crosscut::parse_locales)
                .unwrap_or_default();
            let (targets, unknown) = match target.as_deref() {
                Some(t) => crosscut::parse_run_targets(t),
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
            #[cfg(target_os = "linux")]
            {
                atspi::run()?;
                Ok(ExitCode::SUCCESS)
            }
            #[cfg(not(target_os = "linux"))]
            {
                anyhow::bail!("__atspi (Linux AT-SPI) is unsupported on this platform")
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
         Appium covers native mobile, TUI uses a PTY, and only\nimmediate-mode GUIs (imgui, clay) \
         need an in-app hook."
    );
}
fn backend_config_target(
    config_path: Option<&Path>,
) -> Result<Option<(PathBuf, backend::BackendConfig)>> {
    let path = match config_path {
        Some(path) if path.is_file() => path.to_path_buf(),
        Some(path) => anyhow::bail!("config file {} does not exist", path.display()),
        None => {
            let mut directory = std::env::current_dir()?;
            loop {
                let candidate = directory.join("reproit.yaml");
                if candidate.is_file() {
                    break candidate;
                }
                if !directory.pop() {
                    return Ok(None);
                }
            }
        }
    };
    let document: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&path)?)?;
    if document.get("app").is_some() {
        return Ok(None);
    }
    let Some(backend) = document.get("backend") else {
        return Ok(None);
    };
    let config: backend::BackendConfig = serde_yaml::from_value(backend.clone())?;
    if !config.enabled {
        return Ok(None);
    }
    let schema = config
        .schemas
        .first()
        .context("backend.enabled is true but backend.schemas is empty")?;
    let target = path.parent().unwrap_or_else(|| Path::new(".")).join(schema);
    if !target.is_file() {
        anyhow::bail!("backend schema {} does not exist", target.display());
    }
    Ok(Some((target, config)))
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
mod tests {
    use super::*;
    use crate::model::repro;

    #[test]
    fn simplify_preserves_the_property_matched_fixture() {
        // A data-dependent repro carries inputs + locale. Simplifying it minimizes
        // the ACTIONS but must keep the data, or the adopted repro stops
        // reproducing the bug that only fires for that fixture.
        let src = serde_json::json!({
            "seed": 7u64,
            "replay": ["tap:a", "tap:b", "tap:c"],
            "inputs": [{ "field": "name", "value": "a-long-unicode-name" }],
            "locale": "tr",
        });
        let out = build_simplified_replay(7, &["tap:c".to_string()], &src);
        assert_eq!(out["replay"], serde_json::json!(["tap:c"]));
        assert_eq!(out["locale"], "tr");
        assert_eq!(out["inputs"], src["inputs"]);

        // A path-only repro (no fixture) stays the bare {seed, replay} shape.
        let bare = build_simplified_replay(
            7,
            &["tap:c".to_string()],
            &serde_json::json!({ "seed": 7u64, "replay": ["tap:a", "tap:c"] }),
        );
        assert!(bare.get("inputs").is_none());
        assert!(bare.get("locale").is_none());
        assert_eq!(bare["replay"], serde_json::json!(["tap:c"]));
    }

    #[test]
    fn parse_fuzz_report_extracts_seed_and_repro_actions() {
        // The exact shape modes/fuzz.rs::write_report emits.
        let md = "\
# fuzz finding (seed 42)

## invariants violated

- **no-exception** (1)

## findings

- `no-exception` **EXCEPTION CAUGHT BY WIDGETS LIBRARY**: boom

## confirmed repro (2 actions, shrunk from 7)

```
tap:Login
tap:Submit
```

Replay: write {\"replay\": [...]} to .reproit/tmp/fuzz_config.json ...
";
        let (seed, actions) = parse_fuzz_report(md).expect("parse");
        assert_eq!(seed, 42);
        assert_eq!(actions, vec!["tap:Login", "tap:Submit"]);
        // The id is what `keep` would store under.
        assert_eq!(
            repro::repro_id(seed, &actions),
            repro::repro_id(42, &["tap:Login", "tap:Submit"])
        );
    }

    #[test]
    fn pending_meta_lets_a_finding_be_checked_before_keep() {
        // A finding not yet kept: its in-memory Meta carries the same content-hash
        // id keep would store under, is quarantined, has no alias/created stamp,
        // and triggers at the end of its own minimized sequence.
        let f = Finding {
            id: "abcdef123456".into(),
            seed: 42,
            actions: vec!["tap:Login".into(), "tap:Submit".into()],
            run_dir: std::path::PathBuf::from("/tmp/nonexistent-run"),
        };
        let m = f.pending_meta();
        assert_eq!(m.id, "abcdef123456");
        assert_eq!(m.id, f.id());
        assert_eq!(m.status, repro::Status::Quarantined);
        assert_eq!(m.seed, 42);
        assert!(m.alias.is_none());
        assert!(m.created.is_empty());
        assert!(m.last_checked.is_none());
        assert_eq!(m.trigger_index, Some(2));
    }

    #[test]
    fn public_and_internal_finding_ids_resolve_to_pending_artifact() {
        let root = std::env::temp_dir().join(format!("reproit-fnd-{}", std::process::id()));
        let run = root.join(".reproit/runs/run-1");
        std::fs::create_dir_all(&run).unwrap();
        let md = "\
# fuzz finding (seed 42)

## confirmed repro (2 actions)

```
tap:Login
tap:Submit
```
";
        std::fs::write(run.join("fuzz.md"), md).unwrap();
        let loaded = config::parse_str(
            "app:\n  platform: web\n  webRunnerDir: ./runners/web\n  url: http://localhost:3000\n\
             devices:\n  namePrefix: reproit\n\
             journeys:\n  dir: journeys\n  driver: explore\n  doneMarkers: [DONE]\n\
             evidence:\n  outDir: .reproit/runs\n  video: false\n",
            root.clone(),
        )
        .unwrap();
        let raw = repro::repro_id(42, &["tap:Login", "tap:Submit"]);
        assert!(find_finding_by_id(&loaded, &raw).is_some());
        assert!(find_finding_by_id(&loaded, &repro::display_finding_id(&raw)).is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finding_id_resolves_from_durable_store_after_evidence_moves() {
        let root = std::env::temp_dir().join(format!("reproit-durable-fnd-{}", std::process::id()));
        let raw = repro::repro_id(77, &["tap:key:save"]);
        let durable = root.join(".reproit/findings").join(&raw);
        std::fs::create_dir_all(&durable).unwrap();
        std::fs::write(
            durable.join("fuzz.md"),
            "# fuzz finding (seed 77)\n\n## confirmed repro (1 \
             actions)\n\n```\ntap:key:save\n```\n",
        )
        .unwrap();
        let loaded = config::parse_str(
            "app:\n  platform: web\n  webRunnerDir: ./runners/web\n  url: http://localhost:3000\n\
             devices:\n  namePrefix: reproit\n\
             journeys:\n  dir: journeys\n  driver: explore\n  doneMarkers: [DONE]\n\
             evidence:\n  outDir: moved/evidence\n  video: false\n",
            root.clone(),
        )
        .unwrap();
        let found = find_finding_by_id(&loaded, &repro::display_finding_id(&raw)).unwrap();
        assert_eq!(found.id(), raw);
        assert_eq!(found.run_dir, durable);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parse_fuzz_oracle_reads_occlusion_block() {
        // The `## oracle` block carries the category and violating state sig.
        let md = "\
# fuzz finding (seed 9)

## invariants violated

- **no-occluded-control** (1)

## oracle

- oracle: `occlusion`
- invariant: `no-occluded-control`
- sig: `advanced`

## findings

- `no-occluded-control` **OCCLUSION**: state advanced has an occluded control

## confirmed repro (1 actions)

```
tap:Advanced
```
";
        let (oracle, sig, selector) = parse_fuzz_oracle(md);
        assert_eq!(oracle.as_deref(), Some("occlusion"));
        assert_eq!(sig.as_deref(), Some("advanced"));
        assert_eq!(selector, None);
    }

    #[test]
    fn parse_fuzz_oracle_crash_block_has_no_sig() {
        let md = "\
# fuzz finding (seed 1)

## oracle

- oracle: `crash`
- invariant: `no-exception`
- sig: ``

## findings
";
        let (oracle, sig, selector) = parse_fuzz_oracle(md);
        assert_eq!(oracle.as_deref(), Some("crash"));
        assert_eq!(sig, None);
        assert_eq!(selector, None);
    }

    #[test]
    fn parse_fuzz_oracle_absent_block_is_none() {
        // An older report with no `## oracle` block -> fall back to crash path.
        let md = "# fuzz finding (seed 1)\n\n## findings\n";
        assert_eq!(parse_fuzz_oracle(md), (None, None, None));
    }

    #[test]
    fn state_present_recording_navigates_directly_without_replay() {
        let log = r#"EXPLORE:STATE {"sig":"docs","route":"/docs/search","labels":[]}"#;
        let (url, action) = web_record_metadata(
            Some("https://example.test/start"),
            Some("zoom-reflow"),
            Some("docs"),
            log,
        );
        assert_eq!(url.as_deref(), Some("https://example.test/docs/search"));
        assert_eq!(action, None);
    }

    #[test]
    fn flicker_recording_keeps_only_the_triggering_action() {
        let log = concat!(
            "EXPLORE:STATE {\"sig\":\"header\",\"route\":\"/pricing\",\"labels\":[]}\n",
            "EXPLORE:RERENDER \
             {\"from\":\"header\",\"action\":\"tap:key:menu\",\"churned\":[\"nav\"]}\n"
        );
        let (url, action) = web_record_metadata(
            Some("https://example.test/"),
            Some("flicker"),
            Some("header"),
            log,
        );
        assert_eq!(url.as_deref(), Some("https://example.test/pricing"));
        assert_eq!(action.as_deref(), Some("tap:key:menu"));
    }

    #[test]
    fn legacy_recording_preserves_full_replay() {
        let meta: repro::Meta = serde_json::from_value(serde_json::json!({
            "id": "abc", "status": "quarantined", "seed": 1,
            "created": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        let mut replay = serde_json::json!({"seed": 1, "replay": ["tap:A", "tap:B"]});
        minimize_record_replay(&mut replay, &meta);
        assert_eq!(replay["replay"], serde_json::json!(["tap:A", "tap:B"]));
        assert!(replay.get("gotoUrl").is_none());
    }

    #[test]
    fn direct_recording_replaces_discovery_walk() {
        let mut meta: repro::Meta = serde_json::from_value(serde_json::json!({
            "id": "abc", "status": "quarantined", "seed": 1,
            "created": "2026-01-01T00:00:00Z",
            "record_url": "https://example.test/pricing",
            "record_action": "tap:key:menu"
        }))
        .unwrap();
        let mut replay = serde_json::json!({"seed": 1, "replay": ["tap:A", "tap:B"]});
        minimize_record_replay(&mut replay, &meta);
        assert_eq!(replay["replay"], serde_json::json!(["tap:key:menu"]));
        assert_eq!(replay["gotoUrl"], "https://example.test/pricing");
        meta.record_action = None;
        minimize_record_replay(&mut replay, &meta);
        assert_eq!(replay["replay"], serde_json::json!([]));
    }

    #[test]
    fn parse_fuzz_report_handles_empty_repro_block() {
        let md = "# fuzz finding (seed 5)\n\n## confirmed repro (0 actions)\n\n```\n```\n";
        let (seed, actions) = parse_fuzz_report(md).expect("parse");
        assert_eq!(seed, 5);
        assert!(actions.is_empty());
    }

    #[test]
    fn parse_fuzz_finding_id_accepts_scoped_marker_and_rejects_invalid_ids() {
        assert_eq!(
            parse_fuzz_finding_id("# fuzz finding (seed 0)\n\n<!-- finding-id: abcdef123456 -->"),
            Some("abcdef123456".to_string())
        );
        assert_eq!(
            parse_fuzz_finding_id("<!-- finding-id: not-an-id -->"),
            None
        );
        assert_eq!(parse_fuzz_finding_id("# legacy fuzz report"), None);
    }

    #[test]
    fn parse_fuzz_report_without_seed_is_none() {
        assert!(parse_fuzz_report("# not a finding\n\nblah\n").is_none());
    }

    #[test]
    fn web_engine_targets_route_to_the_cross_engine_path() {
        // A list of only engine names routes to the cross-engine differential.
        assert!(is_web_engines("chromium,firefox,webkit"));
        assert!(is_web_engines("chrome,safari"));
        // A bare `web` (or any platform token) is NOT the engine path: it is a
        // platform run. ios/android likewise route to the platform path.
        assert!(!is_web_engines("web"));
        assert!(!is_web_engines("ios,android"));
        // Mixed engine+platform is NOT all-engine -> platform path.
        assert!(!is_web_engines("chromium,ios"));
        assert!(!is_web_engines(""));
    }

    #[test]
    fn only_flutter_sim_runs_offer_the_device_picker() {
        // Only FlutterDrive provisions a sim reproit picks, and only with --sim
        // (its default is the headless flutter test tier).
        assert!(run_needs_device_pick("flutter", true));
        assert!(!run_needs_device_pick("flutter", false));
        // Every other backend brings its own target (Appium caps, a browser, the
        // host, a PTY), so no reproit picker, even with --sim.
        for p in [
            "web",
            "react-native",
            "swift-ios",
            "android",
            "winui",
            "electron",
            "tauri",
        ] {
            assert!(!run_needs_device_pick(p, false), "{p} should not prompt");
            assert!(
                !run_needs_device_pick(p, true),
                "{p} should not prompt even with --sim"
            );
        }
        // Unknown platform: no prompt.
        assert!(!run_needs_device_pick("cobol-tui", false));
    }

    #[test]
    fn account_login_selects_one_project_and_resolves_names() {
        let projects = vec![
            triage::CloudProject {
                name: "Store".into(),
                app_id: "store-1".into(),
            },
            triage::CloudProject {
                name: "Docs".into(),
                app_id: "docs-2".into(),
            },
        ];
        assert_eq!(
            choose_cloud_project(&projects[..1], None, false)
                .unwrap()
                .as_deref(),
            Some("store-1")
        );
        assert_eq!(
            choose_cloud_project(&projects, Some("Docs"), false)
                .unwrap()
                .as_deref(),
            Some("docs-2")
        );
        assert!(choose_cloud_project(&projects, Some("missing"), false).is_err());
    }

    #[test]
    fn scoped_env_restores_prior_value_and_removes_unset_keys() {
        // ScopedEnv is what guarantees a per-target REPROIT_* never leaks into
        // the next target (Task 1) AND the same Drop pattern underpins the
        // crash-reporter restore (Task 2). Use unique keys to avoid clobbering
        // anything real in the test process.
        let set_key = "REPROIT_TEST_SCOPED_SET";
        let unset_key = "REPROIT_TEST_SCOPED_UNSET";
        std::env::set_var(set_key, "original");
        std::env::remove_var(unset_key);
        {
            let _guard = ScopedEnv::set(vec![
                (set_key.to_string(), "during".to_string()),
                (unset_key.to_string(), "during".to_string()),
            ]);
            assert_eq!(std::env::var(set_key).as_deref(), Ok("during"));
            assert_eq!(std::env::var(unset_key).as_deref(), Ok("during"));
        }
        // After drop: the previously-set key is restored to its old value, and
        // the previously-unset key is removed entirely.
        assert_eq!(std::env::var(set_key).as_deref(), Ok("original"));
        assert!(std::env::var(unset_key).is_err());
        std::env::remove_var(set_key);
    }
}
