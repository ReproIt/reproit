//! `reproit soak` v0: repeat a reversible cycle N times via the fuzz replay
//! machinery and read the heap slope from the VM-service memory samples.
//! The core invariant: a reversible cycle must be resource-neutral; heap
//! growth that scales with cycle count is a leak, with the cycle as repro.
//!
//! v0 takes the cycle as CLI actions; once map cycles carry verified
//! reversibility, soak enumerates them automatically.

use crate::config::Config;
use crate::orchestrator;
use anyhow::{ensure, Context, Result};
use serde_json::json;
use std::path::Path;

/// Heap growth per cycle above this is a leak verdict. Generous: GC noise
/// and warmup allocations are real; a true leak scales linearly.
const LEAK_BYTES_PER_CYCLE: f64 = 262_144.0;

pub struct SoakArgs {
    pub journey: String,
    /// Semicolon-separated actions, e.g. "tap:Compose;tap:New post;tap:Publish".
    pub cycle: String,
    pub repeats: u32,
    pub warm: bool,
}

pub async fn soak(cfg: &Config, root: &Path, args: &SoakArgs) -> Result<bool> {
    let actions: Vec<String> = args
        .cycle
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    ensure!(
        !actions.is_empty(),
        "empty cycle (use 'tap:<label>' / 'back', ';'-separated)"
    );
    let replay: Vec<String> = (0..args.repeats)
        .flat_map(|_| actions.iter().cloned())
        .collect();

    let cfg_path = root.join(".reproit/fuzz_config.json");
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    std::fs::write(&cfg_path, json!({ "replay": replay }).to_string())?;
    let defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];

    println!(
        "soak: {} x [{}] ({} actions)",
        args.repeats,
        actions.join(" ; "),
        replay.len()
    );
    let outcome = orchestrator::run_journey(
        cfg,
        root,
        &args.journey,
        &orchestrator::RunOpts {
            devices: 1,
            warm: args.warm,
            extra_defines: &defines,
            ..Default::default()
        },
    )
    .await?;
    let _ = std::fs::write(&cfg_path, "{}"); // neutralize for later --warm runs

    // Heap series from the sampler.
    let raw = std::fs::read_to_string(outcome.run_dir.join("memory-a.jsonl"))
        .context("no memory samples (VM service URI not observed?)")?;
    let series: Vec<(u64, u64)> = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| Some((v.get("t_ms")?.as_u64()?, v.get("heap_used")?.as_u64()?)))
        .collect();
    ensure!(
        series.len() >= 2,
        "too few memory samples ({})",
        series.len()
    );

    let first = series.first().unwrap().1 as f64;
    let last = series.last().unwrap().1 as f64;
    let peak = series.iter().map(|s| s.1).max().unwrap() as f64;
    let growth = last - first;
    let per_cycle = growth / args.repeats as f64;
    let mb = |b: f64| b / 1_048_576.0;
    let leak = per_cycle > LEAK_BYTES_PER_CYCLE;

    let exceptions = crate::fuzz::app_exceptions(&outcome.run_dir).len();
    let verdict = if leak { "LEAK" } else { "resource-neutral" };
    println!(
        "  heap: {:.1}MB -> {:.1}MB (peak {:.1}MB) over {} cycles",
        mb(first),
        mb(last),
        mb(peak),
        args.repeats
    );
    println!(
        "  {verdict}: {:+.0}KB/cycle (threshold {:.0}KB/cycle); {exceptions} app exception(s)",
        per_cycle / 1024.0,
        LEAK_BYTES_PER_CYCLE / 1024.0
    );

    let mut md = format!(
        "# soak report\n\ncycle: `{}`\nrepeats: {}\nverdict: **{verdict}** ({:+.0}KB/cycle)\n\n## heap samples\n\n| t (s) | heap (MB) |\n|---|---|\n",
        actions.join(" ; "),
        args.repeats,
        per_cycle / 1024.0,
    );
    for (t, h) in &series {
        md.push_str(&format!(
            "| {:.0} | {:.1} |\n",
            *t as f64 / 1000.0,
            mb(*h as f64)
        ));
    }
    if exceptions > 0 {
        md.push_str(&format!(
            "\n{exceptions} app exception(s) during the soak; see exceptions.jsonl.\n"
        ));
    }
    std::fs::write(outcome.run_dir.join("soak.md"), md)?;
    println!("  report: {}", outcome.run_dir.join("soak.md").display());
    Ok(leak)
}
