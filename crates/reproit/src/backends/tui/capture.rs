//! Deterministic screenshot and finding-clip capture from the terminal grid.

use super::{emit, grid_of, shot};
use std::sync::{Arc, Mutex};

/// Sanitize a shoot name to the contract's `[A-Za-z0-9_/-]` alphabet, matching
/// the orchestrator-side filter in drive.rs so the runner writes the same path
/// the orchestrator looks for.
fn sanitize_shot_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-'))
        .collect()
}

/// Screenshot-capture contract (see backends/drive.rs): render the CURRENT
/// vt100 screen to `$REPROIT_SHOTS_DIR/<name>.png`, then print `SHOOT:<name>`.
/// If REPROIT_SHOTS_DIR is unset we skip the PNG but still print the marker, so
/// the journey timeline still records the shoot point. A leading dir in
/// `<name>` (the `/` in the alphabet) is created under the shots dir.
pub(super) fn shoot(parser: &Arc<Mutex<vt100::Parser>>, raw_name: &str) {
    let name = sanitize_shot_name(raw_name);
    if name.is_empty() {
        return;
    }
    if let Ok(dir) = std::env::var("REPROIT_SHOTS_DIR") {
        if !dir.is_empty() {
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let img = shot::render_screen(parser);
            if let Err(e) = img.save(&path) {
                emit(&format!("JOURNEY[a] step: shoot {name} render failed: {e}"));
            }
        }
    }
    // Print the marker regardless: the orchestrator confirms the PNG (RunnerSide
    // capture) and logs the shoot point either way.
    emit(&format!("SHOOT:{name}"));
}

// --record clip capture (video + finding box).
//
// Every finding gets a filmed clip with a red box on the offending element, on
// EVERY backend. The TUI "window" IS its own rendered cell buffer, so there is
// NO OS screen capture and no privacy concern (unlike the desktop runners that
// film a real window): we compose the video from the very frames render_screen
// already produces during the replay, then draw the box post-capture with the
// shared box-overlay.mjs (the uniform path for backends that cannot inject a
// live DOM overlay). This mirrors the macOS-AX runner's startClipCapture /
// finalize.

/// Parse a positional clip selector into `(row, Some(col))` (a cell anchor) or
/// `(row, None)` (the whole row). Accepts the position-key shapes the TUI
/// oracles emit -- `pos:R,C`, `region:R,C`, `row:R,C`, `row:R`, or a bare `R,C`
/// / `R` -- by taking the text after the last `:` and splitting on `,`. Returns
/// None when there is no numeric row (e.g. a label-style selector that has no
/// cell), so the caller reports drew:false rather than boxing the wrong place.
fn parse_sel_pos(sel: &str) -> Option<(usize, Option<usize>)> {
    let body = sel.rsplit(':').next().unwrap_or(sel);
    let mut it = body.split(',');
    let r: usize = it.next()?.trim().parse().ok()?;
    let c = it.next().and_then(|s| s.trim().parse::<usize>().ok());
    Some((r, c))
}

/// Resolve a clip selector to a CELL rect on the CURRENT screen, in the video's
/// own pixel space (x=col*CELL_W, y=row*CELL_H, ...). With a column anchor we
/// box the element's text extent around it, tolerating SINGLE-space gaps so a
/// menu label like `Toggle Sound` boxes as one run (a two-space gap ends the
/// run); an anchor on a blank cell snaps to the nearest ink on that row. With
/// no column we box the row's whole non-blank extent. Returns None when the row
/// is off-screen or entirely blank, matching the "element couldn't be located"
/// -> drew:false path.
fn resolve_clip_rect(
    parser: &Arc<Mutex<vt100::Parser>>,
    sel: &str,
) -> Option<(u32, u32, u32, u32)> {
    let (r, c_opt) = parse_sel_pos(sel)?;
    let grid = grid_of(parser);
    if r >= grid.len() {
        return None;
    }
    let row = &grid[r];
    let n = row.len();
    let is_ink = |i: usize| i < n && !row[i].is_whitespace();
    let (c0, c1) = match c_opt {
        Some(c) => {
            // Anchor on ink; if the exact cell is blank, snap to the nearest ink
            // on this row so a slightly-off column still boxes the element.
            let anchor = if is_ink(c) {
                c
            } else {
                (0..n)
                    .filter(|&i| is_ink(i))
                    .min_by_key(|&i| (i as isize - c as isize).unsigned_abs())?
            };
            // Grow left/right across ink, stepping over a lone space (but not two).
            let mut lo = anchor;
            while lo > 0 && (is_ink(lo - 1) || (lo >= 2 && is_ink(lo - 2))) {
                lo -= 1;
            }
            let mut hi = anchor;
            while hi + 1 < n && (is_ink(hi + 1) || (hi + 2 < n && is_ink(hi + 2))) {
                hi += 1;
            }
            (lo, hi)
        }
        None => {
            let lo = (0..n).find(|&i| is_ink(i))?;
            let hi = (0..n).rev().find(|&i| is_ink(i))?;
            (lo, hi)
        }
    };
    let x = c0 as u32 * shot::CELL_W;
    let y = r as u32 * shot::CELL_H;
    let w = (c1 - c0 + 1) as u32 * shot::CELL_W;
    let h = shot::CELL_H;
    Some((x, y, w, h))
}

/// Assemble a numbered PNG sequence (`frame%04d.png`) into `out` at `fps`. The
/// yuv420p pixel format needs even dimensions; a CELL_W/CELL_H (8x16) grid is
/// always even, so no padding is needed. Returns whether ffmpeg succeeded.
fn assemble_clip(frames_dir: &std::path::Path, fps: u32, out: &std::path::Path) -> bool {
    let pattern = frames_dir.join("frame%04d.png");
    std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-framerate",
            &fps.to_string(),
            "-i",
        ])
        .arg(&pattern)
        .args(["-pix_fmt", "yuv420p"])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The --record clip capture state for one replay. Frames are written as PNGs
/// and assembled to `$REPROIT_VIDEO_DIR/clip.mov`; the finding's cell rect + a
/// time window go to `box-spec.json`; a `FINDING:BOXED` marker reports whether
/// the box drew, all in the video's own px/sec space (logical == px here, so
/// box-overlay scales by 1).
pub(super) struct ClipCapture {
    video_dir: std::path::PathBuf,
    frames_dir: std::path::PathBuf,
    fps: u32,
    count: usize,
    video_w: u32,
    video_h: u32,
    /// Capture-relative time (s) of the frame right after the triggering
    /// action.
    trigger_time: f64,
    /// The finding element's rect (px), resolved at the triggering action.
    rect: Option<(u32, u32, u32, u32)>,
    sel: String,
    label: String,
    oracle: String,
}

impl ClipCapture {
    /// Arm capture if REPROIT_VIDEO_DIR is set (the caller also gates on a
    /// replay being present). Creates the frames scratch dir under the
    /// video dir.
    pub(super) fn arm(clip: &Clip) -> Option<Self> {
        let dir = std::env::var("REPROIT_VIDEO_DIR")
            .ok()
            .filter(|s| !s.is_empty())?;
        let video_dir = std::path::PathBuf::from(dir);
        let frames_dir = video_dir.join("frames");
        std::fs::create_dir_all(&frames_dir).ok()?;
        Some(ClipCapture {
            video_dir,
            frames_dir,
            // ~260ms per replayed action settle -> ~4 fps tracks real time.
            fps: 4,
            count: 0,
            video_w: 0,
            video_h: 0,
            trigger_time: 0.0,
            rect: None,
            sel: clip.sel.clone(),
            label: clip.label.clone(),
            oracle: clip.oracle.clone(),
        })
    }

    /// Render the current screen and append it as the next frame.
    pub(super) fn capture(&mut self, parser: &Arc<Mutex<vt100::Parser>>) {
        let img = shot::render_screen(parser);
        if self.count == 0 {
            self.video_w = img.width();
            self.video_h = img.height();
        }
        let path = self.frames_dir.join(format!("frame{:04}.png", self.count));
        if img.save(&path).is_ok() {
            self.count += 1;
        }
    }

    /// Capture-relative time (s) of the most recently captured frame.
    fn last_time(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            (self.count - 1) as f64 / self.fps as f64
        }
    }

    /// Mark the just-executed action as the finding's trigger: record its frame
    /// time and resolve the sel to a cell rect from the settled screen
    /// (freshest at the tap, exactly as the macOS-AX runner grabs the
    /// element handle there).
    pub(super) fn mark_trigger(&mut self, parser: &Arc<Mutex<vt100::Parser>>) {
        self.trigger_time = self.last_time();
        if let Some(rect) = resolve_clip_rect(parser, &self.sel) {
            self.rect = Some(rect);
        }
    }

    /// Assemble clip.mov, write box-spec.json, and emit FINDING:BOXED. Pads the
    /// tail by holding the last frame so the box stays visible in the final
    /// second (a screen recording keeps rolling after the action; the
    /// verifier grabs the last frame). drew=false when nothing filmed or
    /// the element never resolved.
    pub(super) fn finalize(&mut self) {
        if self.count > 0 {
            let last = self
                .frames_dir
                .join(format!("frame{:04}.png", self.count - 1));
            for _ in 0..(self.fps * 2) {
                let dst = self.frames_dir.join(format!("frame{:04}.png", self.count));
                if std::fs::copy(&last, &dst).is_ok() {
                    self.count += 1;
                } else {
                    break;
                }
            }
        }
        let mov = self.video_dir.join("clip.mov");
        let assembled = self.count > 0 && assemble_clip(&self.frames_dir, self.fps, &mov);
        let mut drew = false;
        if assembled {
            if let Some((x, y, w, h)) = self.rect {
                let t0 = (self.trigger_time - 0.3).max(0.0);
                let spec = serde_json::json!({
                    "videoW": self.video_w,
                    "videoH": self.video_h,
                    "boxes": [{
                        "x": x, "y": y, "w": w, "h": h,
                        "tStart": t0, "tEnd": 1e9,
                        "label": self.label, "color": "red",
                    }],
                });
                let spec_path = self.video_dir.join("box-spec.json");
                if std::fs::write(&spec_path, spec.to_string()).is_ok() {
                    drew = true;
                }
            }
        }
        emit(&format!(
            "FINDING:BOXED {}",
            serde_json::json!({
                "oracle": self.oracle,
                "sel": self.sel,
                "mov": mov.to_string_lossy(),
                "drew": drew,
            })
        ));
    }
}

/// --record clip plan (replay mode only). When present AND REPROIT_VIDEO_DIR is
/// set, the driver assembles the frames it renders during the replay into
/// clip.mov and, after the replay settles, resolves the finding's `sel` to a
/// CELL rect of the offending screen region, writing box-spec.json next to
/// clip.mov so the host box-overlay step draws the red finding box (the uniform
/// post-capture path every non-DOM backend shares).
pub(super) struct Clip {
    /// A positional selector for the finding's screen region, e.g. `pos:R,C`,
    /// `region:R,C`, or `row:R`. Mapped to a cell rect on the settled screen.
    pub(super) sel: String,
    /// Caption text drawn on the box.
    pub(super) label: String,
    /// Oracle id, echoed back on the FINDING:BOXED marker.
    pub(super) oracle: String,
}
