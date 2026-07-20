//! Flicker oracle: an INTRA-run visual-jank detector. Where the `visual` oracle
//! diffs a settled capture against a committed baseline (cross-run regression),
//! this looks for a transient bad frame WITHIN a single run: a flash, an
//! unstyled frame, a layout that jumps and snaps back during a transition. It
//! needs no baseline, so it fits the "automatic, no curated goldens" model.
//!
//! Source: the repro video (already recorded for `--record-video`). We sample frames
//! with ffmpeg (already a runner dependency), downscale them (robust to AA
//! noise and cheap), and flag any frame i that is far from BOTH neighbors while
//! the neighbors are close to each other, i.e. the screen jumped away at frame
//! i and came back. That "spike that resolves" is exactly a flicker; a real
//! navigation is a jump that does NOT come back, so it does not trip.
//!
//! Caveat: pixels are not deterministic across machines/GPUs/fonts the way the
//! structural signature is, so this is a tolerance-based advisory signal, not a
//! byte-exact, repro-grade guarantee.

use crate::runtime::process as exec;
use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::RgbImage;
use std::path::{Path, PathBuf};

/// Tunables. Distances are normalized fractions in [0,1] (mean per-pixel
/// max-channel abs diff / 255 over the downscaled grid).
#[derive(Clone, Copy, Debug)]
pub struct FlickerCfg {
    /// Frames per second sampled from the video.
    pub fps: u32,
    /// Downscale grid (grid x grid). Small = fast + AA-robust.
    pub grid: u32,
    /// A neighbor distance at or above this counts as a "jump".
    pub jump: f64,
    /// The span (i-1 vs i+1) must be at or below this to count as "resolved".
    pub settle: f64,
}

impl Default for FlickerCfg {
    fn default() -> Self {
        FlickerCfg {
            fps: 30,
            grid: 32,
            jump: 0.12,
            settle: 0.04,
        }
    }
}

/// One detected flicker: the frame that spiked, with the distances that flagged
/// it and a severity (how far it stuck out beyond the surrounding stability).
#[derive(Clone, Debug, PartialEq)]
pub struct FlickerEvent {
    pub index: usize,
    pub t_ms: u64,
    pub jump_in: f64,
    pub jump_out: f64,
    pub span: f64,
    pub severity: f64,
}

/// Mean per-pixel max-channel absolute difference, normalized to [0,1]. Assumes
/// the two frames are the same dimensions (they are once downscaled).
pub fn frame_distance(a: &RgbImage, b: &RgbImage) -> f64 {
    debug_assert_eq!(a.dimensions(), b.dimensions());
    let (w, h) = a.dimensions();
    if w == 0 || h == 0 {
        return 0.0;
    }
    let mut acc: u64 = 0;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        let d =
            pa.0.iter()
                .zip(pb.0.iter())
                .map(|(p, q)| (*p as i16 - *q as i16).unsigned_abs())
                .max()
                .unwrap_or(0);
        acc += d as u64;
    }
    acc as f64 / (w as f64 * h as f64 * 255.0)
}

/// Scan a frame sequence for flicker spikes. Pure and deterministic: frame i is
/// a flicker when it jumps away from both neighbors (jump_in, jump_out >= jump)
/// while the neighbors stay close to each other (span <= settle). A normal
/// transition fails the span test (i-1 and i+1 differ), so it is not flagged.
pub fn detect(frames: &[RgbImage], cfg: &FlickerCfg, fps: u32) -> Vec<FlickerEvent> {
    let mut events = Vec::new();
    if frames.len() < 3 {
        return events;
    }
    for i in 1..frames.len() - 1 {
        let jump_in = frame_distance(&frames[i - 1], &frames[i]);
        let jump_out = frame_distance(&frames[i], &frames[i + 1]);
        let span = frame_distance(&frames[i - 1], &frames[i + 1]);
        if jump_in >= cfg.jump && jump_out >= cfg.jump && span <= cfg.settle {
            events.push(FlickerEvent {
                index: i,
                t_ms: (i as u64) * 1000 / fps.max(1) as u64,
                jump_in,
                jump_out,
                span,
                severity: jump_in.min(jump_out) - span,
            });
        }
    }
    events
}

/// Sample + downscale every frame of `video` via ffmpeg. Frames land in a
/// `.flicker` subdir next to the video and are removed after loading.
pub async fn extract_frames(video: &Path, cfg: &FlickerCfg) -> Result<Vec<RgbImage>> {
    let parent = video.parent().unwrap_or_else(|| Path::new("."));
    let dir = parent.join(".flicker");
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).ok();
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let pattern = dir.join("f_%05d.png");
    let vf = format!("fps={}", cfg.fps);
    let res = exec::run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            &video.to_string_lossy(),
            "-vf",
            &vf,
            &pattern.to_string_lossy(),
        ],
    )
    .await;
    if !res.ok() {
        std::fs::remove_dir_all(&dir).ok();
        anyhow::bail!("ffmpeg frame extraction failed: {}", res.stderr.trim());
    }

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("png"))
        .collect();
    paths.sort();

    let grid = cfg.grid.max(1);
    let mut frames = Vec::with_capacity(paths.len());
    for p in &paths {
        let img = image::open(p)
            .with_context(|| format!("opening frame {}", p.display()))?
            .resize_exact(grid, grid, FilterType::Triangle)
            .to_rgb8();
        frames.push(img);
    }
    std::fs::remove_dir_all(&dir).ok();
    Ok(frames)
}

/// Locate the run's repro video (mirrors deliver.rs::find_repro_video).
pub fn find_repro_video(run_dir: &Path) -> Option<PathBuf> {
    for name in ["device-a.mov", "device-A.mov", "composite.mp4"] {
        let p = run_dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    std::fs::read_dir(run_dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
        (ext == "mov" || ext == "mp4").then_some(p)
    })
}

/// End-to-end: find the run's video, sample it, and scan for flicker.
pub async fn analyze_run(run_dir: &Path, cfg: &FlickerCfg) -> Result<Vec<FlickerEvent>> {
    let video = find_repro_video(run_dir)
        .with_context(|| format!("no repro video (.mov/.mp4) in {}", run_dir.display()))?;
    let frames = extract_frames(&video, cfg).await?;
    Ok(detect(&frames, cfg, cfg.fps))
}

/// Print a human-readable report. Returns true when no flicker was found.
pub fn report(events: &[FlickerEvent]) -> bool {
    if events.is_empty() {
        println!("  ok    no flicker detected");
        return true;
    }
    for e in events {
        println!(
            "  FLICKER  frame {:>4} @ {:>6}ms  in={:.3} out={:.3} span={:.3} (severity {:.3})",
            e.index, e.t_ms, e.jump_in, e.jump_out, e.span, e.severity
        );
    }
    println!("\n{} flicker event(s).", events.len());
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(grid: u32, v: u8) -> RgbImage {
        RgbImage::from_pixel(grid, grid, image::Rgb([v, v, v]))
    }

    // End-to-end through the real ffmpeg extraction path. Ignored by default
    // (CI may lack ffmpeg); run with `cargo test --ignored e2e_video_flicker`.
    #[tokio::test]
    #[ignore = "requires ffmpeg"]
    async fn e2e_video_flicker() {
        let root = std::env::temp_dir().join("reproit_flicker_e2e");
        std::fs::remove_dir_all(&root).ok();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        // 21 black frames with a single white flash at index 10.
        for i in 0..21u32 {
            let v = if i == 10 { 255 } else { 0 };
            RgbImage::from_pixel(64, 64, image::Rgb([v, v, v]))
                .save(src.join(format!("s_{i:03}.png")))
                .unwrap();
        }
        let video = root.join("device-a.mov");
        let pat = src.join("s_%03d.png");
        let r = exec::run(
            "ffmpeg",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-framerate",
                "30",
                "-i",
                &pat.to_string_lossy(),
                "-pix_fmt",
                "yuv420p",
                &video.to_string_lossy(),
            ],
        )
        .await;
        assert!(r.ok(), "ffmpeg encode failed: {}", r.stderr);

        let events = analyze_run(&root, &FlickerCfg::default()).await.unwrap();
        assert!(!events.is_empty(), "expected a flicker event for the flash");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn distance_endpoints() {
        assert_eq!(frame_distance(&solid(4, 0), &solid(4, 0)), 0.0);
        assert_eq!(frame_distance(&solid(4, 0), &solid(4, 255)), 1.0);
    }

    #[test]
    fn detects_single_frame_spike() {
        // black, black, WHITE (spike), black, black -> flicker at index 2.
        let frames = vec![
            solid(4, 0),
            solid(4, 0),
            solid(4, 255),
            solid(4, 0),
            solid(4, 0),
        ];
        let ev = detect(&frames, &FlickerCfg::default(), 30);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].index, 2);
        assert_eq!(ev[0].t_ms, 66); // 2 * 1000 / 30
    }

    #[test]
    fn ignores_normal_transition() {
        // A monotone fade is a real navigation: i-1 and i+1 differ, so span is
        // large and nothing trips.
        let frames = vec![solid(4, 0), solid(4, 85), solid(4, 170), solid(4, 255)];
        assert!(detect(&frames, &FlickerCfg::default(), 30).is_empty());
    }

    #[test]
    fn ignores_stable_sequence() {
        let frames = vec![solid(4, 30), solid(4, 30), solid(4, 30), solid(4, 30)];
        assert!(detect(&frames, &FlickerCfg::default(), 30).is_empty());
    }

    #[test]
    fn too_few_frames_is_empty() {
        assert!(detect(&[solid(4, 0), solid(4, 255)], &FlickerCfg::default(), 30).is_empty());
    }
}
