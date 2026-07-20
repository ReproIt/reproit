//! iOS simulator control via `xcrun simctl`, plus ffmpeg compositing.

use crate::adapters::config::Determinism;
use crate::runtime::process::{run, sigint};
use anyhow::{bail, Result};
use regex::Regex;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};

#[derive(Debug, Clone)]
pub struct Sim {
    pub name: String,
    pub udid: String,
}

fn udid_re() -> Regex {
    Regex::new(r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}")
        .unwrap()
}

/// Find a simulator by exact name; create it from `device_type` if missing.
/// Boots it (best-effort: already-booted is fine).
pub async fn ensure_sim(name: &str, device_type: &str) -> Result<Sim> {
    let re = udid_re();
    let list = run("xcrun", &["simctl", "list", "devices"]).await;
    let mut udid: Option<String> = None;
    for line in list.stdout.lines() {
        // Lines look like: "    Example-A (UDID) (Booted)"
        if line.trim_start().starts_with(&format!("{name} (")) {
            if let Some(m) = re.find(line) {
                udid = Some(m.as_str().to_string());
                break;
            }
        }
    }
    let udid = match udid {
        Some(u) => u,
        None => {
            let created = run("xcrun", &["simctl", "create", name, device_type]).await;
            match re.find(created.stdout.trim()) {
                Some(m) => m.as_str().to_string(),
                None => bail!("could not create simulator {name}: {}", created.stderr),
            }
        }
    };
    let _ = run("xcrun", &["simctl", "boot", &udid]).await;
    Ok(Sim {
        name: name.to_string(),
        udid,
    })
}

/// List sims whose name starts with `prefix`.
pub async fn list_sims(prefix: &str) -> Vec<(String, String, bool)> {
    let re = udid_re();
    let list = run("xcrun", &["simctl", "list", "devices"]).await;
    let mut out = Vec::new();
    for line in list.stdout.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(prefix) {
            if let Some(m) = re.find(trimmed) {
                let name = trimmed[..trimmed.find(" (").unwrap_or(trimmed.len())].to_string();
                out.push((name, m.as_str().to_string(), trimmed.contains("(Booted)")));
            }
        }
    }
    out
}

/// Pin determinism: fixed status bar (stable clock/battery for visual diffs),
/// fixed GPS, keyboard intro off. All best-effort.
pub async fn pin_determinism(sim: &Sim, cfg: &Determinism) {
    if let Some([lat, lon]) = cfg.location {
        let _ = run(
            "xcrun",
            &[
                "simctl",
                "location",
                &sim.udid,
                "set",
                &format!("{lat},{lon}"),
            ],
        )
        .await;
    }
    let _ = run(
        "xcrun",
        &[
            "simctl",
            "status_bar",
            &sim.udid,
            "override",
            "--time",
            &cfg.status_bar_time,
            "--batteryLevel",
            "100",
            "--batteryState",
            "charged",
            "--cellularBars",
            "4",
            "--wifiBars",
            "3",
        ],
    )
    .await;
    if cfg.disable_keyboard_intro {
        let _ = run(
            "xcrun",
            &[
                "simctl",
                "spawn",
                &sim.udid,
                "defaults",
                "write",
                "com.apple.keyboard.ContinuousPath",
                "ContinuousPathEnabled",
                "-bool",
                "false",
            ],
        )
        .await;
    }
}

pub async fn grant(sim: &Sim, service: &str, bundle_id: &str) {
    let _ = run(
        "xcrun",
        &["simctl", "privacy", &sim.udid, "grant", service, bundle_id],
    )
    .await;
}

/// Re-grant permissions on a loop for the whole run. `flutter drive`
/// reinstalls the app (resetting TCC), so a one-shot grant is unreliable; a
/// periodic grant lands between install and first permission request, so the
/// dialog never shows and nothing steals host focus. Returns a stop handle.
pub fn start_permission_regrant(
    sims: Vec<Sim>,
    services: Vec<String>,
    bundle_id: String,
) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    tokio::spawn(async move {
        while !stop2.load(Ordering::Relaxed) {
            for sim in &sims {
                for service in &services {
                    grant(sim, service, &bundle_id).await;
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    stop
}

pub async fn screenshot(udid: &str, out_path: &Path) -> bool {
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    run(
        "xcrun",
        &[
            "simctl",
            "io",
            udid,
            "screenshot",
            &out_path.to_string_lossy(),
        ],
    )
    .await
    .ok()
}

pub struct Recording {
    child: Child,
    pub path: std::path::PathBuf,
}

/// Start a video recording. Stop with `stop()`: SIGINT writes the moov atom
/// so the file is playable.
pub fn start_recording(udid: &str, out_path: &Path) -> Result<Recording> {
    let child = Command::new("xcrun")
        .args([
            "simctl",
            "io",
            udid,
            "recordVideo",
            "--codec=h264",
            "--force",
            &out_path.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(Recording {
        child,
        path: out_path.to_path_buf(),
    })
}

impl Recording {
    pub async fn stop(mut self) {
        if let Some(pid) = self.child.id() {
            sigint(pid).await;
        }
        // Give it time to finalize, then force-reap.
        let timed = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
        if timed.is_err() {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
    }
}

/// Open the Simulator app and tile the given sims' windows so none occludes
/// another (recordings are per-device, but visible windows help the operator).
pub async fn tile_windows(sims: &[Sim]) {
    let _ = run("open", &["-a", "Simulator"]).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let placements: String = sims
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "\n  try\n    set position of (first window whose name contains \"{}\") to {{{}, \
                 60}}\n  end try",
                s.name,
                40 + i * 520
            )
        })
        .collect();
    let script = format!(
        "tell application \"System Events\" to tell process \"Simulator\"{placements}\nend tell"
    );
    let _ = run("osascript", &["-e", &script]).await;
}

/// hstack N videos side by side into one mp4.
pub async fn composite_side_by_side(inputs: &[std::path::PathBuf], out: &Path) -> bool {
    if inputs.len() < 2 {
        return false;
    }
    let mut args: Vec<String> = vec!["-y".into()];
    for i in inputs {
        args.push("-i".into());
        args.push(i.to_string_lossy().into_owned());
    }
    let labels: String = (0..inputs.len()).map(|i| format!("[{i}:v]")).collect();
    args.push("-filter_complex".into());
    args.push(format!("{labels}hstack=inputs={}", inputs.len()));
    args.push(out.to_string_lossy().into_owned());
    let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run("ffmpeg", &argrefs).await.ok()
}
