//! Visual regression: current capture vs committed baseline. Determinism is
//! handled upstream (pinned status bar, seeded data); here a per-pixel
//! tolerance absorbs antialiasing noise and a per-image percent threshold
//! keeps tiny diffs from tripping the gate.

use crate::config::Visual;
use anyhow::{bail, Context, Result};
use image::{Rgb, RgbImage};
use std::path::{Path, PathBuf};

pub fn diff(cfg: &Visual, root: &Path, update: bool) -> Result<bool> {
    let shots_dir = root.join(&cfg.shots_dir);
    let baseline_dir = shots_dir.join("baseline");
    let diff_dir = shots_dir.join("diff");

    let shots = current_shots(&shots_dir)?;
    if shots.is_empty() {
        bail!("no screenshots found under {}", shots_dir.display());
    }

    if update {
        if baseline_dir.is_dir() {
            std::fs::remove_dir_all(&baseline_dir)?;
        }
        for p in &shots {
            let rel = p.strip_prefix(&shots_dir).unwrap();
            let dst = baseline_dir.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(p, dst)?;
        }
        println!("Updated baseline: {} screenshot(s)", shots.len());
        return Ok(true);
    }

    if diff_dir.is_dir() {
        std::fs::remove_dir_all(&diff_dir)?;
    }

    let (mut failures, mut new, mut advisory) = (0u32, 0u32, 0u32);
    for p in &shots {
        let rel = p.strip_prefix(&shots_dir).unwrap().to_path_buf();
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let base_path = baseline_dir.join(&rel);
        if !base_path.exists() {
            println!("  NEW   {rel_str}  (no baseline, run visual update to accept)");
            new += 1;
            continue;
        }
        let base = load_rgb(&base_path)?;
        let cur = load_rgb(p)?;

        // Compare the common region so a few-px size drift still diffs.
        let w = base.width().min(cur.width());
        let h = base.height().min(cur.height());
        let size_note = if base.dimensions() != cur.dimensions() {
            format!(
                "  [size {}x{} -> {}x{}]",
                base.width(),
                base.height(),
                cur.width(),
                cur.height()
            )
        } else {
            String::new()
        };

        let mut changed_mask = vec![false; (w as usize) * (h as usize)];
        let mut changed: u64 = 0;
        for y in 0..h {
            for x in 0..w {
                let a = base.get_pixel(x, y);
                let b = cur.get_pixel(x, y);
                let d =
                    a.0.iter()
                        .zip(b.0.iter())
                        .map(|(p, q)| (*p as i16 - *q as i16).unsigned_abs())
                        .max()
                        .unwrap_or(0);
                if d > cfg.pixel_tol as u16 {
                    changed_mask[(y * w + x) as usize] = true;
                    changed += 1;
                }
            }
        }
        let pct = 100.0 * changed as f64 / (w as f64 * h as f64);
        if pct <= cfg.fail_pct {
            println!("  ok    {rel_str}  {pct:5.2}% changed{size_note}");
            continue;
        }

        let diff_path = write_diff(&diff_dir, &rel, &cur, &changed_mask, w, h)?;
        let tail = format!("{size_note}  -> {}", diff_path.display());
        if cfg.advisory.iter().any(|a| a == &rel_str) {
            advisory += 1;
            println!("  adv   {rel_str}  {pct:5.2}% changed (non-deterministic, advisory){tail}");
        } else {
            failures += 1;
            println!("  FAIL  {rel_str}  {pct:5.2}% changed{tail}");
        }
    }

    println!(
        "\n{failures} regression(s), {advisory} advisory, {new} new.  (pixel-tol={}, fail-pct={})",
        cfg.pixel_tol, cfg.fail_pct
    );
    Ok(failures == 0)
}

fn load_rgb(path: &Path) -> Result<RgbImage> {
    Ok(image::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .to_rgb8())
}

/// Darken the current capture and mark changed pixels in red so they pop.
fn write_diff(
    diff_dir: &Path,
    rel: &Path,
    cur: &RgbImage,
    changed_mask: &[bool],
    w: u32,
    h: u32,
) -> Result<PathBuf> {
    let mut out = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = cur.get_pixel(x, y);
            let px = if changed_mask[(y * w + x) as usize] {
                Rgb([255, 0, 64])
            } else {
                Rgb([p.0[0] / 3, p.0[1] / 3, p.0[2] / 3])
            };
            out.put_pixel(x, y, px);
        }
    }
    let dst = diff_dir.join(rel);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    out.save(&dst)?;
    Ok(dst)
}

/// All .png under the shots dir, excluding baseline/ and diff/.
fn current_shots(shots_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(shots_dir, shots_dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.parent() == Some(root) && (name == "baseline" || name == "diff") {
                continue;
            }
            walk(root, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("png") {
            out.push(path);
        }
    }
    Ok(())
}
