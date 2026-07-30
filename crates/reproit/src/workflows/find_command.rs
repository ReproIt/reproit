//! Outcome-oriented discovery over the existing scan and fuzz engines.
//!
//! For a backend project the engine split is decided from the scaffold, not by
//! the user: scan evaluates only safe (read-only) operations by design, so a
//! schema with mutations routes through fuzz as well, and a read-only schema
//! never pays for a fuzz pass. `--quick`/`--deep` (and `--only`/`--no`) stay
//! as the expert door. When nothing names a live target, find boots the
//! service itself with the same machinery bare `reproit init` uses and tears
//! it down afterwards.

use super::backend_learn::boot;
use super::{backend_headless, backend_target, fuzz_command, scan_command};
use crate::interface::cli::args::{FindArgs, FuzzArgs, ScanArgs};
use crate::interface::cli::context::Ctx;
use anyhow::Result;
use std::path::Path;
use std::process::ExitCode;

const DEFAULT_SCAN_BUDGET: u32 = 60;
const DEFAULT_FUZZ_BUDGET: u32 = 40;
const EXHAUSTIVE_BUDGET: u32 = 120;
const DEFAULT_RUNS: u32 = 3;
const EXHAUSTIVE_RUNS: u32 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FindMode {
    Quick,
    Staged,
    Deep,
}

pub(super) async fn run(ctx: &Ctx, config_path: Option<&Path>, args: FindArgs) -> Result<ExitCode> {
    let mode = mode(&args);
    // A configured backend project with no positional target: the scaffold
    // decides the engines and, when needed, find boots the service itself.
    if args.target.is_none() && args.platform.as_deref() != Some("web") {
        if let Some(project) = backend_target::find(config_path)? {
            return run_backend(ctx, config_path, &args, mode, &project).await;
        }
    }
    let scan_args = build_scan_args(&args);
    let fuzz_args = build_fuzz_args(&args);

    if mode == FindMode::Quick {
        return scan_command::run(ctx, config_path, scan_args).await;
    }
    if mode == FindMode::Deep {
        return fuzz_command::run(ctx, config_path, fuzz_args).await;
    }

    ctx.say("find: fast surface pass");
    let scan_exit = scan_command::run(ctx, config_path, scan_args).await?;
    ctx.say("find: deep interaction pass");
    let fuzz_exit = fuzz_command::run(ctx, config_path, fuzz_args).await?;
    if fuzz_exit == ExitCode::SUCCESS {
        Ok(scan_exit)
    } else {
        Ok(fuzz_exit)
    }
}

async fn run_backend(
    ctx: &Ctx,
    config_path: Option<&Path>,
    args: &FindArgs,
    mode: FindMode,
    project: &backend_target::BackendProject,
) -> Result<ExitCode> {
    let schemas = project.schema_paths()?;
    let surface = backend_headless::schema_surface(&schemas)?;
    let (run_scan, run_fuzz) = backend_engines(mode, surface.read_only, surface.mutating);
    let booted = backend_target::ensure_live_target(
        ctx,
        &project.root,
        args.service.as_deref(),
        project.config.target.as_deref(),
        &schemas,
    )
    .await?;
    let result = run_backend_engines(ctx, config_path, args, run_scan, run_fuzz).await;
    if booted {
        boot::shutdown_process_reset().await;
    }
    result
}

/// Which engines a backend find runs, from the scaffold's operation mix.
/// Scan evaluates only safe operations; mutations need fuzz. The user never
/// picks: safe-only schemas skip the fuzz pass, mutation-only schemas skip the
/// scan pass, mixed schemas run both. Quick/deep remain explicit overrides.
fn backend_engines(mode: FindMode, read_only: usize, mutating: usize) -> (bool, bool) {
    match mode {
        FindMode::Quick => (true, false),
        FindMode::Deep => (false, true),
        FindMode::Staged => (read_only > 0 || mutating == 0, mutating > 0),
    }
}

async fn run_backend_engines(
    ctx: &Ctx,
    config_path: Option<&Path>,
    args: &FindArgs,
    run_scan: bool,
    run_fuzz: bool,
) -> Result<ExitCode> {
    let staged = run_scan && run_fuzz;
    let mut exit = ExitCode::SUCCESS;
    if run_scan {
        if staged {
            ctx.say("find: fast surface pass");
        }
        let scan_exit = scan_command::run(ctx, config_path, build_scan_args(args)).await?;
        if scan_exit != ExitCode::SUCCESS {
            exit = scan_exit;
        }
    }
    if run_fuzz {
        if staged {
            ctx.say("find: deep interaction pass");
        }
        let fuzz_exit = fuzz_command::run(ctx, config_path, build_fuzz_args(args)).await?;
        if fuzz_exit != ExitCode::SUCCESS {
            exit = fuzz_exit;
        }
    }
    Ok(exit)
}

fn mode(args: &FindArgs) -> FindMode {
    if args.quick {
        FindMode::Quick
    } else if args.deep {
        FindMode::Deep
    } else {
        FindMode::Staged
    }
}

fn build_scan_args(args: &FindArgs) -> ScanArgs {
    ScanArgs {
        target: args.target.clone(),
        service: args.service.clone(),
        target_url: None,
        platform: args.platform.clone(),
        budget: args.budget.unwrap_or(if args.exhaustive {
            EXHAUSTIVE_BUDGET
        } else {
            DEFAULT_SCAN_BUDGET
        }),
        sim: false,
        record_video: args.record_video,
        out: None,
        headers: args.headers.clone(),
        only: None,
    }
}

fn build_fuzz_args(args: &FindArgs) -> FuzzArgs {
    FuzzArgs {
        target_arg: args.target.clone(),
        service: args.service.clone(),
        reset: args.reset.clone(),
        journey: "explore".to_string(),
        seed: 1,
        runs: args.runs.unwrap_or(if args.exhaustive {
            EXHAUSTIVE_RUNS
        } else {
            DEFAULT_RUNS
        }),
        budget: args.budget.unwrap_or(if args.exhaustive {
            EXHAUSTIVE_BUDGET
        } else {
            DEFAULT_FUZZ_BUDGET
        }),
        no_confirm: false,
        all: true,
        frontier: false,
        from: None,
        uniform: false,
        seeds: None,
        batch: 0,
        profile_timing: false,
        sim: false,
        confirm_on_sim: false,
        cloud: None,
        app: None,
        bucket: None,
        post_comment: false,
        soak: false,
        cycle: None,
        repeats: 15,
        warm: false,
        target: None,
        platform: args.platform.clone(),
        url: None,
        headless: false,
        locale: None,
        only: args.only.clone(),
        no_oracles: args.no_oracles.clone(),
        device: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> FindArgs {
        FindArgs {
            target: None,
            quick: false,
            deep: false,
            exhaustive: false,
            runs: None,
            budget: None,
            service: None,
            platform: None,
            reset: None,
            record_video: false,
            headers: Vec::new(),
            only: None,
            no_oracles: None,
        }
    }

    #[test]
    fn default_is_staged_and_keeps_each_engine_bounded() {
        let args = args();
        assert_eq!(mode(&args), FindMode::Staged);
        assert_eq!(build_scan_args(&args).budget, DEFAULT_SCAN_BUDGET);
        let fuzz = build_fuzz_args(&args);
        assert_eq!(fuzz.runs, DEFAULT_RUNS);
        assert_eq!(fuzz.budget, DEFAULT_FUZZ_BUDGET);
        assert!(fuzz.all);
        assert!(!fuzz.no_confirm);
    }

    #[test]
    fn exhaustive_mode_has_an_explicit_larger_bound() {
        let mut args = args();
        args.exhaustive = true;
        assert_eq!(build_scan_args(&args).budget, EXHAUSTIVE_BUDGET);
        let fuzz = build_fuzz_args(&args);
        assert_eq!(fuzz.runs, EXHAUSTIVE_RUNS);
        assert_eq!(fuzz.budget, EXHAUSTIVE_BUDGET);
    }

    #[test]
    fn backend_engine_split_follows_the_scaffolds_operation_mix() {
        // Safe-only schemas never pay for a fuzz pass; mutation-only schemas
        // never run a scan that would evaluate nothing; mixed schemas run both.
        assert_eq!(backend_engines(FindMode::Staged, 3, 0), (true, false));
        assert_eq!(backend_engines(FindMode::Staged, 0, 2), (false, true));
        assert_eq!(backend_engines(FindMode::Staged, 3, 1), (true, true));
        // An empty draft still runs the scan pass so the run reports honestly.
        assert_eq!(backend_engines(FindMode::Staged, 0, 0), (true, false));
        // The expert door overrides the scaffold.
        assert_eq!(backend_engines(FindMode::Quick, 0, 2), (true, false));
        assert_eq!(backend_engines(FindMode::Deep, 3, 0), (false, true));
    }

    #[test]
    fn expert_oracle_flags_reach_the_deep_pass() {
        // Unset flags leave the stable default in charge downstream.
        assert!(build_fuzz_args(&args()).only.is_none());
        let mut expert = args();
        expert.only = Some("crash".into());
        expert.no_oracles = Some("jank".into());
        let fuzz = build_fuzz_args(&expert);
        assert_eq!(fuzz.only.as_deref(), Some("crash"));
        assert_eq!(fuzz.no_oracles.as_deref(), Some("jank"));
    }

    #[test]
    fn explicit_bounds_override_mode_defaults() {
        let mut args = args();
        args.exhaustive = true;
        args.runs = Some(4);
        args.budget = Some(75);
        assert_eq!(build_scan_args(&args).budget, 75);
        let fuzz = build_fuzz_args(&args);
        assert_eq!(fuzz.runs, 4);
        assert_eq!(fuzz.budget, 75);
    }
}
