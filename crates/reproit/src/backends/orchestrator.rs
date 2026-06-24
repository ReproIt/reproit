//! Multi-device journey runs: ensure + pin sims, reset state, launch drives
//! (device A builds, the rest reuse with --no-build), record once all are
//! live, wait for log-reported results, finalize evidence.

use crate::config::Config;
use crate::drive::{spawn_drive, Drive, RunCtx};
use crate::reset::run_reset;
use crate::simctl::{
    self, composite_side_by_side, pin_determinism, start_permission_regrant, start_recording,
    tile_windows, Sim,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct RunOutcome {
    pub passed: bool,
    pub run_dir: PathBuf,
}

/// Options for one journey run. `..Default::default()` keeps call sites
/// readable as options accrete.
#[derive(Default)]
pub struct RunOpts<'a> {
    pub kind: Option<&'a str>,
    /// Concurrent devices (multi-actor). 0 is treated as 1.
    pub devices: usize,
    /// Skip device A's build (--no-build). Only valid when the previous
    /// build used the SAME journey target AND the same build mode.
    pub warm: bool,
    /// Override where SHOOT captures land (visual shotsDir for the tour).
    pub shots_dir: Option<&'a Path>,
    /// Extra --dart-define pairs. Keep CONSTANT across warm-reused runs.
    pub extra_defines: &'a [(String, String)],
    /// Drive in profile mode (AOT): required for representative frame/perf
    /// evidence; debug-mode (JIT) numbers overstate jank.
    pub profile: bool,
    /// Print a per-phase wall-clock breakdown (sim ensure, reset, build,
    /// launch->ready, walk, teardown). Off unless set.
    pub profile_timing: bool,
}

#[derive(Serialize)]
struct Manifest {
    journey: String,
    kind: Option<String>,
    started_at: String,
    finished_at: String,
    passed: bool,
    devices: Vec<DeviceManifest>,
    composite: Option<String>,
}

#[derive(Serialize)]
struct DeviceManifest {
    name: String,
    udid: String,
    log: String,
    video: Option<String>,
    passed: Option<bool>,
    /// Frame-timing summary (jank, percentiles) when the journey recorded
    /// frames via trackFrames/reportFrames.
    frames: Option<crate::frames::FrameSummary>,
    /// Heap trend from the VM-service sampler (first/last/peak), when the
    /// service URI was observed.
    memory: Option<serde_json::Value>,
}

const DEVICE_LETTERS: &[char] = &['A', 'B', 'C', 'D', 'E', 'F'];

pub async fn run_journey(
    cfg: &Config,
    root: &Path,
    journey: &str,
    opts: &RunOpts<'_>,
) -> Result<RunOutcome> {
    let RunOpts {
        kind,
        devices,
        warm,
        shots_dir,
        extra_defines,
        profile,
        profile_timing,
    } = *opts;
    let started_at = chrono::Local::now();
    // Per-phase wall-clock instrumentation (printed only with profile_timing).
    let mut timing = PhaseTimer::new(profile_timing);
    // Resolve the platform to its backend; refuse early (before touching any
    // device) if its runner is registered but not built yet, with guidance.
    let plat = crate::platform::resolve(&cfg.app.platform)
        .ok_or_else(|| anyhow::anyhow!("unknown platform {}", cfg.app.platform))?;
    if !plat.status.executable() {
        anyhow::bail!(
            "platform '{}' is registered (backend {}, status {}) but its runner is \
             not built yet.\n  {}",
            plat.id,
            plat.backend.as_str(),
            plat.status.label(),
            plat.note
        );
    }
    if let Some(need) = plat.backend.required_os() {
        if need != std::env::consts::OS {
            anyhow::bail!(
                "platform '{}' uses the {} backend, which only runs on {} (this host is {}).",
                plat.id,
                plat.backend.as_str(),
                need,
                std::env::consts::OS
            );
        }
    }
    // Does reproit provision the device, or does the runner bring its own?
    // `byo_target` (bring-your-own) backends manage their own target (browser,
    // Appium device, desktop app, PTY), so they skip simctl + host recording.
    // Keyed on a backend property, not a "not-Flutter" guess.
    let byo_target = !plat.backend.provisions_device();
    let n = devices.clamp(1, DEVICE_LETTERS.len());

    // Evidence layout: <outDir>/<timestamp>-<journey>/
    let run_dir = root
        .join(&cfg.evidence.out_dir)
        .join(format!("{}-{journey}", started_at.format("%Y%m%d-%H%M%S")));
    // Create the run dir itself (actions.jsonl / exceptions.jsonl below need it).
    // The SHOOT landing dir is NOT created here: it is resolved later (default
    // <run_dir>/screenshots, or the visual/--shots-dir override) and created at
    // its real location, so an override run no longer leaves an empty screenshots/.
    std::fs::create_dir_all(&run_dir)?;

    // BEFORE provisioning a device: the FlutterDrive sim tier needs a vendored
    // explorer (journey_<name>.dart or <name>.dart). Check it here so a missing
    // explorer no longer boots a simulator it then throws away (only FlutterDrive
    // provisions a device, so it is the only backend that could waste one). For
    // reproit's own `explore` journey we SELF-HEAL by vendoring it rather than
    // erroring on a file we know how to create; a named user journey we cannot
    // author, so that still errors with guidance.
    if plat.backend == crate::platform::Backend::FlutterDrive {
        let project_dir = root.join(&cfg.app.project_dir);
        let jd = project_dir.join(&cfg.journeys.dir);
        // The sim tier imports package:integration_test; ensure it's a dev
        // dependency even when the explorer already exists (a project can have
        // one without the other). Idempotent.
        crate::init::ensure_integration_test_dep(&project_dir)?;
        let missing = !jd.join(format!("journey_{journey}.dart")).exists()
            && !jd.join(format!("{journey}.dart")).exists();
        if missing {
            if journey == "explore" {
                crate::init::vendor_sim_explorer(&project_dir, &jd, &cfg.journeys.driver)?;
            } else {
                anyhow::bail!(
                    "no journey_{journey}.dart or {journey}.dart under {}. \
                     Author the journey there, or run `reproit fuzz` to explore.",
                    jd.display()
                );
            }
        }
    }

    // 1. Simulators: only <prefix>-X sims are touched; a sim you use for
    //    other work is never grabbed or rebooted.
    timing.mark("sim");
    let mut sims: Vec<Sim> = Vec::new();
    if byo_target {
        let tag = match plat.backend {
            crate::platform::Backend::Appium => "rn",
            crate::platform::Backend::WebCdp => "web",
            _ => "app",
        };
        for letter in DEVICE_LETTERS.iter().take(n) {
            sims.push(Sim {
                name: format!("{tag}-{letter}"),
                udid: format!("{tag}-{letter}"),
            });
        }
    } else {
        for letter in DEVICE_LETTERS.iter().take(n) {
            let name = format!("{}-{letter}", cfg.devices.name_prefix);
            let sim = simctl::ensure_sim(&name, &cfg.devices.device_type).await?;
            eprintln!("  sim   {} = {}", sim.name, sim.udid);
            sims.push(sim);
        }
        tile_windows(&sims).await;
        for sim in &sims {
            pin_determinism(sim, &cfg.devices.determinism).await;
        }
    }

    // 2. Permission regrant loop, spanning the whole run.
    let services: Vec<String> = cfg
        .devices
        .permissions
        .iter()
        .map(|p| p.service.clone())
        .collect();
    let regrant_stop = (!byo_target && !services.is_empty())
        .then(|| start_permission_regrant(sims.clone(), services, cfg.app.bundle_id.clone()));

    // 3. State reset.
    timing.mark("reset");
    run_reset(&cfg.reset.steps, &cfg.auth.accounts, root).await?;

    // 4. Launch drives. Device 0 compiles; once it reports ready, the rest
    //    launch with --no-build and reuse the build.
    let project_dir = root.join(&cfg.app.project_dir);
    let mut defines: Vec<(String, String)> = cfg
        .app
        .defines
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(k) = kind {
        defines.push(("PROMPT_KIND".to_string(), k.to_string()));
    }
    defines.extend(extra_defines.iter().cloned());
    // Login secrets resolved from the encrypted vault. They still ride env/defines
    // for legacy/login-UI use, but the agnostic path is host-side: actions get
    // their ${REPROIT_SECRET_*} placeholders resolved before delivery, and these
    // values are also handed to the log capture so the resolved value is redacted
    // back to its placeholder in evidence (see RunCtx.secrets / auth::redact).
    let secrets = match crate::auth::secret_env(&cfg.auth, root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  warn: auth: {e}; continuing without injected secrets");
            Vec::new()
        }
    };
    defines.extend(secrets.iter().cloned());
    // Target resolution. Only the Flutter backend has a Dart target file to
    // resolve (journey_<name>.dart first, then <name>.dart for the screenshot
    // tour and other non-journey drive targets). Every other backend's runner
    // is its own driver, so the "target" is just the journey name for labels and
    // evidence. Gated on FlutterDrive specifically, not on "not byo", so the
    // Dart assumption can't leak onto a future provisioning backend.
    let journeys_dir = project_dir.join(&cfg.journeys.dir);
    let target = if plat.backend == crate::platform::Backend::FlutterDrive {
        if journeys_dir
            .join(format!("journey_{journey}.dart"))
            .exists()
        {
            format!("{}/journey_{journey}.dart", cfg.journeys.dir)
        } else if journeys_dir.join(format!("{journey}.dart")).exists() {
            format!("{}/{journey}.dart", cfg.journeys.dir)
        } else {
            anyhow::bail!(
                "no journey_{journey}.dart or {journey}.dart under {}",
                journeys_dir.display()
            );
        }
    } else {
        journey.to_string()
    };
    // SHOOT only takes pictures when the caller asked for screenshots, i.e. it
    // passed an explicit shots dir (the `screenshots` command, record/baseline,
    // or --shots-dir). A plain check/fuzz passes None, so shoot steps stay inert.
    let capture_shots = shots_dir.is_some();
    let shots_dir = match shots_dir {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => root.join(p),
        None => run_dir.join("screenshots"),
    };
    std::fs::create_dir_all(&shots_dir)?;
    let ctx = Arc::new(RunCtx {
        root: root.to_path_buf(),
        project_dir,
        driver: cfg.journeys.driver.clone(),
        target,
        hooks: cfg
            .journeys
            .hooks
            .iter()
            .map(|h| (h.marker.clone(), h.run.clone()))
            .collect(),
        shots_dir,
        capture_shots,
        defines,
        secrets,
        ready_marker: cfg.journeys.ready_marker.clone(),
        done_markers: cfg.journeys.done_markers.clone(),
        device_done_marker: cfg.journeys.device_done_marker.clone(),
        action_prefix: cfg.journeys.action_prefix.clone(),
        screenshot_marker: cfg.evidence.screenshot_marker.clone(),
        profile,
        platform: cfg.app.platform.clone(),
        web_runner_dir: cfg.app.web_runner_dir.as_ref().map(|d| root.join(d)),
        web_url: cfg.app.url.clone(),
        rn_runner_dir: cfg.app.rn_runner_dir.as_ref().map(|d| root.join(d)),
        appium_url: cfg.app.appium_url.clone(),
        appium_caps: cfg.app.appium_caps.clone().unwrap_or_default(),
        // Desktop/Electron/Tauri/instrumented target: explicit executable, else
        // the bundle id (used by the macOS AX runner).
        target_app: cfg
            .app
            .executable
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| (!cfg.app.bundle_id.is_empty()).then(|| cfg.app.bundle_id.clone())),
        runner_dir: cfg.app.runner_dir.as_ref().map(|d| root.join(d)),
        run_dir: run_dir.clone(),
        started: Instant::now(),
        actions: Mutex::new(std::fs::File::create(run_dir.join("actions.jsonl"))?),
        exceptions: Mutex::new(std::fs::File::create(run_dir.join("exceptions.jsonl"))?),
    });

    let deadline = Instant::now() + Duration::from_secs(cfg.journeys.timeout_sec);
    let mut drives: Vec<Drive> = Vec::new();
    let mut watch = DriveWatch::new(n);

    // 4. Build (cold) or launch (warm) device A. The spawn itself returns
    //    quickly; the cost lives in the ready-wait below, so this phase
    //    captures the `flutter drive` compile when cold.
    timing.mark("build");
    eprintln!(
        "  drive {} ({})...",
        sims[0].name,
        if warm { "warm, --no-build" } else { "building" }
    );
    drives.push(spawn_drive(ctx.clone(), &sims[0].udid, "a", warm)?);
    if n > 1 {
        // Wait for A to be live before launching B..N with --no-build. With no
        // ready marker configured, fall back to a fixed build allowance.
        if ctx.ready_marker.is_some() {
            wait_watching(
                &mut watch,
                &drives,
                |ds| ds[0].is_ready() || ds[0].is_done(),
                deadline,
            )
            .await;
        } else {
            tokio::time::sleep(Duration::from_secs(if warm { 20 } else { 90 })).await;
        }
        for (i, sim) in sims.iter().enumerate().skip(1) {
            let label = DEVICE_LETTERS[i].to_lowercase().to_string();
            eprintln!("  drive {} (--no-build)...", sim.name);
            drives.push(spawn_drive(ctx.clone(), &sim.udid, &label, true)?);
        }
    }

    // 5. Record once all devices are live, so the video captures just the
    //    interaction. Without a ready marker, recording starts immediately
    //    after launch. The ready-wait is the app launch + Dart VM connect that
    //    batching pays only once per session.
    timing.mark("launch");
    if ctx.ready_marker.is_some() {
        wait_watching(
            &mut watch,
            &drives,
            |ds| ds.iter().all(|d| d.is_ready() || d.is_done()),
            deadline,
        )
        .await;
    }
    let mut recordings = Vec::new();
    // Byo-target runners record themselves (e.g. Playwright's recordVideo in the
    // browser context), so the host-side simctl recorder is for provisioned
    // devices only.
    if cfg.evidence.video && !byo_target {
        eprintln!("  recording...");
        for (sim, drive) in sims.iter().zip(&drives) {
            let path = run_dir.join(format!("device-{}.mov", drive.label));
            match start_recording(&sim.udid, &path) {
                Ok(r) => recordings.push(r),
                Err(e) => eprintln!("  warn: recording {} failed: {e}", sim.name),
            }
        }
    }

    // 5b. Memory sampler (instrument v1a): every 5s, one-shot VM-service
    //     samples per device into memory-<dev>.jsonl. Soak's leak oracle and
    //     the manifest summary read these.
    let mem_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let states: Vec<(String, Arc<Mutex<crate::drive::DriveState>>)> = drives
            .iter()
            .map(|d| (d.label.clone(), d.state.clone()))
            .collect();
        let run_dir2 = run_dir.clone();
        let t0 = ctx.started;
        let stop = mem_stop.clone();
        tokio::spawn(async move {
            use std::io::Write;
            while !stop.load(Ordering::Relaxed) {
                for (label, state) in &states {
                    let url = state.lock().unwrap().vm_url.clone();
                    let Some(url) = url else { continue };
                    if let Ok(sample) = crate::vmservice::sample_memory(&url).await {
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(run_dir2.join(format!("memory-{label}.jsonl")))
                        {
                            let _ = writeln!(
                                f,
                                "{}",
                                serde_json::json!({
                                    "t_ms": t0.elapsed().as_millis() as u64,
                                    "heap_used": sample.heap_used,
                                    "heap_capacity": sample.heap_capacity,
                                    "external": sample.external,
                                })
                            );
                        }
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    // 6. Wait for every journey to REPORT done in its log (not for the
    //    process to exit, which can hang). This is the actual walk (every seed
    //    in a batch runs here within the single session).
    timing.mark("walk");
    // Wait for every journey to REPORT done in its log (not for the
    //    process to exit, which can hang), capped by the timeout. Once the
    //    first device is done, the rest get a bounded grace window: a drive
    //    can linger forever without flushing its runner verdict, and a
    //    finished run must not ride out the full timeout because of it.
    let any_done = wait_watching(
        &mut watch,
        &drives,
        |ds| ds.iter().any(|d| d.is_done()),
        deadline,
    )
    .await;
    let all_done = if any_done {
        let grace = Instant::now() + Duration::from_secs(cfg.journeys.linger_grace_sec);
        let cap = if grace < deadline { grace } else { deadline };
        wait_watching(
            &mut watch,
            &drives,
            |ds| ds.iter().all(|d| d.is_done()),
            cap,
        )
        .await
    } else {
        false
    };
    if !any_done {
        eprintln!(
            "  warn: timeout after {}s; collecting evidence anyway",
            cfg.journeys.timeout_sec
        );
    } else if !all_done {
        eprintln!(
            "  warn: device(s) without a runner verdict after {}s grace (lingering drive); judging by observed markers",
            cfg.journeys.linger_grace_sec
        );
    }
    // Small tail so the recording includes the final state.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 7. Teardown: stop regrant loop, finalize recordings, reap drives.
    timing.mark("teardown");
    if let Some(stop) = regrant_stop {
        stop.store(true, Ordering::Relaxed);
    }
    mem_stop.store(true, Ordering::Relaxed);
    let mut videos: Vec<PathBuf> = Vec::new();
    for rec in recordings {
        videos.push(rec.path.clone());
        rec.stop().await;
    }

    // Verdict: any explicit failure fails the run, and a run with no explicit
    // pass at all fails. A device that never reported done is harder: when a
    // deviceDoneMarker is configured, journeys DECLARE completion, so a device
    // killed without marker or verdict did not finish its journey (e.g. it
    // starved waiting on a counterpart that broke) and must fail the run, or a
    // weak pass on one device can mask a wedged assertion on another. Without
    // the marker convention, a verdictless lingerer stays neutral (the
    // pre-marker behavior).
    let verdicts: Vec<Option<bool>> = drives.iter().map(|d| d.passed()).collect();
    let incomplete = ctx.device_done_marker.is_some() && drives.iter().any(|d| !d.is_done());
    let passed = verdicts.contains(&Some(true)) && !verdicts.contains(&Some(false)) && !incomplete;
    if incomplete {
        eprintln!("  note: device(s) killed before declaring completion; run fails");
    }

    // Coverage snapshot (instrument v1b): while the VM service is still alive
    // and the verdict is known, record which code this run executed, labeled
    // pass/fail. Best-effort (only Flutter exposes the service); `reproit
    // localize` ranks suspicious code across a run's snapshots via Ochiai SBFL.
    for drive in &drives {
        let url = drive.state.lock().unwrap().vm_url.clone();
        let Some(url) = url else { continue };
        if let Ok(covered) = crate::vmservice::collect_coverage(&url).await {
            let _ = std::fs::write(
                run_dir.join("coverage.cov.json"),
                serde_json::to_string(&serde_json::json!({ "passed": passed, "covered": covered }))
                    .unwrap_or_default(),
            );
            break;
        }
    }

    let device_manifests: Vec<DeviceManifest> = sims
        .iter()
        .zip(&drives)
        .enumerate()
        .map(|(i, (sim, drive))| DeviceManifest {
            name: sim.name.clone(),
            udid: sim.udid.clone(),
            log: drive.log_path.to_string_lossy().into_owned(),
            video: videos.get(i).map(|v| v.to_string_lossy().into_owned()),
            passed: drive.passed(),
            frames: std::fs::read_to_string(&drive.log_path)
                .ok()
                .and_then(|log| crate::frames::process(&run_dir, &drive.label, &log)),
            memory: memory_summary(&run_dir, &drive.label),
        })
        .collect();
    for drive in drives {
        drive.kill().await;
    }

    // 8. Composite multi-device videos side by side.
    let mut composite_path = None;
    if cfg.evidence.composite && videos.len() >= 2 {
        let out = run_dir.join("composite.mp4");
        if composite_side_by_side(&videos, &out).await {
            eprintln!("  wrote {}", out.display());
            composite_path = Some(out.to_string_lossy().into_owned());
        } else {
            eprintln!("  warn: composite failed; raw clips are in the run dir");
        }
    }

    let manifest = Manifest {
        journey: journey.to_string(),
        kind: kind.map(String::from),
        started_at: started_at.to_rfc3339(),
        finished_at: chrono::Local::now().to_rfc3339(),
        passed,
        devices: device_manifests,
        composite: composite_path,
    };
    std::fs::write(
        run_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )
    .context("writing manifest")?;

    timing.finish();
    eprintln!(
        "  {}  evidence: {}",
        if passed { "PASS" } else { "FAIL" },
        run_dir.display()
    );
    Ok(RunOutcome { passed, run_dir })
}

/// True when the headless tier (`flutter test`) is available for this config's
/// platform. The headless tier is Flutter-only: every other backend (web-cdp,
/// appium, desktop) manages its own non-simulator target and routes through
/// `run_journey`. This is the SINGLE place tier eligibility is decided, so
/// `fuzz` and `check` select the same way.
pub fn headless_tier_available(cfg: &Config) -> bool {
    crate::platform::resolve(&cfg.app.platform)
        .map(|p| p.backend == crate::platform::Backend::FlutterDrive)
        .unwrap_or(false)
}

/// Run a journey on the appropriate execution tier. When `sim` is requested, or
/// the platform has no headless tier (any non-Flutter backend), this routes
/// through the real tier (`run_journey`). Otherwise it uses the cheap headless
/// tier (`run_journey_headless`). `fuzz` and `check` both dispatch through here
/// so they agree on the tier for a given platform.
pub async fn run_journey_tier(
    cfg: &Config,
    root: &Path,
    journey: &str,
    opts: &RunOpts<'_>,
    sim: bool,
) -> Result<RunOutcome> {
    if sim || !headless_tier_available(cfg) {
        run_journey(cfg, root, journey, opts).await
    } else {
        run_journey_headless(cfg, root, journey, opts).await
    }
}

/// HEADLESS execution tier: run the seeded explorer under
/// `flutter test` (WidgetTester drives the REAL app in-process) instead of
/// `flutter drive` on a simulator. NO simctl, NO recording, NO VM service, NO
/// Xcode: the walk runs in well under a second on any machine, Linux included.
///
/// It captures the test's stdout into the SAME `drive-a.log` path the simulator
/// path writes, so marker/exception parsing (model/map.rs, modes/fuzz.rs) is
/// byte-identical and findings/trace/coverage attribution work unchanged. The
/// outcome is minimal (passed + run_dir): the perf/jank and runtime oracles are
/// the simulator tier's job (see explorer_headless.dart's oracle-scope header).
///
/// Flutter-only: every other backend manages its own (non-simulator) target
/// already, so headless tiering does not apply to them.
pub async fn run_journey_headless(
    cfg: &Config,
    root: &Path,
    journey: &str,
    opts: &RunOpts<'_>,
) -> Result<RunOutcome> {
    let started_at = chrono::Local::now();
    let mut timing = PhaseTimer::new(opts.profile_timing);

    let plat = crate::platform::resolve(&cfg.app.platform)
        .ok_or_else(|| anyhow::anyhow!("unknown platform {}", cfg.app.platform))?;
    if plat.backend != crate::platform::Backend::FlutterDrive {
        anyhow::bail!(
            "the headless tier is Flutter-only (platform '{}' uses the {} backend); \
             use the simulator tier",
            plat.id,
            plat.backend.as_str()
        );
    }

    let run_dir = root
        .join(&cfg.evidence.out_dir)
        .join(format!("{}-{journey}", started_at.format("%Y%m%d-%H%M%S")));
    std::fs::create_dir_all(&run_dir)?;

    // Resolve the headless test target: <journeys.dir>/fuzz_headless_<name>.dart,
    // then fuzz_headless_test.dart (the conventional bugzoo/scaffold name), then
    // a sibling test/ dir. This is a `flutter test` target (a testWidgets file),
    // NOT an integration_test/flutter_drive target.
    let project_dir = root.join(&cfg.app.project_dir);
    let target = resolve_headless_target(cfg, &project_dir, journey)?;

    // Defines: the same config-level defines + caller extras (REPROIT_FUZZ_CONFIG
    // travels here exactly as on the simulator path), plus injected secrets.
    let mut defines: Vec<(String, String)> = cfg
        .app
        .defines
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(k) = opts.kind {
        defines.push(("PROMPT_KIND".to_string(), k.to_string()));
    }
    defines.extend(opts.extra_defines.iter().cloned());
    match crate::auth::secret_env(&cfg.auth, root) {
        Ok(secrets) => defines.extend(secrets),
        Err(e) => eprintln!("  warn: auth: {e}; continuing without injected secrets"),
    }

    // `flutter test` compiles + runs in one invocation; there is no separate
    // warm/no-build launch. Phases: build (compile) is folded into the single
    // run, so we time the whole `flutter test` as `test-run`.
    timing.mark("test-run");
    eprintln!("  headless: flutter test {target}");
    let mut cmd = tokio::process::Command::new("flutter");
    cmd.current_dir(&project_dir)
        .arg("test")
        .arg(&target)
        .arg("--reporter=expanded");
    for (k, v) in &defines {
        cmd.arg(format!("--dart-define={k}={v}"));
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd
        .output()
        .await
        .with_context(|| format!("spawning flutter test for {target}"))?;

    // Persist BOTH streams to drive-a.log: marker lines are on stdout, but the
    // test framework prints exception blocks and verdicts on either, so the
    // parser sees everything the simulator log would have.
    let log_path = run_dir.join("drive-a.log");
    let mut log = String::new();
    log.push_str(&String::from_utf8_lossy(&output.stdout));
    log.push_str(&String::from_utf8_lossy(&output.stderr));
    std::fs::write(&log_path, &log).context("writing headless drive log")?;

    // Verdict: the explorer drains app exceptions so the test process itself
    // exits 0 even when it found a bug (findings travel via the emitted blocks,
    // judged by the fuzz oracle, not the test exit code). Treat a JOURNEY DONE
    // marker as the run completing; a non-zero exit with no marker is a harness
    // failure, not a finding.
    let done = log.contains("JOURNEY DONE");
    let passed = done && output.status.success();
    if !done {
        eprintln!(
            "  warn: headless run produced no JOURNEY DONE marker (flutter test \
             exit {:?}); see {}",
            output.status.code(),
            log_path.display()
        );
    }

    // Minimal manifest so downstream tooling (list/triage) finds the run; no
    // device/video/frames (those are the simulator tier's evidence).
    let manifest = serde_json::json!({
        "journey": journey,
        "tier": "headless",
        "started_at": started_at.to_rfc3339(),
        "finished_at": chrono::Local::now().to_rfc3339(),
        "passed": passed,
        "log": log_path.to_string_lossy(),
    });
    std::fs::write(
        run_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )
    .context("writing headless manifest")?;

    timing.finish();
    Ok(RunOutcome { passed, run_dir })
}

/// Resolve the `flutter test` target for the headless tier.
fn resolve_headless_target(cfg: &Config, project_dir: &Path, journey: &str) -> Result<String> {
    let dir = &cfg.journeys.dir;
    let candidates = [
        format!("{dir}/fuzz_headless_{journey}.dart"),
        format!("{dir}/fuzz_headless_test.dart"),
        format!("test/fuzz_headless_{journey}.dart"),
        "test/fuzz_headless_test.dart".to_string(),
    ];
    for c in &candidates {
        if project_dir.join(c).exists() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!(
        "no headless explorer found (looked for {}). Vendor templates/explorer_headless.dart \
         as test/fuzz_headless_test.dart and set the app import + pumpWidget. Or run the \
         simulator tier with `reproit fuzz --sim`.",
        candidates.join(", ")
    )
}

/// Per-phase wall-clock timer for `--profile-timing`. Each `mark(name)` closes
/// the previous phase and opens a new one; `finish()` closes the last and, if
/// enabled, prints a single `timing:` line. Off (no allocation churn, no
/// output) unless enabled.
struct PhaseTimer {
    enabled: bool,
    started: Instant,
    last: Instant,
    phases: Vec<(&'static str, Duration)>,
}

impl PhaseTimer {
    fn new(enabled: bool) -> Self {
        let now = Instant::now();
        PhaseTimer {
            enabled,
            started: now,
            last: now,
            phases: Vec::new(),
        }
    }

    fn mark(&mut self, next: &'static str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if let Some((_, d)) = self.phases.last_mut() {
            *d = now - self.last;
        }
        self.phases.push((next, Duration::ZERO));
        self.last = now;
    }

    fn finish(&mut self) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if let Some((_, d)) = self.phases.last_mut() {
            *d = now - self.last;
        }
        eprintln!("  {}", Self::format(&self.phases, now - self.started));
    }

    /// The single `timing:` line: each phase's seconds plus the total. Pure, so
    /// the format is unit-testable without going through the device run.
    fn format(phases: &[(&'static str, Duration)], total: Duration) -> String {
        let parts: Vec<String> = phases
            .iter()
            .map(|(name, d)| format!("{name}={:.1}s", d.as_secs_f64()))
            .collect();
        format!(
            "timing: {} total={:.1}s",
            parts.join(" "),
            total.as_secs_f64()
        )
    }
}

/// Per-device liveness announcements: prints each device's "live" and
/// "done" transition exactly once across the whole run, so long waits are
/// never silent.
struct DriveWatch {
    started: Instant,
    ready: Vec<bool>,
    done: Vec<bool>,
}

impl DriveWatch {
    fn new(n: usize) -> Self {
        DriveWatch {
            started: Instant::now(),
            ready: vec![false; n],
            done: vec![false; n],
        }
    }

    fn tick(&mut self, drives: &[Drive]) {
        let t = self.started.elapsed().as_secs();
        for (i, d) in drives.iter().enumerate() {
            if !self.ready[i] && d.is_ready() {
                self.ready[i] = true;
                eprintln!("  live  device {} (t+{t}s)", d.label);
            }
            if !self.done[i] && d.is_done() {
                self.done[i] = true;
                let verdict = match d.passed() {
                    Some(true) => "PASS",
                    Some(false) => "FAIL",
                    None => "?",
                };
                eprintln!("  done  device {}: {verdict} (t+{t}s)", d.label);
            }
        }
    }
}

/// Poll `cond` every 2s until true or `deadline`, announcing per-device
/// transitions as they happen. Returns the final value.
async fn wait_watching<F: Fn(&[Drive]) -> bool>(
    watch: &mut DriveWatch,
    drives: &[Drive],
    cond: F,
    deadline: Instant,
) -> bool {
    loop {
        watch.tick(drives);
        if cond(drives) {
            return true;
        }
        if Instant::now() >= deadline {
            watch.tick(drives);
            return cond(drives);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Summarize memory-<label>.jsonl into first/last/peak heap (bytes) and
/// print the trend line. None when no samples were collected.
fn memory_summary(run_dir: &Path, label: &str) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(run_dir.join(format!("memory-{label}.jsonl"))).ok()?;
    let samples: Vec<u64> = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("heap_used").and_then(serde_json::Value::as_u64))
        .collect();
    let (first, last) = (*samples.first()?, *samples.last()?);
    let peak = *samples.iter().max()?;
    let mb = |b: u64| b as f64 / 1_048_576.0;
    eprintln!(
        "  memory device {label}: heap {:.1}MB -> {:.1}MB (peak {:.1}MB, {} samples)",
        mb(first),
        mb(last),
        mb(peak),
        samples.len()
    );
    Some(serde_json::json!({
        "samples": samples.len(),
        "heap_first": first,
        "heap_last": last,
        "heap_peak": peak,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_line_lists_each_phase_then_total() {
        let phases = [
            ("sim", Duration::from_millis(1200)),
            ("reset", Duration::from_millis(300)),
            ("build", Duration::from_millis(10200)),
            ("launch", Duration::from_millis(4500)),
            ("walk", Duration::from_millis(150000)),
            ("teardown", Duration::from_millis(2000)),
        ];
        let total: Duration = phases.iter().map(|(_, d)| *d).sum();
        let line = PhaseTimer::format(&phases, total);
        assert_eq!(
            line,
            "timing: sim=1.2s reset=0.3s build=10.2s launch=4.5s walk=150.0s teardown=2.0s total=168.2s"
        );
    }

    #[test]
    fn disabled_timer_does_nothing() {
        let mut t = PhaseTimer::new(false);
        t.mark("sim");
        t.mark("walk");
        t.finish();
        assert!(t.phases.is_empty()); // no work accrued when disabled
    }
}
