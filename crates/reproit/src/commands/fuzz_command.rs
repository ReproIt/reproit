//! Fuzz command coordination across schemas, apps, journeys, devices, and locales.

use super::device::{is_web_engines, pick_device_interactive, run_needs_device_pick, run_targets};
use super::map::ensure_app_map;
use super::repro::latest_finding;
use super::{backend_target, confirm_tui_fuzz};
use crate::cli::args::FuzzArgs;
use crate::cli::context::{exit_with, Ctx, Exit};
use crate::cli::target::{target_as_executable, target_as_url};
use crate::model::repro;
use crate::modes::{a2ui, backend_headless, fuzz, journey, pwfuzz, soak};
use crate::{config, crosscut, VERSION};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(super) async fn run(ctx: &Ctx, config_path: Option<&Path>, args: FuzzArgs) -> Result<ExitCode> {
    let FuzzArgs {
        target_arg,
        service,
        reset,
        journey,
        seed,
        runs,
        budget,
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
    } = args;
    if let Some(service) = service {
        std::env::set_var("REPROIT_BACKEND_URL", service);
    }
    if let Some(reset) = reset {
        std::env::set_var("REPROIT_BACKEND_RESET_URL", reset);
    }
    let configured_backend = if target_arg.is_none() {
        backend_target::resolve(config_path)?
    } else {
        None
    };
    if let Some(path) = target_arg.as_deref().map(PathBuf::from) {
        if path.is_file() && a2ui::looks_like_target(&path) {
            return a2ui::run_target(ctx, &path, "fuzz", seed, runs);
        }
        if path.is_file() && backend_headless::looks_like_schema(&path) {
            return backend_headless::run_target(ctx, &path, "fuzz", seed, runs).await;
        }
    } else if let Some((path, config)) = configured_backend {
        return backend_headless::run_configured_target(ctx, &path, "fuzz", seed, runs, config)
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
        let is_test =
            pwfuzz::looks_like_playwright_test(t) || (pwfuzz::has_pw_test_ext(t) && p.is_file());
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
        ensure_app_map(ctx, &l, &journey).await?;
        pw_capture = Some(cap);
        l
    } else if let Some(u) = &target_url {
        let wrd = config::ensure_web_runner_dir(VERSION, &|m| ctx.say(m))?;
        ctx.say(format!("zero-config web run against {u}"));
        let l = config::synthesize_web(u, &wrd, std::env::current_dir()?)?;
        ensure_app_map(ctx, &l, &journey).await?;
        l
    } else {
        match config::load(config_path) {
            Ok(l) => {
                let map_journey = if target_arg.is_some() {
                    "explore"
                } else {
                    &journey
                };
                ensure_app_map(ctx, &l, map_journey).await?;
                l
            }
            Err(e) => match target_arg.as_deref().and_then(target_as_executable) {
                Some(exe) => {
                    if !confirm_tui_fuzz(ctx, &exe) {
                        return Ok(ExitCode::SUCCESS);
                    }
                    ctx.say(format!("zero-config TUI run against `{exe}`"));
                    let l = config::synthesize_tui(&exe, std::env::current_dir()?)?;
                    ensure_app_map(ctx, &l, &journey).await?;
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
        let cycle =
            cycle.ok_or_else(|| anyhow::anyhow!("--soak needs --cycle \"tap:A;tap:B;...\""))?;
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
            let summary =
                journey::fuzz_multi_checkpoint(&loaded, name, seed, runs, budget, !no_confirm)
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
            return run_targets(ctx, &loaded, &targets, device.as_deref(), args).await;
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
                ctx,
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
        let evidence_status = fuzz_summary.evidence.status(fuzz_summary.complete);
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
                "evidenceStatus": evidence_status,
                "evidence": fuzz_summary.evidence,
            })),
            None => ctx.emit(&serde_json::json!({
                "command": "fuzz",
                "complete": fuzz_summary.complete,
                "seeds_run": fuzz_summary.seeds_run,
                "seeds_requested": fuzz_summary.seeds_requested,
                "found": false,
                "evidenceStatus": evidence_status,
                "evidence": fuzz_summary.evidence,
            })),
        }
    }
    Ok(if fuzz_summary.complete {
        ExitCode::SUCCESS
    } else {
        exit_with(Exit::Regression)
    })
}
