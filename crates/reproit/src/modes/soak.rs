//! `reproit soak` v0: repeat a reversible cycle N times via the fuzz replay
//! machinery and read the heap slope from the VM-service memory samples.
//! The core invariant: a reversible cycle must be resource-neutral; heap
//! growth that scales with cycle count is a leak, with the cycle as repro.
//!
//! v0 takes the cycle as CLI actions; once map cycles carry verified
//! reversibility, soak enumerates them automatically.

use crate::config::Config;
use crate::orchestrator;
use anyhow::{ensure, Result};
use serde_json::json;
use std::path::Path;

/// Read the heap-vs-time series for a soak run, from whichever source produced
/// it. `memory-a.jsonl` (the Dart VM-service sampler) is authoritative when
/// present; otherwise the WEB runner's `MEMORY:SAMPLE {"t_ms":..,"heap_used":..}`
/// markers in `drive-a.log` are parsed (the web heap sampler, since the web
/// target exposes no VM service). Each entry is `(t_ms, heap_used)`. An empty
/// result means neither source had samples (the caller then errors honestly).
fn read_memory_series(run_dir: &Path) -> Vec<(u64, u64)> {
    let parse_line = |l: &str| -> Option<(u64, u64)> {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        Some((v.get("t_ms")?.as_u64()?, v.get("heap_used")?.as_u64()?))
    };
    // 1. VM-service samples (Flutter sim/VM tier): one JSON object per line.
    if let Ok(raw) = std::fs::read_to_string(run_dir.join("memory-a.jsonl")) {
        let series: Vec<(u64, u64)> = raw.lines().filter_map(parse_line).collect();
        if series.len() >= 2 {
            return series;
        }
    }
    // 2. Web heap sampler: MEMORY:SAMPLE markers embedded in the drive log.
    let Ok(log) = std::fs::read_to_string(run_dir.join("drive-a.log")) else {
        return Vec::new();
    };
    log.lines()
        .filter_map(|line| {
            let idx = line.find("MEMORY:SAMPLE ")?;
            parse_line(line[idx + "MEMORY:SAMPLE ".len()..].trim())
        })
        .collect()
}

/// Least-squares slope (heap units per sample index) of a heap series. `x` is the
/// 0-based sample index, `y` the heap value; returns `dy/dx` of the best-fit line,
/// 0.0 for a degenerate (<2 distinct-x) series. Robust to endpoint noise in a way
/// `(last - first)` is not.
fn least_squares_slope(series: &[(u64, u64)]) -> f64 {
    let n = series.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let (mut sx, mut sy, mut sxy, mut sxx) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (i, &(_, heap)) in series.iter().enumerate() {
        let x = i as f64;
        let y = heap as f64;
        sx += x;
        sy += y;
        sxy += x * y;
        sxx += x * x;
    }
    let denom = n * sxx - sx * sx;
    if denom == 0.0 {
        return 0.0;
    }
    (n * sxy - sx * sy) / denom
}

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

    let cfg_path = crate::layout::fuzz_config_path(root);
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

    // Heap series from the sampler. Two sources, tried in order:
    //   1. memory-a.jsonl: the Dart VM-service sampler (Flutter sim/VM tier).
    //   2. MEMORY:SAMPLE markers in drive-a.log: the WEB runner emits these per
    //      cycle from performance.memory.usedJSHeapSize (the web heap sampler),
    //      since the web target has no VM service. Reconstructing the series from
    //      the drive log lets `--soak --target web` measure heap growth.
    // When NEITHER source yields samples, the run truly observed no heap signal,
    // so we still error honestly with the original message.
    let series = read_memory_series(&outcome.run_dir);
    ensure!(
        series.len() >= 2,
        "no memory samples (VM service URI not observed?): too few memory samples ({})",
        series.len()
    );

    let first = series.first().unwrap().1 as f64;
    let last = series.last().unwrap().1 as f64;
    let peak = series.iter().map(|s| s.1).max().unwrap() as f64;
    // Per-cycle growth from a LEAST-SQUARES slope over the whole heap series, not
    // the endpoint delta `last - first`. Endpoints are the worst estimator of a
    // trend: one GC right before the final sample hides a real leak, one transient
    // spike at either end invents one. The regression fits every sample, so a
    // monotonic climb reads as a leak and a noisy-but-flat series does not.
    let slope_per_sample = least_squares_slope(&series);
    let growth = slope_per_sample * (series.len() - 1) as f64;
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

#[cfg(test)]
mod tests {
    use super::least_squares_slope;

    #[test]
    fn slope_recovers_a_leak_hidden_by_an_endpoint_gc() {
        // A monotonic climb whose FINAL sample drops (a GC right before the last
        // read). The endpoint delta `last - first` reads NEGATIVE -> "no leak",
        // but the least-squares slope over every sample stays POSITIVE, so the
        // leak is still caught.
        let s: Vec<(u64, u64)> = vec![(0, 100), (1, 200), (2, 300), (3, 400), (4, 50)];
        let endpoint = s.last().unwrap().1 as f64 - s.first().unwrap().1 as f64;
        assert!(endpoint < 0.0, "endpoint delta hides the leak");
        assert!(least_squares_slope(&s) > 0.0, "slope still sees the climb");
    }

    #[test]
    fn slope_is_small_on_noisy_but_flat_heap() {
        // Jitter around a flat mean: no real trend, so the slope must stay far
        // below a genuine climb's -- a single bounce must not invent a leak.
        let flat: Vec<(u64, u64)> = vec![(0, 100), (1, 130), (2, 90), (3, 110), (4, 100)];
        let climb: Vec<(u64, u64)> = vec![(0, 100), (1, 200), (2, 300), (3, 400), (4, 500)];
        assert!(least_squares_slope(&flat).abs() < least_squares_slope(&climb));
    }

    #[test]
    fn slope_zero_on_degenerate_series() {
        assert_eq!(least_squares_slope(&[]), 0.0);
        assert_eq!(least_squares_slope(&[(0, 5)]), 0.0);
    }
}
