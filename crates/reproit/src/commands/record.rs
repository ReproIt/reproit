//! Recording, capture shrinking, and video playback workflows.

use super::repro::{adopt_simplified, check_repro};
use super::*;
use crate::model::repro;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureChannel {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OriginalCaptureManifest {
    schema_version: u32,
    id: String,
    kind: &'static str,
    immutable_original: bool,
    created: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    mode: &'static str,
    platform: String,
    target: String,
    video: CaptureChannel,
    actions: CaptureChannel,
    state_graph: CaptureChannel,
    action_count: usize,
    file_sha256: std::collections::BTreeMap<String, String>,
    environment: serde_json::Value,
    oracle: Option<String>,
    verification: &'static str,
    derivation: serde_json::Value,
    upload: serde_json::Value,
}

pub(super) struct OriginalCapture {
    pub(super) id: String,
    pub(super) path: PathBuf,
}

/// Preserve the tester's original experience rather than attempting to prove a
/// machine-detected finding. The original is deliberately not a `repro::Meta`:
/// it has no oracle and must never enter `check` or the regression suite until
/// a separately derived replay has been verified.
pub(super) async fn human_create_session(
    config_path: Option<&Path>,
    attach: bool,
    title: Option<&str>,
    actions_file: Option<&Path>,
    record_video: bool,
    ctx: &Ctx,
) -> Result<OriginalCapture> {
    let loaded = config::load(config_path).with_context(|| {
        "create needs a runnable reproit.yaml; run `reproit init` in the app checkout"
    })?;
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("creating a human repro requires an interactive terminal");
    }

    let captures = layout::captures_dir(&loaded.root);
    std::fs::create_dir_all(&captures)?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let staging = captures.join(format!(".staging-{stamp}-{}", std::process::id()));
    std::fs::create_dir(&staging)?;

    let target = if attach {
        "attached-current-app".to_string()
    } else {
        capture_target(&loaded)?
    };
    let video_path = staging.join("original.mov");
    let (mut recorder, video_unavailable) = if record_video {
        match start_human_screen_recording(&video_path) {
            Ok(child) => (Some(child), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, Some("video was not requested".to_string()))
    };
    if !attach {
        if let Err(error) = launch_capture_target(&loaded, &target) {
            if let Some(child) = recorder.as_mut() {
                stop_human_screen_recording(child).await;
            }
            return Err(error).with_context(|| {
                format!(
                    "launch failed after capture staging began; private staging remains at {}",
                    staging.display()
                )
            });
        }
    }

    if !ctx.json {
        ctx.say(format!(
            "Creating bug report ({})...",
            if attach {
                "attached app"
            } else {
                "launched app"
            }
        ));
        if recorder.is_some() {
            ctx.say("  main-display video is active; only the original will be preserved");
        } else if let Some(reason) = video_unavailable.as_deref() {
            ctx.say(format!("  screen video unavailable: {reason}"));
        }
        ctx.say("  reproduce the bug normally, then return here and press Enter to stop");
        ctx.say("  Repro It will not shrink, replay, or require an oracle for this capture");
    }
    let mut line = String::new();
    let input_result = std::io::stdin().read_line(&mut line);
    if let Some(child) = recorder.as_mut() {
        stop_human_screen_recording(child).await;
    }
    input_result.context("waiting for the demonstration stop signal")?;

    let mut action_count = 0usize;
    let mut has_states = false;
    let action_dest = staging.join("actions.json");
    if let Some(source) = actions_file {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(source).with_context(|| {
                format!(
                    "reading {}; incomplete private capture remains at {}",
                    source.display(),
                    staging.display()
                )
            })?)
            .with_context(|| {
                format!(
                    "parsing {} as JSON; incomplete private capture remains at {}",
                    source.display(),
                    staging.display()
                )
            })?;
        action_count = capture_action_count(&value).with_context(|| {
            format!(
                "invalid SDK action export; incomplete private capture remains at {}",
                staging.display()
            )
        })?;
        has_states = value
            .as_object()
            .and_then(|object| object.get("states"))
            .is_some_and(|states| states.as_array().is_some_and(|states| !states.is_empty()));
        std::fs::write(&action_dest, serde_json::to_vec_pretty(&value)?)?;
    }

    let video_present = video_path.metadata().is_ok_and(|meta| meta.len() > 0);
    let actions_present = action_count > 0;
    let structural_evidence_present = actions_present || has_states;
    require_capture_evidence(video_present, structural_evidence_present, &staging)?;
    let file_sha256 = hash_capture_files(&staging)?;
    let id = capture_id(&file_sha256, title, attach, &loaded.config.app.platform);
    let public_id = format!("cap_{id}");
    let final_dir = captures.join(&public_id);
    if final_dir.exists() {
        anyhow::bail!(
            "capture {public_id} already exists; original was left at {}",
            staging.display()
        );
    }
    let video_reason = if video_present {
        None
    } else {
        video_unavailable.or_else(|| Some("the recorder produced no finalized video".to_string()))
    };
    let manifest = OriginalCaptureManifest {
        schema_version: 1,
        id: public_id.clone(),
        kind: "human-original",
        immutable_original: true,
        created: chrono::Utc::now().to_rfc3339(),
        title: title.map(str::to_owned),
        mode: if attach { "attach" } else { "launch" },
        platform: loaded.config.app.platform.clone(),
        target: redact_capture_target(&target, &loaded.root),
        video: channel(video_present, "original.mov", video_reason),
        actions: channel(
            actions_present,
            "actions.json",
            (!actions_present).then(|| {
                if actions_file.is_some() {
                    "the SDK export contained no actions".to_string()
                } else {
                    "no SDK action export was supplied".to_string()
                }
            }),
        ),
        state_graph: channel(
            has_states,
            "actions.json",
            (!has_states).then(|| "the SDK export contained no state graph".to_string()),
        ),
        action_count,
        file_sha256,
        environment: capture_environment(&loaded.root),
        oracle: None,
        verification: "not-required",
        derivation: serde_json::json!({
            "isDerived": false,
            "parentCapture": null,
            "policy": "derived replays and minimized repros must reference this capture id"
        }),
        upload: serde_json::json!({
            "status": "local",
            "explicitConsentRequired": true,
            "format": "reproit-human-capture-v1"
        }),
    };
    std::fs::write(
        staging.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::rename(&staging, &final_dir)?;
    make_capture_files_readonly(&final_dir)?;

    if !ctx.json {
        ctx.say(format!("CAPTURED {public_id}"));
        ctx.say(format!("  original: {}", final_dir.display()));
    }
    Ok(OriginalCapture {
        id: public_id,
        path: final_dir,
    })
}

fn capture_target(loaded: &config::Loaded) -> Result<String> {
    let app = &loaded.config.app;
    app.url
        .clone()
        .or_else(|| app.executable.clone())
        .or_else(|| (!app.bundle_id.is_empty()).then(|| app.bundle_id.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the configured platform has no launchable url, executable, or bundleId"
            )
        })
}

fn launch_capture_target(loaded: &config::Loaded, target: &str) -> Result<()> {
    let app = &loaded.config.app;
    if app.url.is_none() && !(cfg!(target_os = "macos") && app.executable.is_none()) {
        std::process::Command::new(target)
            .current_dir(&loaded.root)
            .spawn()
            .with_context(|| format!("launching {target}"))?;
        return Ok(());
    }
    let status = if app.url.is_some() {
        if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg(target).status()
        } else if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "start", ""])
                .arg(target)
                .status()
        } else {
            std::process::Command::new("xdg-open").arg(target).status()
        }
    } else {
        std::process::Command::new("open")
            .args(["-b", target])
            .status()
    };
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => anyhow::bail!("launching configured target exited with {status}"),
        Err(error) => Err(error).with_context(|| format!("launching {target}")),
    }
}

fn start_human_screen_recording(path: &Path) -> Result<std::process::Child> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!(
            "host screen recording is not yet implemented on this OS; create from an \
             --actions-file without --record-video"
        )
    }
    std::process::Command::new("/usr/sbin/screencapture")
        .args(["-v", "-C", "-k", "-D1"])
        .arg(path)
        .spawn()
        .context("starting macOS screen recording (grant Screen Recording permission if prompted)")
}

async fn stop_human_screen_recording(child: &mut std::process::Child) {
    #[cfg(unix)]
    crate::exec::sigint(child.id()).await;
    #[cfg(not(unix))]
    let _ = child.kill();
    for _ in 0..50 {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn capture_action_count(value: &serde_json::Value) -> Result<usize> {
    if let Some(actions) = value.as_array() {
        return Ok(actions.len());
    }
    let object = value
        .as_object()
        .context("SDK export must be an array or an object containing actions or states")?;
    if let Some(actions) = object.get("actions") {
        return actions
            .as_array()
            .map(Vec::len)
            .context("SDK export actions must be an array");
    }
    if object
        .get("states")
        .is_some_and(serde_json::Value::is_array)
    {
        return Ok(0);
    }
    anyhow::bail!("SDK export object must contain an actions or states array")
}

fn channel(present: bool, path: &str, reason: Option<String>) -> CaptureChannel {
    CaptureChannel {
        status: if present { "captured" } else { "unavailable" },
        path: present.then(|| path.to_string()),
        reason: (!present).then_some(reason).flatten(),
    }
}

fn require_capture_evidence(video: bool, actions: bool, staging: &Path) -> Result<()> {
    if video || actions {
        return Ok(());
    }
    anyhow::bail!(
        "capture stopped without any saved evidence; no video finalized and no SDK action export \
         was supplied. Incomplete private staging remains at {}",
        staging.display()
    )
}

fn hash_capture_files(dir: &Path) -> Result<std::collections::BTreeMap<String, String>> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()?;
    entries.sort();
    let mut hashes = std::collections::BTreeMap::new();
    for path in entries.into_iter().filter(|path| path.is_file()) {
        let bytes = std::fs::read(&path)?;
        hashes.insert(
            path.file_name().unwrap().to_string_lossy().into_owned(),
            hex_digest(Sha256::digest(bytes).as_slice()),
        );
    }
    Ok(hashes)
}

fn redact_capture_target(target: &str, root: &Path) -> String {
    let Some((scheme, rest)) = target.split_once("://") else {
        let path = Path::new(target);
        if let Ok(relative) = path.strip_prefix(root) {
            return relative.display().to_string();
        }
        return if path.is_absolute() {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "configured-executable".to_string())
        } else {
            target.to_string()
        };
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let suffix = &rest[authority_end..];
    let path_end = suffix.find(['?', '#']).unwrap_or(suffix.len());
    let path = &suffix[..path_end];
    format!(
        "{scheme}://{authority}{}",
        if path.is_empty() { "/" } else { path }
    )
}

fn capture_id(
    hashes: &std::collections::BTreeMap<String, String>,
    title: Option<&str>,
    attach: bool,
    platform: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"reproit-human-capture-v1\n");
    digest.update(platform.as_bytes());
    digest.update(if attach { b"\nattach\n" } else { b"\nlaunch\n" });
    if let Some(title) = title {
        digest.update(title.trim().as_bytes());
    }
    for (path, hash) in hashes {
        digest.update(path.as_bytes());
        digest.update(hash.as_bytes());
    }
    hex_digest(digest.finalize().as_slice())[..12].to_string()
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn capture_environment(root: &Path) -> serde_json::Value {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    serde_json::json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "reproitVersion": crate::VERSION,
        "gitCommit": git(&["rev-parse", "HEAD"]),
        "gitDirty": git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty()),
    })
}

fn make_capture_files_readonly(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() {
            let mut permissions = std::fs::metadata(&path)?.permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(path, permissions)?;
        }
    }
    Ok(())
}

/// Watch the selected Cloud project for a tester-marked capture, then pull,
/// clean-launch replay, and ddmin it locally. The capture never enters Cloud's
/// confirmed feed unless `check` reaches the exact captured structural state.
pub(super) async fn exploratory_create_session(
    config_path: Option<&Path>,
    app: Option<String>,
    timeout_secs: u64,
    kind: Option<&str>,
    ctx: &Ctx,
) -> Result<ExitCode> {
    let loaded = config::load(config_path).with_context(|| {
        "exploratory recording needs the app source and a runnable reproit.yaml; run `reproit \
         init` in the source checkout"
    })?;
    let app = cloud_app_id(app)?;
    let (cloud, key) = cloud_creds(None, None);
    let initial = triage::pending_captures(&app, cloud.clone(), key.clone()).await?;
    let mut seen: std::collections::HashMap<String, u64> = initial
        .iter()
        .filter_map(|item| {
            Some((
                item["bucketId"].as_str()?.to_string(),
                item["count"].as_u64().unwrap_or(0),
            ))
        })
        .collect();

    if !ctx.json {
        ctx.say(format!("Exploratory capture armed for {app}."));
        ctx.say("  use the debug build normally");
        ctx.say("  web: press Alt+Shift+B when the bug is visible");
        ctx.say("  native: call ReproIt.captureBug() from the app's debug menu");
        ctx.say("  waiting for a new capture...");
    }

    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs.max(1));
    let captured = loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "no tester capture arrived within {timeout_secs}s; the app must use this \
                 project's SDK key and have tester capture enabled"
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let pending = triage::pending_captures(&app, cloud.clone(), key.clone()).await?;
        if let Some(item) = pending.iter().find(|item| {
            let Some(bucket) = item["bucketId"].as_str() else {
                return false;
            };
            item["count"].as_u64().unwrap_or(0) > seen.get(bucket).copied().unwrap_or(0)
        }) {
            break item.clone();
        }
        for item in pending {
            if let Some(bucket) = item["bucketId"].as_str() {
                seen.insert(bucket.to_string(), item["count"].as_u64().unwrap_or(0));
            }
        }
    };

    let bucket = captured["bucketId"]
        .as_str()
        .context("Cloud returned a capture without a bucket id")?
        .to_string();
    ctx.say(format!(
        "  captured {bucket}; verifying on a clean launch..."
    ));
    let verdict = triage::verify_tester_capture(
        &loaded.root,
        &app,
        &bucket,
        &bucket,
        ctx.json,
        cloud.clone(),
        key.clone(),
    )
    .await?;

    match verdict {
        triage::ReproVerdict::Reproduced => {
            let meta = shrink_tester_capture(&loaded, &bucket, kind, ctx).await?;
            let public = repro::display_repro_id(&meta.id);
            triage::report_tester_capture(
                &app,
                &bucket,
                &public,
                triage::ReproVerdict::Reproduced,
                2,
                cloud,
                key,
            )
            .await?;
            if ctx.json {
                ctx.emit(&serde_json::json!({
                    "command": "create",
                    "status": "confirmed",
                    "bucket": bucket,
                    "repro": public,
                    "actions": load_repro_actions(&loaded, &meta.id)?.len(),
                }));
            } else {
                ctx.say(format!("CONFIRMED {bucket}"));
                ctx.say(format!("  local repro: {public}"));
                ctx.say(format!("  reproduce: reproit {public}"));
            }
            Ok(ExitCode::SUCCESS)
        }
        triage::ReproVerdict::Flaky => {
            triage::report_tester_capture(
                &app,
                &bucket,
                &bucket,
                triage::ReproVerdict::Flaky,
                2,
                cloud,
                key,
            )
            .await?;
            ctx.say(format!("  {bucket} remains pending: replay was unstable"));
            Ok(exit_with(Exit::Flaky))
        }
        triage::ReproVerdict::NotReproduced
        | triage::ReproVerdict::Stale
        | triage::ReproVerdict::CouldNotReplay => {
            triage::report_tester_capture(&app, &bucket, &bucket, verdict, 1, cloud, key).await?;
            ctx.say(format!(
                "  {bucket} remains pending: the captured structural state did not reproduce"
            ));
            Ok(exit_with(Exit::Stale))
        }
    }
}

/// Delta-debug a confirmed tester path. Candidate removals are accepted only
/// when the tester-capture oracle still reaches the exact captured state. A
/// final two-run check prevents a flaky candidate from replacing the original.
async fn shrink_tester_capture(
    loaded: &config::Loaded,
    alias: &str,
    kind: Option<&str>,
    ctx: &Ctx,
) -> Result<repro::Meta> {
    let meta = repro::resolve(&loaded.root, alias)
        .with_context(|| format!("the confirmed capture `{alias}` was not saved locally"))?;
    if meta.oracle.as_deref() != Some("tester-capture") {
        return Ok(meta);
    }
    let mut current = load_repro_actions(loaded, &meta.id)?;
    if !ctx.json {
        ctx.say(format!(
            "  shrinking {} captured action(s)...",
            current.len()
        ));
    }
    let mut granularity = 2usize;
    let mut replays = 0usize;
    const MAX_REPLAYS: usize = 20;
    while !current.is_empty() && replays < MAX_REPLAYS {
        let chunk = current.len().div_ceil(granularity);
        let mut removed = false;
        let mut start = 0usize;
        while start < current.len() && replays < MAX_REPLAYS {
            let end = (start + chunk).min(current.len());
            let candidate: Vec<String> = current[..start]
                .iter()
                .chain(current[end..].iter())
                .cloned()
                .collect();
            replays += 1;
            let (result, _) = check_repro(
                loaded,
                &meta.id,
                1,
                1,
                kind,
                None,
                true,
                Some(&candidate),
                false,
            )
            .await?;
            if result.outcome == repro::Outcome::Fail {
                current = candidate;
                removed = true;
                granularity = 2;
                break;
            }
            start += chunk;
        }
        if !removed {
            if granularity >= current.len() {
                break;
            }
            granularity = (granularity * 2).min(current.len());
        }
    }

    let (final_check, _) = check_repro(
        loaded,
        &meta.id,
        2,
        1,
        kind,
        None,
        true,
        Some(&current),
        false,
    )
    .await?;
    if final_check.outcome != repro::Outcome::Fail {
        anyhow::bail!("the minimized tester capture was not deterministic; keeping it pending");
    }
    let new_id = repro::repro_id(meta.seed, &current);
    adopt_simplified(loaded, &meta, &current, &new_id)?;
    let minimized = repro::resolve(&loaded.root, alias)
        .or_else(|| repro::load_meta(&loaded.root, &new_id))
        .context("the minimized tester capture was not saved")?;
    if !ctx.json {
        ctx.say(format!(
            "  minimized to {} action(s) in {replays} candidate replay(s)",
            current.len()
        ));
    }
    Ok(minimized)
}

/// Prefer a direct screen URL for recordings. Legacy repros lack this metadata
/// and retain their original full replay unchanged.
#[cfg(test)]
pub(super) fn minimize_record_replay(replay: &mut serde_json::Value, meta: &repro::Meta) {
    let Some(url) = meta.record_url.as_ref() else {
        return;
    };
    let Some(obj) = replay.as_object_mut() else {
        return;
    };
    obj.insert("gotoUrl".into(), serde_json::Value::String(url.clone()));
    let actions = meta
        .record_action
        .iter()
        .cloned()
        .map(serde_json::Value::String)
        .collect();
    obj.insert("replay".into(), serde_json::Value::Array(actions));
}

pub(super) fn web_record_metadata(
    app_url: Option<&str>,
    oracle: Option<&str>,
    sig: Option<&str>,
    log: &str,
) -> (Option<String>, Option<String>) {
    let (Some(app_url), Some(oracle), Some(sig)) = (app_url, oracle, sig) else {
        return (None, None);
    };
    let state_present = matches!(
        oracle,
        "content-bug"
            | "choice-anomaly"
            | "broken-route"
            | "occlusion"
            | "security"
            | "stuck-keyboard"
            | "blank-screen"
            | "broken-asset"
            | "zoom-reflow"
            | "invariant"
            | "safe-area"
    );
    if !state_present && oracle != "flicker" {
        return (None, None);
    }
    let obs = crate::model::map::parse_run(log);
    let Some(route) = obs.routes.get(sig) else {
        return (None, None);
    };
    let Some(origin) = app_url_origin(app_url) else {
        return (None, None);
    };
    let url = format!("{origin}{route}");
    if state_present {
        return (Some(url), None);
    }
    let action = obs
        .rerenders
        .keys()
        .chain(obs.paint_flickers.keys())
        .find_map(|(from, action)| (from == sig).then(|| action.clone()));
    match action {
        Some(action) => (Some(url), Some(action)),
        None => (None, None),
    }
}

fn app_url_origin(url: &str) -> Option<&str> {
    let authority = url.find("://")? + 3;
    let end = url[authority..]
        .find(['/', '?', '#'])
        .map(|i| authority + i)
        .unwrap_or(url.len());
    Some(&url[..end])
}

/// Video container extensions reproit's backends can emit: Playwright writes
/// `.webm`, the sim/native tier `.mov`, and the annotated delivery clip `.mp4`.
const VIDEO_EXTS: [&str; 3] = ["mp4", "mov", "webm"];

fn is_video(p: &Path) -> bool {
    match p.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let ext = ext.to_ascii_lowercase();
            VIDEO_EXTS.contains(&ext.as_str())
        }
        None => false,
    }
}

/// Resolve the recording to play for a repro, caching it into the gitignored
/// per-id recording slot so future `watch`es are instant and precise.
///
/// Lookup order: the per-id recording slot
/// (`.reproit/recordings/repro/<id>/video.*`) first; else the newest recording
/// under `.reproit/runs/` (the one just produced with `--record-video`), which
/// is then copied into the per-id slot. Bails with a how-to if neither exists.
/// `.reproit/recordings/` is gitignored, so cached videos can never be
/// committed by accident.
pub(super) fn resolve_repro_video(loaded: &config::Loaded, id_or_alias: &str) -> Result<PathBuf> {
    let root = loaded.root.as_path();
    // Key media by the canonical content-hash id (so an alias and its id share
    // one cached file); pending findings use their public `fnd_...` id.
    let id = if let Some(m) = repro::resolve(root, id_or_alias) {
        m.id
    } else if let Some(id) = repro::raw_finding_id(id_or_alias) {
        id.to_string()
    } else {
        anyhow::bail!(
            "no repro or finding `{id_or_alias}`. Use a saved alias, rep_..., or fnd_..."
        );
    };
    let recording_dir = layout::repro_recording_dir(root, &id);

    // 1. Already cached for this id.
    if let Some(v) = newest_video_in(&recording_dir, Some("video")) {
        return Ok(v);
    }
    // 2. Newest recording from any run; promote it into the per-id recording slot.
    if let Some(src) = newest_video_in(&root.join(&loaded.config.evidence.out_dir), None) {
        std::fs::create_dir_all(&recording_dir)?;
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("webm");
        let dest = layout::repro_video_path(root, &id, ext);
        std::fs::copy(&src, &dest)
            .map_err(|e| anyhow::anyhow!("caching recording to {}: {e}", dest.display()))?;
        return Ok(dest);
    }
    anyhow::bail!(
        "no recording for `{id_or_alias}`. Make one with: reproit @{id_or_alias} --record-video"
    )
}

/// Newest video file under `dir` (recursively), by modification time. When
/// `stem` is set, only files whose name stem equals it are considered (the
/// per-id media slot); when None, any video counts (scanning run dirs).
fn newest_video_in(dir: &Path, stem: Option<&str>) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !is_video(&p) {
                continue;
            }
            if let Some(want) = stem {
                if p.file_stem().and_then(|s| s.to_str()) != Some(want) {
                    continue;
                }
            }
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
                best = Some((mtime, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Open a file in the OS default application (the user's video player). Uses
/// the platform opener directly so there's no extra dependency.
pub(super) fn open_in_player(path: &Path) -> Result<()> {
    println!("  opening {}", path.display());
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(path).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .status()
    } else {
        std::process::Command::new("xdg-open").arg(path).status()
    };
    match result {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => anyhow::bail!(
            "the video player exited with {s} (file: {})",
            path.display()
        ),
        Err(e) => anyhow::bail!(
            "could not launch a video player ({e}). The recording is at:\n  {}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod human_capture_tests {
    use super::*;

    #[test]
    fn action_export_accepts_array_or_structural_object() {
        assert_eq!(
            capture_action_count(&serde_json::json!(["tap:a", "tap:b"])).unwrap(),
            2
        );
        assert_eq!(
            capture_action_count(&serde_json::json!({
                "actions": ["tap:a"], "states": [{"sig": "home"}]
            }))
            .unwrap(),
            1
        );
        assert_eq!(capture_action_count(&serde_json::json!([])).unwrap(), 0);
        assert_eq!(
            capture_action_count(&serde_json::json!({"states": [{"sig": "home"}]})).unwrap(),
            0
        );
        assert!(capture_action_count(&serde_json::json!({"states": "bad"})).is_err());
    }

    #[test]
    fn original_id_is_content_addressed_and_mode_sensitive() {
        let hashes = std::collections::BTreeMap::from([
            ("actions.json".to_string(), "abc".to_string()),
            ("original.mov".to_string(), "def".to_string()),
        ]);
        let first = capture_id(&hashes, Some("broken menu"), false, "web");
        assert_eq!(
            first,
            capture_id(&hashes, Some("broken menu"), false, "web")
        );
        assert_ne!(first, capture_id(&hashes, Some("broken menu"), true, "web"));
        assert_eq!(first.len(), 12);
    }

    #[test]
    fn absent_channel_is_explicitly_unavailable() {
        let value =
            serde_json::to_value(channel(false, "actions.json", Some("not supplied".into())))
                .unwrap();
        assert_eq!(value["status"], "unavailable");
        assert!(value.get("path").is_none());
        assert_eq!(value["reason"], "not supplied");
    }

    #[test]
    fn original_capture_fails_closed_without_any_evidence_channel() {
        let staging = Path::new("/private/capture-staging");
        let error = require_capture_evidence(false, false, staging).unwrap_err();
        assert!(error.to_string().contains("without any saved evidence"));
        assert!(error.to_string().contains("/private/capture-staging"));
        assert!(require_capture_evidence(true, false, staging).is_ok());
        assert!(require_capture_evidence(false, true, staging).is_ok());
    }

    #[test]
    fn uploaded_target_drops_credentials_query_and_fragment() {
        let root = Path::new("/workspace/app");
        assert_eq!(
            redact_capture_target(
                "https://user:secret@example.com/private/path?token=secret#view",
                root
            ),
            "https://example.com/private/path"
        );
        assert_eq!(
            redact_capture_target("https://example.com?key=secret", root),
            "https://example.com/"
        );
        assert_eq!(
            redact_capture_target("/workspace/app/target/debug/app", root),
            "target/debug/app"
        );
        assert_eq!(redact_capture_target("/opt/private/bin/app", root), "app");
        assert_eq!(
            redact_capture_target("./target/debug/app", root),
            "./target/debug/app"
        );
    }
}
