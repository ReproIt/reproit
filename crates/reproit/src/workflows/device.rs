//! Device discovery, target execution, and cross-target divergence.

use super::repro::check_repro;
use super::*;
use crate::domain::repro;
use crate::workflows::fuzz;

pub(super) async fn enumerate_devices() -> Vec<crate::adapters::device::Device> {
    let mut out: Vec<crate::adapters::device::Device> = Vec::new();
    // Flutter knows all three platforms; ask it first.
    let flutter = exec::run("flutter", &["devices", "--machine"]).await;
    if flutter.ok() {
        out.extend(crate::adapters::device::parse_flutter_devices(
            &flutter.stdout,
        ));
    }
    // iOS simulators (macOS only; the command simply fails elsewhere).
    let sims = exec::run("xcrun", &["simctl", "list", "devices"]).await;
    if sims.ok() {
        for d in crate::adapters::device::parse_simctl_devices(&sims.stdout) {
            if !out.iter().any(|e| e.id == d.id) {
                out.push(d);
            }
        }
    }
    // Android devices/emulators.
    let adb = exec::run("adb", &["devices"]).await;
    if adb.ok() {
        for d in crate::adapters::device::parse_adb_devices(&adb.stdout) {
            if !out.iter().any(|e| e.id == d.id) {
                out.push(d);
            }
        }
    }
    out
}

/// Whether the interactive device picker should appear for this run. The picker
/// selects a simulator that REPROIT provisions, which only the FlutterDrive
/// backend does (it boots the sim via simctl). Every other backend brings its
/// own target (Appium via caps, web a browser, desktop the host, TUI a PTY), so
/// none need reproit's picker. Even FlutterDrive defaults to the headless
/// `flutter test` tier (no device) unless --sim. `--target`/`--device` bypass
/// this upstream.
pub(super) fn run_needs_device_pick(platform: &str, sim: bool) -> bool {
    match platform::backend(platform) {
        Some(b) if b.provisions_device() => sim,
        _ => false,
    }
}

/// Interactive device picker: enumerate devices, filter to the targets the
/// project supports, print a numbered list, and read a selection from stdin.
/// When `want_name` is given, match it without prompting. Returns None if there
/// are no devices or the selection is invalid/empty (the caller then falls back
/// to the config default rather than hanging).
pub(super) async fn pick_device_interactive(
    want_name: Option<&str>,
    allowed: &[crate::domain::target::Target],
) -> Option<crate::adapters::device::Device> {
    let mut devices = enumerate_devices().await;
    if !allowed.is_empty() {
        devices.retain(|d| allowed.contains(&d.target));
    }
    if devices.is_empty() {
        eprintln!("  no devices found (flutter/simctl/adb reported none)");
        return None;
    }
    if let Some(want) = want_name {
        return devices
            .iter()
            .find(|d| d.name == want || d.id == want)
            .cloned();
    }
    println!("Select a device:");
    for (i, d) in devices.iter().enumerate() {
        println!(
            "  {}) {} [{}] {}{}",
            i + 1,
            d.name,
            d.target.as_str(),
            d.id,
            if d.booted { "  (booted)" } else { "" }
        );
    }
    print!("  > ");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    let choice: usize = line.trim().parse().ok()?;
    if choice == 0 || choice > devices.len() {
        return None;
    }
    Some(devices[choice - 1].clone())
}

/// Resolve the device for a `check` run from `--target`/`--device`. When a
/// `--device` is given, it is matched against the enumerated list (or used
/// verbatim if not found, so an offline-but-known id still routes). When only
/// `--target` is given, the first matching device is chosen. When neither is
/// given and a TTY is present (and not --yes), the interactive picker is shown;
/// non-interactive returns None (the run falls back to the config default
/// rather than hanging on a prompt).
pub(super) async fn resolve_check_device(
    ctx: &Ctx,
    platform: &str,
    target: Option<&str>,
    device: Option<&str>,
) -> Option<crate::adapters::device::Device> {
    if device.is_none() && target.is_none() {
        // Non-interactive, OR a headless-tier run that uses no device: skip the
        // prompt and let the config default (headless) stand.
        if ctx.yes
            || !std::io::IsTerminal::is_terminal(&std::io::stdin())
            || !run_needs_device_pick(platform, false)
        {
            return None;
        }
        return pick_device_interactive(None, &crate::domain::target::platform_targets(platform))
            .await;
    }
    let devices = enumerate_devices().await;
    let want_target = target.and_then(crate::domain::target::Target::parse);
    if let Some(want) = device {
        if let Some(d) = devices.iter().find(|d| d.name == want || d.id == want) {
            return Some(d.clone());
        }
        // Unknown but explicit: route it anyway under the requested (or web) target.
        return Some(crate::adapters::device::Device {
            name: want.to_string(),
            id: want.to_string(),
            target: want_target.unwrap_or(crate::domain::target::Target::Web),
            booted: false,
        });
    }
    if let Some(t) = want_target {
        return devices
            .iter()
            .find(|d| d.target == t && d.booted)
            .or_else(|| devices.iter().find(|d| d.target == t))
            .cloned();
    }
    None
}

/// Run the saved repro suite against MULTIPLE run targets and diff which repros
/// are red on a SUBSET of targets (a cross-target divergence). Each target gets
/// its own driver invocation via the same REPROIT_PLATFORM/REPROIT_DEVICE/
/// REPROIT_ENGINE env contract as `run_targets`, ScopedEnv-restored between
/// targets. This is the `check` analog of fuzz's multi-target routing: instead
/// of finding NEW bugs it re-confirms KNOWN repros per target, and a repro that
/// fails on one target but passes on another is the divergence.
///
/// Web engines (chromium/firefox/webkit) are the direct runtime fanout. Mobile
/// (ios/android) exercises the routing + per-target dispatch + divergence diff,
/// but a real dual-device check needs two booted devices on the host (infra-
/// gated); without a device for a target it routes to the config default and
/// says so.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_check_targets(
    ctx: &Ctx,
    loaded: &config::Loaded,
    targets: &[crate::domain::target::RunTarget],
    device: Option<&str>,
    repro: &Option<String>,
    runs: Option<u32>,
    devices: usize,
    kind: Option<&str>,
    record_video: bool,
) -> Result<ExitCode> {
    let times = runs.unwrap_or(loaded.config.gate.runs).max(1);
    // The suite: a single named repro, or every saved repro.
    let metas = match repro {
        Some(r) => vec![repro::resolve(&loaded.root, r).ok_or_else(|| {
            anyhow::anyhow!("no repro `{r}` (by id or alias). List them with `reproit repros`.")
        })?],
        None => repro::list(&loaded.root),
    };
    if metas.is_empty() {
        anyhow::bail!("no repros to check. Find some with `reproit fuzz`, then `reproit keep`.");
    }
    let all_devices = enumerate_devices().await;
    let mut worst = repro::Outcome::Pass;
    // target label -> the set of repro ids that were RED (non-pass) on it.
    let mut red_per_target: Vec<(String, std::collections::BTreeSet<String>)> = Vec::new();
    let mut results = Vec::new();
    for rt in targets {
        let label = rt.label();
        let mut env = Vec::new();
        match rt {
            crate::domain::target::RunTarget::Engine(engine) => {
                ctx.say(format!("target {label}: web engine ({engine})"));
                env.push(("REPROIT_PLATFORM".to_string(), "web".to_string()));
                env.push(("REPROIT_ENGINE".to_string(), engine.clone()));
            }
            crate::domain::target::RunTarget::Platform(t) => {
                let chosen = device
                    .and_then(|want| {
                        all_devices
                            .iter()
                            .find(|d| d.target == *t && (d.name == want || d.id == want))
                    })
                    .or_else(|| all_devices.iter().find(|d| d.target == *t && d.booted))
                    .or_else(|| all_devices.iter().find(|d| d.target == *t));
                match chosen {
                    Some(d) => {
                        ctx.say(format!("target {label}: device {} ({})", d.name, d.id));
                        env.push(("REPROIT_DEVICE".to_string(), d.id.clone()));
                    }
                    None => ctx.say(format!(
                        "target {label}: no device found; using the config default platform (real \
                         dual-device check needs a booted {label} device)"
                    )),
                }
                env.push(("REPROIT_PLATFORM".to_string(), t.as_str().to_string()));
            }
        }
        let _guard = ScopedEnv::set(env);
        ctx.say(format!("\n=== target {label} ==="));
        let mut red = std::collections::BTreeSet::new();
        for meta in &metas {
            let lbl = repro_label(meta);
            let (result, run_dir) = check_repro(
                loaded,
                &meta.id,
                times,
                devices,
                kind,
                None,
                ctx.json || ctx.quiet,
                None,
                record_video,
            )
            .await?;
            worst = worst.max(result.outcome);
            if result.outcome != repro::Outcome::Pass {
                red.insert(meta.id.clone());
            }
            ctx.say(format!(
                "  {} {} ({})",
                result.outcome.as_str().to_uppercase(),
                lbl,
                result.rate()
            ));
            results.push(serde_json::json!({
                "id": repro::display_repro_id(&meta.id),
                "kind": "repro",
                "target": label,
                "outcome": result.outcome.as_str(),
                "rate": result.rate(),
                "evidence": run_dir.to_string_lossy(),
            }));
        }
        red_per_target.push((label, red));
        // _guard drops here, restoring the prior env.
    }
    // Divergence: a repro red on a subset of targets (not all) is a divergence.
    let diverging = crate::domain::target::cross_target_divergence(&red_per_target);
    if diverging.is_empty() {
        ctx.say("\ndivergence: none (every repro behaves the same on all targets)");
    } else {
        ctx.say("\ndivergence: repros that differ across targets:");
        for (id, on) in &diverging {
            let label = metas
                .iter()
                .find(|m| &m.id == id)
                .map(repro_label)
                .unwrap_or_else(|| repro::display_repro_id(id));
            ctx.say(format!("  {label} fails only on: {}", on.join(", ")));
        }
    }
    ctx.emit(&serde_json::json!({
        "command": "check",
        "repros": results,
        "outcome": worst.as_str(),
        "exit": worst.exit_code(),
        "divergence": diverging
            .iter()
            .map(|(id, on)| serde_json::json!({
                "id": repro::display_repro_id(id),
                "kind": "repro",
                "fails_only_on": on
            }))
            .collect::<Vec<_>>(),
    }));
    ctx.say(format!(
        "\ncheck: {} ({} repro(s) x {} target(s))",
        worst.as_str().to_uppercase(),
        metas.len(),
        targets.len()
    ));
    Ok(exit_with(Exit::from(worst)))
}

/// Run `fuzz` against one or more run targets, routing each to its own driver
/// and diffing for divergence when more than one target is given. ONE path now
/// handles both web ENGINES and PLATFORMS (see `crate::domain::target::RunTarget`):
///
///   - `RunTarget::Engine(e)` (chromium/firefox/webkit) routes through the web
///     backend (the WebCdp runner reads `REPROIT_ENGINE`), forcing the web
///     platform for the run. `fuzz --target chromium,firefox,webkit` thus runs
///     the SAME seeded walk on each engine and diffs the findings.
///   - `RunTarget::Platform(t)` (ios/android/web) resolves a device from the
///     platform's own device list (simctl/adb/flutter) and runs the loop on it.
///     For mobile (ios/android) the device-resolution + per-target dispatch +
///     divergence diff are exercised here and unit-tested, but a full
///     dual-REAL-device run is infra-gated: it needs two booted devices (a
///     simulator/emulator or a tethered handset) present on the host. When none
///     is found we fall back to the config default platform and say so.
///
/// Every target's run gets a SEPARATE driver invocation: the per-target env
/// (REPROIT_PLATFORM / REPROIT_DEVICE / REPROIT_ENGINE) is set, the run loop
/// executes, then the env is restored so a later target never sees a stale
/// value. With multiple targets, a finding signature present on a SUBSET of
/// targets (some but not all) is reported as a divergence.
pub(super) async fn run_targets(
    ctx: &Ctx,
    loaded: &config::Loaded,
    targets: &[crate::domain::target::RunTarget],
    device: Option<&str>,
    base: fuzz::FuzzArgs,
) -> Result<ExitCode> {
    let devices = enumerate_devices().await;
    // label -> the set of finding signatures it produced, for the divergence diff.
    let mut per_target: Vec<(String, std::collections::BTreeSet<String>)> = Vec::new();
    for rt in targets {
        let label = rt.label();
        // Build this target's per-run env. RAII-restored after the run (Drop) so
        // a panic mid-target cannot leak a stale REPROIT_* into the next target.
        let mut env = Vec::new();
        match rt {
            crate::domain::target::RunTarget::Engine(engine) => {
                // Cross-engine differential: force the web platform and select
                // the engine. The web runner reads REPROIT_ENGINE; REPROIT_URL
                // carries the page (flag/config). Headless is the CI default.
                ctx.say(format!("target {label}: web engine ({engine})"));
                env.push(("REPROIT_PLATFORM".to_string(), "web".to_string()));
                env.push(("REPROIT_ENGINE".to_string(), engine.clone()));
            }
            crate::domain::target::RunTarget::Platform(t) => {
                // Resolve the device for this platform: an explicit --device that
                // belongs to it, else the first booted device, else the first
                // device. None -> config default platform (mobile dual-real-device
                // runtime is infra-gated; see the doc comment).
                let chosen = device
                    .and_then(|want| {
                        devices
                            .iter()
                            .find(|d| d.target == *t && (d.name == want || d.id == want))
                    })
                    .or_else(|| devices.iter().find(|d| d.target == *t && d.booted))
                    .or_else(|| devices.iter().find(|d| d.target == *t));
                match chosen {
                    Some(d) => {
                        ctx.say(format!("target {label}: device {} ({})", d.name, d.id));
                        env.push(("REPROIT_DEVICE".to_string(), d.id.clone()));
                    }
                    None => ctx.say(format!(
                        "target {label}: no device found; using the config default platform (real \
                         dual-device runtime needs a booted {label} device)"
                    )),
                }
                env.push(("REPROIT_PLATFORM".to_string(), t.as_str().to_string()));
            }
        }
        let _guard = ScopedEnv::set(env);
        ctx.say(format!("\n=== target {label} ==="));
        let result = fuzz::fuzz_targeted(&loaded.config, &loaded.root, &base).await?;
        let complete = result.complete;
        per_target.push((label, result.signatures));
        if !complete {
            return Ok(exit_with(Exit::Regression));
        }
        // _guard drops here, restoring the prior env.
    }
    report_divergence(ctx, &per_target);
    Ok(ExitCode::SUCCESS)
}

/// Print the cross-target divergence report from per-target finding signatures.
/// A finding on every target is consistent; a finding on a subset is
/// divergence.
pub(super) fn report_divergence(
    ctx: &Ctx,
    per_target: &[(String, std::collections::BTreeSet<String>)],
) {
    if per_target.len() < 2 {
        return;
    }
    let diverging = crate::domain::target::cross_target_divergence(per_target);
    if diverging.is_empty() {
        ctx.say("\ndivergence: none (every finding reproduces on all targets)");
    } else {
        ctx.say("\ndivergence: findings that differ across targets:");
        for (sig, on) in &diverging {
            ctx.say(format!("  [{}] only on: {}", sig, on.join(", ")));
        }
    }
}

/// Whether a `--target` string names ONLY web browser engines (so `fuzz`/`check
/// --target` routes to the cross-engine differential). A list is web-engine iff
/// every non-empty token is an engine alias; a bare `web` is a platform token.
pub(super) fn is_web_engines(target: &str) -> bool {
    let toks: Vec<&str> = target
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    !toks.is_empty()
        && toks
            .iter()
            .all(|t| crate::domain::target::is_web_engine_token(t))
}
