//! One `flutter drive` per device, with line-level log parsing: orchestration
//! markers (ready/done), SHOOT screenshot capture, and structured action-log
//! lines into actions.jsonl.

use crate::simctl;
use anyhow::Result;
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Shared context for one journey run (all devices).
pub struct RunCtx {
    /// Config root: host hook commands run from here.
    pub root: PathBuf,
    pub project_dir: PathBuf,
    pub driver: String,
    pub target: String,
    /// (marker, command) host hooks; {udid} and {device} are substituted.
    pub hooks: Vec<(String, String)>,
    /// Where SHOOT captures land (default: <run_dir>/screenshots; the
    /// screenshot tour overrides this to the visual differ's shotsDir).
    pub shots_dir: PathBuf,
    /// Drive in profile mode (AOT). Perf evidence is only representative
    /// here; debug (JIT) numbers overstate jank.
    pub profile: bool,
    /// --dart-define key=value pairs (already includes KIND if set).
    pub defines: Vec<(String, String)>,
    /// Resolved secret (env-key, value) pairs, used ONLY to redact secret values
    /// out of captured logs. Secrets are resolved into actions host-side, so a
    /// runner never handles them; this keeps the resolved value from persisting
    /// in drive-*.log / evidence on any framework.
    pub secrets: Vec<(String, String)>,
    pub ready_marker: Option<String>,
    pub done_markers: Vec<String>,
    /// Journey-declared completion (see config). Counts as done + passed
    /// unless a runner verdict says otherwise.
    pub device_done_marker: Option<String>,
    pub action_prefix: String,
    pub screenshot_marker: String,
    /// Platform: "flutter-ios-sim" or "web-playwright". Selects the driver.
    pub platform: String,
    /// Web runner directory (where runner.mjs lives) and app URL.
    pub web_runner_dir: Option<PathBuf>,
    pub web_url: Option<String>,
    /// RN runner directory + Appium session config (url, caps).
    pub rn_runner_dir: Option<PathBuf>,
    pub appium_url: Option<String>,
    pub appium_caps: std::collections::BTreeMap<String, String>,
    /// Desktop/Electron/Tauri/instrumented target: bundle id (macOS AX) or a
    /// path to the built executable.
    pub target_app: Option<String>,
    /// Where the per-backend runner scripts live (resolved by orchestrator).
    pub runner_dir: Option<PathBuf>,
    pub run_dir: PathBuf,
    pub started: Instant,
    pub actions: Mutex<std::fs::File>,
    /// Structured exception records (exceptions.jsonl): the third evidence
    /// timeline alongside video and actions. Parsed from Flutter test
    /// framework exception blocks in the drive log.
    pub exceptions: Mutex<std::fs::File>,
}

#[derive(Default)]
pub struct DriveState {
    pub ready: bool,
    pub done: bool,
    /// Some(true) on "All tests passed"-class markers; the first done marker
    /// in config is treated as the passing one.
    pub passed: Option<bool>,
    /// In-flight exception block capture (between the ╡...╞ header and the
    /// closing ═ rule).
    exc_buf: Option<Vec<String>>,
    /// Dart VM service URI, parsed from the drive log; instrument probes
    /// (memory sampling, coverage) attach here.
    pub vm_url: Option<String>,
}

pub struct Drive {
    pub label: String,
    pub state: Arc<Mutex<DriveState>>,
    pub log_path: PathBuf,
    child: Child,
}

#[derive(Serialize)]
struct ActionRecord<'a> {
    t_ms: u128,
    device: &'a str,
    line: &'a str,
}

#[derive(Serialize)]
struct ExceptionRecord {
    t_ms: u128,
    device: String,
    /// Header text, e.g. "EXCEPTION CAUGHT BY FLUTTER TEST FRAMEWORK".
    kind: String,
    /// The thrown description (lines after "The following ... was thrown").
    message: String,
    /// Stack frames pointing at Dart source (file:line preserved).
    frames: Vec<String>,
}

pub fn spawn_drive(ctx: Arc<RunCtx>, udid: &str, label: &str, no_build: bool) -> Result<Drive> {
    let log_path = ctx.run_dir.join(format!("drive-{label}.log"));
    let mut cmd = build_command(&ctx, udid, label, no_build)?;
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;
    let state = Arc::new(Mutex::new(DriveState::default()));
    let log = Arc::new(Mutex::new(std::fs::File::create(&log_path)?));

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    for stream in [stdout.map(StreamSrc::Out), stderr.map(StreamSrc::Err)]
        .into_iter()
        .flatten()
    {
        let ctx = ctx.clone();
        let state = state.clone();
        let log = log.clone();
        let label = label.to_string();
        let udid = udid.to_string();
        tokio::spawn(async move {
            let mut lines = match stream {
                StreamSrc::Out(s) => Lines::Out(BufReader::new(s).lines()),
                StreamSrc::Err(s) => Lines::Err(BufReader::new(s).lines()),
            };
            while let Ok(Some(line)) = lines.next().await {
                handle_line(&ctx, &state, &log, &label, &udid, &line).await;
            }
        });
    }

    Ok(Drive {
        label: label.to_string(),
        state,
        log_path,
        child,
    })
}

/// Build the OS command for one device, routing by the platform's backend.
/// Every backend spawns a runner that prints the SAME marker protocol; only
/// the launch differs. Non-flutter backends receive `defines` as env, so
/// REPROIT_FUZZ_CONFIG and injected secrets arrive identically everywhere.
fn build_command(ctx: &RunCtx, udid: &str, label: &str, no_build: bool) -> Result<Command> {
    use crate::platform::Backend;
    let backend = crate::platform::backend(&ctx.platform)
        .ok_or_else(|| anyhow::anyhow!("unknown platform {}", ctx.platform))?;
    let video_dir = ctx.run_dir.join(format!("video-{label}"));
    let with_defines = |c: &mut Command| {
        for (k, v) in &ctx.defines {
            c.env(k, v);
        }
        // The runner's own device label, so a multi-actor scenario runner knows
        // which actor it is (the conductor maps `a`/`b`/... to actor order).
        c.env("REPROIT_DEVICE", label);
    };

    let cmd = match backend {
        Backend::FlutterDrive => {
            let mut c = Command::new("flutter");
            c.current_dir(&ctx.project_dir)
                .arg("drive")
                .arg(format!("--driver={}", ctx.driver))
                .arg(format!("--target={}", ctx.target));
            for (k, v) in &ctx.defines {
                c.arg(format!("--dart-define={k}={v}"));
            }
            // Device label, so the dart explorer can play one actor of a scenario.
            c.arg(format!("--dart-define=REPROIT_DEVICE={label}"));
            if ctx.profile {
                c.arg("--profile");
            }
            if no_build {
                c.arg("--no-build");
            }
            c.arg("-d").arg(udid);
            c
        }
        // Web: Playwright node runner, configured by URL.
        Backend::WebCdp if ctx.platform == "web-playwright" => {
            let dir = ctx
                .web_runner_dir
                .clone()
                .ok_or_else(|| anyhow::anyhow!("web-playwright needs webRunnerDir in config"))?;
            let mut c = Command::new("node");
            c.arg(dir.join("runner.mjs"));
            c.env("REPROIT_URL", ctx.web_url.clone().unwrap_or_default());
            c.env("REPROIT_VIDEO_DIR", &video_dir);
            with_defines(&mut c);
            c
        }
        // Electron / Tauri: single-file node runner; target = built executable.
        Backend::WebCdp => {
            let mut c = Command::new("node");
            c.arg(runner_script(ctx, &format!("{}.mjs", ctx.platform))?);
            c.env("REPROIT_APP", target_app(ctx)?);
            c.env("REPROIT_VIDEO_DIR", &video_dir);
            with_defines(&mut c);
            c
        }
        // React Native / native mobile over an Appium session.
        Backend::Appium => {
            let dir = ctx
                .rn_runner_dir
                .clone()
                .ok_or_else(|| anyhow::anyhow!("appium backend needs rnRunnerDir in config"))?;
            let mut c = Command::new("node");
            c.arg(dir.join("runner.mjs"));
            c.env(
                "REPROIT_APPIUM_URL",
                ctx.appium_url.clone().unwrap_or_default(),
            );
            c.env(
                "REPROIT_APPIUM_CAPS",
                serde_json::to_string(&ctx.appium_caps).unwrap_or_else(|_| "{}".into()),
            );
            c.env("REPROIT_VIDEO_DIR", &video_dir);
            with_defines(&mut c);
            c
        }
        // Native desktop via the OS accessibility API. The toolkit (AppKit/
        // SwiftUI/Qt/GTK/wxWidgets/Avalonia) is irrelevant; the runner reads
        // whichever a11y tree the host OS exposes.
        Backend::DesktopAx => {
            let mut c = Command::new("swift");
            c.arg(runner_script(ctx, "macos-ax.swift")?);
            c.env("REPROIT_TARGET", target_app(ctx)?);
            with_defines(&mut c);
            c
        }
        Backend::DesktopUia => {
            let mut c = Command::new("uv");
            c.arg("run").arg(runner_script(ctx, "windows-uia.py")?);
            c.env("REPROIT_TARGET", target_app(ctx)?);
            with_defines(&mut c);
            c
        }
        Backend::DesktopAtspi => {
            let mut c = Command::new("uv");
            c.arg("run").arg(runner_script(ctx, "linux-atspi.py")?);
            c.env("REPROIT_TARGET", target_app(ctx)?);
            with_defines(&mut c);
            c
        }
        // Immediate-mode: the app carries the reproit hook and prints markers
        // itself, so we just launch the built executable and parse its stdout.
        Backend::Instrumented => {
            let mut c = Command::new(target_app(ctx)?);
            with_defines(&mut c);
            c
        }
        // Terminal UI: re-invoke ourselves as the PTY driver (always available,
        // no external script). REPROIT_TUI_CMD is the terminal app to launch.
        Backend::Tui => {
            let exe = std::env::current_exe()
                .map_err(|e| anyhow::anyhow!("locating reproit binary for tui backend: {e}"))?;
            let mut c = Command::new(exe);
            c.arg("__tui");
            c.env("REPROIT_TUI_CMD", target_app(ctx)?);
            with_defines(&mut c);
            c
        }
    };
    Ok(cmd)
}

/// Resolve a runner script: config `runnerDir`, then `REPROIT_RUNNERS`, then a
/// `runners/` dir beside the config.
fn runner_script(ctx: &RunCtx, file: &str) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(d) = &ctx.runner_dir {
        candidates.push(d.join(file));
    }
    if let Ok(d) = std::env::var("REPROIT_RUNNERS") {
        candidates.push(PathBuf::from(d).join(file));
    }
    candidates.push(ctx.root.join("runners").join(file));
    candidates.into_iter().find(|p| p.exists()).ok_or_else(|| {
        anyhow::anyhow!("runner {file} not found (set app.runnerDir or REPROIT_RUNNERS)")
    })
}

/// The app to drive: explicit executable, else the bundle id (macOS AX).
fn target_app(ctx: &RunCtx) -> Result<String> {
    ctx.target_app
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "platform {} needs app.executable (or bundleId)",
                ctx.platform
            )
        })
}

enum StreamSrc {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

enum Lines {
    Out(tokio::io::Lines<BufReader<tokio::process::ChildStdout>>),
    Err(tokio::io::Lines<BufReader<tokio::process::ChildStderr>>),
}

impl Lines {
    async fn next(&mut self) -> std::io::Result<Option<String>> {
        match self {
            Lines::Out(l) => l.next_line().await,
            Lines::Err(l) => l.next_line().await,
        }
    }
}

async fn handle_line(
    ctx: &RunCtx,
    state: &Mutex<DriveState>,
    log: &Mutex<std::fs::File>,
    label: &str,
    udid: &str,
    line: &str,
) {
    // Redact any resolved secret value back to its placeholder before it lands
    // in the captured log, so a vault secret never persists in evidence even
    // though the runner typed the real value.
    let line = crate::auth::redact(line, &ctx.secrets);
    let line = line.as_str();
    if let Ok(mut f) = log.lock() {
        let _ = writeln!(f, "{line}");
    }

    if let Some(marker) = &ctx.ready_marker {
        if line.contains(marker.as_str()) {
            state.lock().unwrap().ready = true;
        }
    }
    if let Some(idx) = line.find("Connecting to Flutter application at ") {
        let url = line[idx + "Connecting to Flutter application at ".len()..]
            .trim()
            .to_string();
        if url.starts_with("http") {
            state.lock().unwrap().vm_url = Some(url);
        }
    }
    for (i, marker) in ctx.done_markers.iter().enumerate() {
        if line.contains(marker.as_str()) {
            let mut s = state.lock().unwrap();
            s.done = true;
            // Convention: the FIRST configured done marker is the passing
            // one. Runner verdicts are authoritative: they overwrite a
            // journey-declared completion.
            s.passed = Some(i == 0);
        }
    }
    if let Some(marker) = &ctx.device_done_marker {
        if line.contains(marker.as_str()) {
            let mut s = state.lock().unwrap();
            s.done = true;
            // Journey-declared completion: passes only if no runner verdict
            // has spoken (and a later runner verdict still overwrites).
            if s.passed.is_none() {
                s.passed = Some(true);
            }
        }
    }
    if let Some(idx) = line.find(ctx.screenshot_marker.as_str()) {
        let raw = &line[idx + ctx.screenshot_marker.len()..];
        let name: String = raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-'))
            .collect();
        if !name.is_empty() {
            let path = ctx.shots_dir.join(format!("{name}.png"));
            if simctl::screenshot(udid, &path).await {
                println!("  shot  {label}: {name}.png");
            }
        }
    }
    // Exception blocks: capture between the ╡ header and the closing ═ rule,
    // then emit a structured record with message and Dart source frames.
    {
        let mut s = state.lock().unwrap();
        if line.contains("EXCEPTION CAUGHT BY") {
            s.exc_buf = Some(vec![line.to_string()]);
        } else if let Some(buf) = s.exc_buf.as_mut() {
            let trimmed = line.trim_start_matches("flutter: ").trim();
            let is_close = !trimmed.is_empty() && trimmed.chars().all(|c| c == '═');
            if is_close || buf.len() > 300 {
                let record = parse_exception_block(ctx, label, buf);
                s.exc_buf = None;
                if let (Ok(mut f), Ok(json)) =
                    (ctx.exceptions.lock(), serde_json::to_string(&record))
                {
                    let _ = writeln!(f, "{json}");
                }
            } else {
                buf.push(line.to_string());
            }
        }
    }
    for (marker, cmd) in &ctx.hooks {
        if line.contains(marker.as_str()) {
            let cmd = cmd.replace("{udid}", udid).replace("{device}", label);
            println!("  hook  {label}: {marker} -> {cmd}");
            let root = ctx.root.clone();
            tokio::spawn(async move {
                let res = crate::exec::run_shell(&cmd, &root).await;
                if !res.ok() {
                    println!("  warn: hook command failed: {}", res.stderr.trim());
                }
            });
        }
    }
    if line.contains(ctx.action_prefix.as_str()) {
        let record = ActionRecord {
            t_ms: ctx.started.elapsed().as_millis(),
            device: label,
            line,
        };
        if let (Ok(mut f), Ok(json)) = (ctx.actions.lock(), serde_json::to_string(&record)) {
            let _ = writeln!(f, "{json}");
        }
    }
}

fn parse_exception_block(ctx: &RunCtx, device: &str, buf: &[String]) -> ExceptionRecord {
    let clean = |l: &String| l.trim_start_matches("flutter: ").trim().to_string();
    let kind = buf
        .first()
        .and_then(|l| {
            let l = clean(l);
            let start = l.find('╡')? + '╡'.len_utf8();
            let end = l.find('╞')?;
            Some(l[start..end].trim().to_string())
        })
        .unwrap_or_else(|| "EXCEPTION".to_string());
    // Message: lines after "The following ... thrown ...:" until the first blank.
    let mut message = String::new();
    if let Some(start) = buf
        .iter()
        .position(|l| clean(l).starts_with("The following"))
    {
        for l in &buf[start + 1..] {
            let l = clean(l);
            if l.is_empty() {
                break;
            }
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(&l);
        }
    }
    let frames: Vec<String> = buf
        .iter()
        .map(clean)
        .filter(|l| l.contains(".dart") && (l.contains("package:") || l.contains("file://")))
        .take(12)
        .collect();
    ExceptionRecord {
        t_ms: ctx.started.elapsed().as_millis(),
        device: device.to_string(),
        kind,
        message,
        frames,
    }
}

impl Drive {
    pub fn is_ready(&self) -> bool {
        self.state.lock().unwrap().ready
    }
    pub fn is_done(&self) -> bool {
        self.state.lock().unwrap().done
    }
    pub fn passed(&self) -> Option<bool> {
        self.state.lock().unwrap().passed
    }
    /// flutter drive often does NOT exit after the test finishes (app timers
    /// keep the isolate alive), so the orchestrator kills it once the log
    /// reports a result.
    pub async fn kill(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}
