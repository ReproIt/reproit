use super::*;

fn sanitize_shot_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-'))
        .collect()
}

pub(super) fn shoot(window: &IUIAutomationElement, raw_name: &str) {
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
            let _ = capture_window(window, &path);
        }
    }
    emit(&format!("SHOOT:{name}"));
}

// PrintWindow the target into a memory DC, pull the pixels via GetDIBits, and
// write a PNG with the `image` crate. Best-effort: any failure just skips the
// PNG.
fn capture_window(window: &IUIAutomationElement, path: &std::path::Path) -> Result<()> {
    let (l, t, r, b) = el_bounds(window).context("no window bounds")?;
    let (w, h) = ((r - l).max(1), (b - t).max(1));
    let hwnd = window_hwnd(window);
    if hwnd.0.is_null() {
        anyhow::bail!("no native window handle");
    }
    unsafe {
        let hwnd_dc = GetWindowDC(Some(hwnd));
        if hwnd_dc.0.is_null() {
            anyhow::bail!("GetWindowDC failed");
        }
        let mem_dc = CreateCompatibleDC(Some(hwnd_dc));
        let bmp = CreateCompatibleBitmap(hwnd_dc, w, h);
        let old = SelectObject(mem_dc, HGDIOBJ(bmp.0));
        // PW_RENDERFULLCONTENT (0x2) so DWM-composited content is included.
        let printed = PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(2)).as_bool() || {
            // Fall back to a plain BitBlt of the window DC.
            BitBlt(mem_dc, 0, 0, w, h, Some(hwnd_dc), 0, 0, SRCCOPY).is_ok()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let got = GetDIBits(
            mem_dc,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(Some(hwnd), hwnd_dc);
        if !printed || got == 0 {
            anyhow::bail!("PrintWindow/GetDIBits failed");
        }
        // BGRX -> RGBA for the image encoder.
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            rgba[i * 4] = buf[i * 4 + 2];
            rgba[i * 4 + 1] = buf[i * 4 + 1];
            rgba[i * 4 + 2] = buf[i * 4];
            rgba[i * 4 + 3] = 255;
        }
        let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
            .context("image buffer size mismatch")?;
        img.save(path)?;
    }
    Ok(())
}

// --record-video clip capture (ffmpeg gdigrab, window-region only).
//
// Films ONLY the target window (never the whole desktop, a hard privacy rule)
// for the duration of a replay. The Windows twin of the macOS runner's
// `screencapture -v -l <id>`: ffmpeg's gdigrab grabber scoped to the window.
//
// Primary path is a fixed screen-rectangle capture of the window's own UIA
// bounds (`-offset_x/-offset_y -video_size WxH -i desktop`), window-region
// only. This is deliberately the SAME coordinate space the finding box is
// measured in: BoundingRectangle gives every element AND the window a
// screen-pixel, top-left origin, so a capture of exactly the window rect makes
// videoW/H == capture px and box = element - windowOrigin land 1:1 with no
// scale or offset. (gdigrab's `-i title=` alternative crops to the window's
// CLIENT area, dropping the title bar + borders -- a different origin than the
// window rect -- so it is only the fallback for when the window bounds are
// unavailable.)
//
// box-overlay still rescales by (actual video px / videoW) as a safety net, so
// an odd-sized window cropped to even dimensions for the H.264 encoder is
// handled.
//
// Windows has no clean SIGINT for a child, so finalize by writing `q` to
// ffmpeg's stdin (its documented graceful-quit key), then reaping -- this
// flushes the moov atom and closes the .mov exactly as Control-C would.
pub(super) fn start_clip_capture(
    window_title: &str,
    window_bounds: Option<(i32, i32, i32, i32)>,
    out_mov: &str,
) -> Option<std::process::Child> {
    if let Some(parent) = std::path::Path::new(out_mov).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.arg("-y").args(["-f", "gdigrab", "-framerate", "10"]);
    if let Some((l, t, r, b)) = window_bounds {
        // Region-scoped to the window's own screen rectangle (its UIA bounds), so
        // the video origin == the box coordinate origin. Nothing else is filmed.
        let (w, h) = ((r - l).max(2), (b - t).max(2));
        cmd.args([
            "-offset_x",
            &l.to_string(),
            "-offset_y",
            &t.to_string(),
            "-video_size",
            &format!("{w}x{h}"),
            "-i",
            "desktop",
        ]);
    } else if !window_title.is_empty() {
        // Fallback: title-scoped capture (client area only) when we have no bounds.
        cmd.arg("-i").arg(format!("title={window_title}"));
    } else {
        return None;
    }
    // Force even dimensions + a broadly-playable pixel format so H.264 accepts an
    // odd-sized window (crop drops at most 1px on the right/bottom edge).
    cmd.args([
        "-vf",
        "crop=trunc(iw/2)*2:trunc(ih/2)*2",
        "-pix_fmt",
        "yuv420p",
    ])
    .arg(out_mov)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    cmd.spawn().ok()
}

pub(super) fn stop_clip_capture(child: Option<std::process::Child>) {
    let Some(mut child) = child else { return };
    if let Some(mut stdin) = child.stdin.take() {
        // `q` = ffmpeg's graceful quit; it finalizes and closes the .mov.
        let _ = stdin.write_all(b"q");
        let _ = stdin.flush();
    }
    let _ = child.wait();
}
