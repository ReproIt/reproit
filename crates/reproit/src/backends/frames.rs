//! Frame-timing evidence: per-frame UI-thread (build) and raster-thread
//! durations recorded in-app (journey helpers' trackFrames/reportFrames) and
//! parsed from FRAMES:BATCH log lines. Produces frames-<dev>.json, a
//! rendered SVG frame chart, and a jank summary in the manifest.
//!
//! Budget: 16.7ms per thread at 60Hz. A frame is janky if EITHER thread
//! exceeds budget (they run in parallel; either one late drops the frame).
//! Soak mode later turns p90-per-transition into a regression oracle.

use serde::Serialize;
use std::path::Path;

const BUDGET_US: i64 = 16_700;

/// (vsync-relative ms, build us, raster us)
type Frame = (i64, i64, i64);

#[derive(Serialize)]
pub struct FrameSummary {
    pub frames: usize,
    pub span_ms: i64,
    pub p50_build_ms: f64,
    pub p90_build_ms: f64,
    pub p50_raster_ms: f64,
    pub p90_raster_ms: f64,
    pub jank_frames: usize,
    pub jank_pct: f64,
    /// Worst frame: max(build, raster) in ms, and when it happened.
    pub worst_ms: f64,
    pub worst_at_ms: i64,
}

/// Parse FRAMES:BATCH lines ("t,b,r;t,b,r;...") out of a drive log.
fn parse(log: &str) -> Vec<Frame> {
    let mut frames = Vec::new();
    for line in log.lines() {
        let Some(idx) = line.find("FRAMES:BATCH ") else {
            continue;
        };
        for triple in line[idx + "FRAMES:BATCH ".len()..].trim().split(';') {
            let mut it = triple.split(',');
            if let (Some(t), Some(b), Some(r)) = (it.next(), it.next(), it.next()) {
                if let (Ok(t), Ok(b), Ok(r)) =
                    (t.trim().parse(), b.trim().parse(), r.trim().parse())
                {
                    frames.push((t, b, r));
                }
            }
        }
    }
    frames
}

fn percentile(sorted: &[i64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx] as f64 / 1000.0
}

fn summarize(frames: &[Frame]) -> FrameSummary {
    let mut builds: Vec<i64> = frames.iter().map(|f| f.1).collect();
    let mut rasters: Vec<i64> = frames.iter().map(|f| f.2).collect();
    builds.sort_unstable();
    rasters.sort_unstable();
    let jank = frames
        .iter()
        .filter(|f| f.1 > BUDGET_US || f.2 > BUDGET_US)
        .count();
    let worst = frames
        .iter()
        .max_by_key(|f| f.1.max(f.2))
        .copied()
        .unwrap_or((0, 0, 0));
    FrameSummary {
        frames: frames.len(),
        span_ms: frames.last().map(|f| f.0).unwrap_or(0),
        p50_build_ms: percentile(&builds, 0.5),
        p90_build_ms: percentile(&builds, 0.9),
        p50_raster_ms: percentile(&rasters, 0.5),
        p90_raster_ms: percentile(&rasters, 0.9),
        jank_frames: jank,
        jank_pct: if frames.is_empty() {
            0.0
        } else {
            100.0 * jank as f64 / frames.len() as f64
        },
        worst_ms: worst.1.max(worst.2) as f64 / 1000.0,
        worst_at_ms: worst.0,
    }
}

/// Parse, write frames-<label>.json + frames-<label>.svg, print a summary.
/// Returns the summary for the manifest, or None if the journey did not
/// record frames.
pub fn process(run_dir: &Path, label: &str, log: &str) -> Option<FrameSummary> {
    let frames = parse(log);
    if frames.is_empty() {
        return None;
    }
    let summary = summarize(&frames);
    let _ = std::fs::write(
        run_dir.join(format!("frames-{label}.json")),
        serde_json::to_string(&frames).unwrap_or_default(),
    );
    let _ = std::fs::write(run_dir.join(format!("frames-{label}.svg")), chart(&frames));
    println!(
        "  frames device {label}: {} frames, p90 build {:.1}ms raster {:.1}ms, jank {} ({:.1}%), worst {:.0}ms @t+{:.1}s",
        summary.frames,
        summary.p90_build_ms,
        summary.p90_raster_ms,
        summary.jank_frames,
        summary.jank_pct,
        summary.worst_ms,
        summary.worst_at_ms as f64 / 1000.0
    );
    Some(summary)
}

/// The frame chart: one bar per frame (sampled if huge), green within
/// budget, amber when the UI thread blew it, red when the raster thread did.
/// Dashed line = the 16.7ms / 60fps budget.
fn chart(frames: &[Frame]) -> String {
    let stride = (frames.len() / 1200).max(1);
    let sampled: Vec<&Frame> = frames.iter().step_by(stride).collect();
    let n = sampled.len();
    let (bar_w, h, max_ms) = (2.0, 160.0, 50.0);
    let w = n as f64 * bar_w + 60.0;
    let scale = |us: i64| -> f64 { ((us as f64 / 1000.0).min(max_ms)) / max_ms * (h - 20.0) };
    let mut bars = String::new();
    for (i, f) in sampled.iter().enumerate() {
        let worst = f.1.max(f.2);
        let color = if f.2 > BUDGET_US {
            "#e5533c"
        } else if f.1 > BUDGET_US {
            "#ffb000"
        } else {
            "#54d17a"
        };
        let bh = scale(worst).max(1.0);
        bars.push_str(&format!(
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="{color}"/>"#,
            40.0 + i as f64 * bar_w,
            h - bh,
            bar_w - 0.4,
            bh
        ));
    }
    let budget_y = h - scale(BUDGET_US);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w:.0} {total_h:.0}" font-family="monospace" font-size="9">
<rect width="100%" height="100%" fill="#0e0d0b"/>
<text x="40" y="14" fill="#ede6d6">frame times (max of build/raster per frame, {count} frames{sampled_note})</text>
{bars}
<line x1="40" y1="{budget_y:.1}" x2="{w:.0}" y2="{budget_y:.1}" stroke="#a89e8a" stroke-dasharray="4 3"/>
<text x="2" y="{budget_label_y:.1}" fill="#a89e8a">16.7ms</text>
<text x="40" y="{legend_y:.0}" fill="#54d17a">within budget</text>
<text x="140" y="{legend_y:.0}" fill="#ffb000">build jank</text>
<text x="230" y="{legend_y:.0}" fill="#e5533c">raster jank</text>
</svg>
"##,
        total_h = h + 20.0,
        count = frames.len(),
        sampled_note = if stride > 1 {
            format!(", 1/{stride} sampled")
        } else {
            String::new()
        },
        budget_label_y = budget_y + 3.0,
        legend_y = h + 14.0,
    )
}
