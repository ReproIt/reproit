//! Fuzz command coordination across schemas, apps, journeys, devices, and locales.

use super::device::{is_web_engines, pick_device_interactive, run_needs_device_pick, run_targets};
use super::map::ensure_app_map;
use super::{backend_target, confirm_tui_fuzz};
use crate::adapters::config;
use crate::interface::cli::args::FuzzArgs;
use crate::interface::cli::context::{exit_with, Ctx, Exit};
use crate::interface::cli::target::{target_as_executable, target_as_url};
use crate::workflows::{a2ui, backend_headless, fuzz, journey, pwfuzz, soak};
use crate::VERSION;
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
        platform,
    } = args;
    if let Some(service) = service {
        std::env::set_var("REPROIT_BACKEND_URL", service);
    }
    if let Some(reset) = reset {
        std::env::set_var("REPROIT_BACKEND_RESET_URL", reset);
    }
    let force_web = match platform.as_deref() {
        None | Some("web") | Some("backend") => platform.as_deref() == Some("web"),
        Some(other) => anyhow::bail!("--platform for fuzz is web or backend, got {other:?}"),
    };
    // On the backend path, a URL-valued --target is the backend service base
    // URL (for app platforms it stays the engine/platform list).
    let target_flag_url = target.as_deref().and_then(target_as_url);
    if let Some(path) = target_arg.as_deref().map(PathBuf::from) {
        if path.is_file() && a2ui::looks_like_target(&path) {
            return a2ui::run_target(ctx, &path, "fuzz", seed, runs);
        }
        if path.is_file() && backend_headless::looks_like_schema(&path) {
            backend_target::apply_target_precedence(target_flag_url.as_deref(), None)?;
            return backend_headless::run_target(ctx, &path, "fuzz", seed, runs).await;
        }
    }
    let configured_backend = if force_web {
        None
    } else {
        backend_target::resolve(config_path)?
    };
    let route = backend_target::route_positional(
        configured_backend.is_some(),
        force_web,
        target_arg.as_deref(),
    );
    if let backend_target::BackendRoute::Backend(positional_url) = route {
        let (path, config) = configured_backend.expect("backend route implies a backend config");
        let flag = target_flag_url.or(positional_url);
        backend_target::apply_target_precedence(flag.as_deref(), config.target.as_deref())?;
        return backend_headless::run_configured_target(ctx, &path, "fuzz", seed, runs, config)
            .await;
    }
    if platform.as_deref() == Some("backend") {
        anyhow::bail!(
            "--platform backend needs a backend reproit.yaml (backend.enabled with a schema); \
             create one with `reproit init <schema url or file>`"
        );
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
        crate::domain::oracle::OracleFilter::build(only.as_deref(), no_oracles.as_deref());
    for u in &unknown {
        ctx.say(format!("  warn: unknown oracle category `{u}` (ignored)"));
    }
    let locales = locale
        .as_deref()
        .map(crate::domain::locale::parse_locales)
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
    let run_targets_parsed = target
        .as_deref()
        .map(crate::domain::target::parse_run_targets);
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
            &crate::domain::target::platform_targets(&loaded.config.app.platform),
        )
        .await
        {
            ctx.say(format!("  selected {} ({})", dev.name, dev.target.as_str()));
            return run_targets(
                ctx,
                &loaded,
                &[crate::domain::target::RunTarget::Platform(dev.target)],
                Some(&dev.id),
                args,
            )
            .await;
        }
    }

    let fuzz_summary = fuzz::fuzz(&loaded.config, &loaded.root, &args).await?;
    // Surface the discovered artifact and every independently confirmed
    // finding. Per-finding causes avoid inventing one aggregate cause for a run
    // that found multiple bugs.
    if ctx.json {
        ctx.emit(&fuzz_json(&fuzz_summary));
    }
    Ok(if fuzz_summary.complete {
        ExitCode::SUCCESS
    } else {
        exit_with(Exit::Regression)
    })
}

fn fuzz_json(summary: &fuzz::FuzzSummary) -> serde_json::Value {
    let findings = summary
        .confirmed_findings
        .iter()
        .map(|finding| {
            serde_json::json!({
                "id": finding.id,
                "cause": finding.cause.as_str(),
                "causalHttpRequest": if matches!(
                    finding.cause,
                    crate::domain::capsule::CauseCategory::HttpTransaction
                ) {
                    "captured"
                } else {
                    "not applicable"
                },
                "actionCount": finding.action_count,
                "minimized": true,
                "findingArtifactSaved": true,
                "regressionGuardKept": false,
                "artifact": finding.artifact.to_string_lossy(),
            })
        })
        .collect::<Vec<_>>();
    let evidence_status = summary.evidence.status(summary.complete);
    let mut output = serde_json::json!({
        "command": "fuzz",
        "complete": summary.complete,
        "seeds_run": summary.seeds_run,
        "seeds_requested": summary.seeds_requested,
        "found": !findings.is_empty(),
        "confirmed": !findings.is_empty(),
        "confirmedFindings": findings.len(),
        "findings": findings,
        "evidenceStatus": evidence_status,
        "evidence": summary.evidence,
    });
    // Preserve the convenient top-level single-finding fields, but derive them
    // only from this run's summary. Looking in the historical evidence store
    // here can leak an old finding into an otherwise clean run.
    if let Some(finding) = summary.confirmed_findings.last() {
        let object = output.as_object_mut().expect("fuzz output is an object");
        object.insert("id".into(), serde_json::Value::String(finding.id.clone()));
        object.insert("kind".into(), serde_json::Value::String("finding".into()));
        object.insert("seed".into(), serde_json::Value::from(finding.seed));
        object.insert(
            "actions".into(),
            serde_json::to_value(&finding.actions).expect("actions serialize"),
        );
        object.insert(
            "artifact".into(),
            serde_json::Value::String(finding.artifact.to_string_lossy().into_owned()),
        );
        object.insert("findingArtifactSaved".into(), serde_json::Value::Bool(true));
        object.insert("regressionGuardKept".into(), serde_json::Value::Bool(false));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::capsule::CauseCategory;

    #[test]
    fn fuzz_json_keeps_cause_and_guard_state_per_finding() {
        let summary = fuzz::FuzzSummary {
            complete: true,
            seeds_run: 2,
            seeds_requested: 2,
            confirmed_findings: vec![
                fuzz::ConfirmedFinding {
                    id: "fnd_launch000001".into(),
                    cause: CauseCategory::ApplicationLaunch,
                    action_count: 0,
                    seed: 1,
                    actions: Vec::new(),
                    artifact: PathBuf::from(".reproit/findings/launch000001"),
                },
                fuzz::ConfirmedFinding {
                    id: "fnd_http0000002".into(),
                    cause: CauseCategory::HttpTransaction,
                    action_count: 3,
                    seed: 2,
                    actions: vec!["tap:key:testid:submit".into()],
                    artifact: PathBuf::from(".reproit/findings/http0000002"),
                },
            ],
            ..Default::default()
        };
        let output = fuzz_json(&summary);
        assert_eq!(output["confirmedFindings"], 2);
        assert_eq!(output["findings"][0]["cause"], "application launch");
        assert_eq!(output["findings"][1]["cause"], "HTTP transaction");
        assert_eq!(output["findings"][1]["causalHttpRequest"], "captured");
        assert_eq!(output["findings"][0]["regressionGuardKept"], false);
        assert!(output.get("cause").is_none());
        assert!(output.get("regressionSaved").is_none());
        assert_eq!(output["id"], "fnd_http0000002");
        assert_eq!(output["seed"], 2);
    }

    #[test]
    fn fuzz_json_never_reports_a_finding_outside_the_current_summary() {
        let output = fuzz_json(&fuzz::FuzzSummary {
            complete: true,
            seeds_run: 1,
            seeds_requested: 1,
            ..Default::default()
        });
        assert_eq!(output["found"], false);
        assert_eq!(output["confirmedFindings"], 0);
        assert!(output.get("id").is_none());
        assert!(output.get("artifact").is_none());
    }
}
