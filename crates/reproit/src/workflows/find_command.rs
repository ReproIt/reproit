//! Outcome-oriented discovery over the existing scan and fuzz engines.

use super::{fuzz_command, scan_command};
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
        only: None,
        no_oracles: None,
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
