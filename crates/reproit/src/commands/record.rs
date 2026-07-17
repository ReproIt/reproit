//! Recording, capture shrinking, and video playback workflows.

use super::*;
use crate::model::repro;

/// Watch the selected Cloud project for a tester-marked capture, then pull,
/// clean-launch replay, and ddmin it locally. The capture never enters Cloud's
/// confirmed feed unless `check` reaches the exact captured structural state.
pub(super) async fn exploratory_record_session(
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
                    "command": "record",
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
        triage::ReproVerdict::Clean
        | triage::ReproVerdict::Stale
        | triage::ReproVerdict::Unknown => {
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
            let (result, _) =
                check_repro(loaded, &meta.id, 1, 1, kind, None, true, Some(&candidate)).await?;
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

    let (final_check, _) =
        check_repro(loaded, &meta.id, 2, 1, kind, None, true, Some(&current)).await?;
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
/// under `.reproit/runs/` (the one you just produced with `record <id>`), which
/// we then copy into the per-id slot. Bails with a how-to if neither exists.
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
    anyhow::bail!("no recording for `{id_or_alias}`. Make one with:  reproit record {id_or_alias}")
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
