//! Saved-repro and pending-finding verification workflow.

use super::device::{resolve_check_device, run_check_targets};
use super::map::ensure_app_map;
use super::repro::{
    check_label, check_repro, find_finding_by_id, public_json_id, public_json_kind,
};
use crate::adapters::config;
use crate::domain::repro;
use crate::interface::cli::context::{exit_with, Ctx, Exit};
use crate::interface::junit;
use crate::workflows::{a2ui, backend_headless, flicker, journey};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(super) struct CheckArgs {
    pub(super) repro: Option<String>,
    pub(super) devices: usize,
    pub(super) kind: Option<String>,
    pub(super) runs: Option<u32>,
    pub(super) junit: Option<PathBuf>,
    pub(super) strict: bool,
    pub(super) locale: Option<String>,
    pub(super) target: Option<String>,
    pub(super) device: Option<String>,
    pub(super) record_video: bool,
    pub(super) flicker: bool,
    pub(super) changed: Option<String>,
    pub(super) inspect: bool,
}

pub(super) async fn run(
    ctx: &Ctx,
    config_path: Option<&Path>,
    args: CheckArgs,
) -> Result<ExitCode> {
    if args.inspect && args.repro.is_none() {
        anyhow::bail!("inspection needs exactly one saved repro");
    }
    if let Some(id) = args.repro.as_deref() {
        if let Some(code) = backend_headless::try_replay(ctx, id).await? {
            if args.record_video {
                anyhow::bail!("backend repros do not produce screen video evidence");
            }
            return Ok(code);
        }
        if let Some(code) = a2ui::try_replay(ctx, id)? {
            if args.record_video {
                anyhow::bail!("A2UI repros do not produce screen video evidence");
            }
            return Ok(code);
        }
    }
    let loaded = config::load(config_path)?;
    if let Some(reference) = args.repro.as_deref() {
        if routes_to_capture_file(&loaded, reference) {
            if args.record_video {
                anyhow::bail!("backend captures do not produce screen video evidence");
            }
            return backend_headless::check_capture(ctx, Path::new(reference));
        }
    }
    ensure_app_map(ctx, &loaded, "explore").await?;
    let _inspect_env = if args.inspect {
        Some(crate::adapters::scoped_env::ScopedEnv::set(vec![
            ("REPROIT_HEADLESS".to_string(), "0".to_string()),
            ("REPROIT_INSPECT".to_string(), "1".to_string()),
        ]))
    } else {
        None
    };
    if let Some(code) = try_multi_target(ctx, &loaded, &args).await? {
        return Ok(code);
    }
    select_device(ctx, &loaded, &args).await;
    let times = if args.inspect {
        1
    } else {
        args.runs.unwrap_or(loaded.config.gate.runs).max(1)
    };
    if let Some(code) = try_journey(ctx, &loaded, &args, times).await? {
        return Ok(code);
    }
    let mut metas = resolve_metas(ctx, &loaded, args.repro.as_deref())?;
    if let Some(base) = args.changed.as_deref() {
        metas = super::change_selection::prioritize(ctx, &loaded.root, metas, base);
    }
    run_repro_matrix(ctx, &loaded, &args, times, &metas).await
}

/// Whether a check reference routes to the backend capture-file re-evaluation.
/// Pinned precedence (see the disambiguation test): a saved repro or pending
/// finding ALWAYS wins over a same-named file, so a repro whose alias looks
/// like a path still resolves as a repro; the file is routed only when nothing
/// local matches.
fn routes_to_capture_file(loaded: &config::Loaded, reference: &str) -> bool {
    backend_headless::is_capture_file(Path::new(reference))
        && repro::resolve(&loaded.root, reference).is_none()
        && find_finding_by_id(loaded, reference).is_none()
}

async fn try_multi_target(
    ctx: &Ctx,
    loaded: &config::Loaded,
    args: &CheckArgs,
) -> Result<Option<ExitCode>> {
    let Some(raw) = args.target.as_deref() else {
        return Ok(None);
    };
    let (targets, unknown) = crate::domain::target::parse_run_targets(raw);
    for target in unknown {
        ctx.say(format!("  warn: unknown target `{target}` (ignored)"));
    }
    if targets.len() <= 1 {
        return Ok(None);
    }
    if args.flicker {
        anyhow::bail!("--flicker supports one execution target at a time");
    }
    run_check_targets(
        ctx,
        loaded,
        &targets,
        args.device.as_deref(),
        &args.repro,
        args.runs,
        args.devices,
        args.kind.as_deref(),
        args.record_video,
    )
    .await
    .map(Some)
}

async fn select_device(ctx: &Ctx, loaded: &config::Loaded, args: &CheckArgs) {
    let selected = resolve_check_device(
        ctx,
        &loaded.config.app.platform,
        args.target.as_deref(),
        args.device.as_deref(),
    )
    .await;
    if let Some(device) = selected {
        std::env::set_var("REPROIT_PLATFORM", device.target.as_str());
        std::env::set_var("REPROIT_DEVICE", &device.id);
        ctx.say(format!(
            "  device: {} ({})",
            device.name,
            device.target.as_str()
        ));
    }
}

async fn try_journey(
    ctx: &Ctx,
    loaded: &config::Loaded,
    args: &CheckArgs,
    times: u32,
) -> Result<Option<ExitCode>> {
    let Some(reference) = args.repro.as_deref() else {
        return Ok(None);
    };
    if repro::resolve(&loaded.root, reference).is_some()
        || find_finding_by_id(loaded, reference).is_some()
        || !journey::exists(&loaded.root, reference)
    {
        return Ok(None);
    }
    if args.record_video {
        anyhow::bail!("--record-video needs a saved repro or finding id, not a journey name");
    }
    let result = journey::run(loaded, reference, times, ctx.json || ctx.quiet).await?;
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "check",
            "journey": reference,
            "outcome": result.outcome.as_str(),
            "rate": result.rate(),
            "exit": result.outcome.exit_code(),
        }));
    } else {
        ctx.say(format!(
            "\ncheck: {} ({})  journey {reference}",
            result.outcome.as_str().to_uppercase(),
            result.rate()
        ));
    }
    Ok(Some(ExitCode::from(result.outcome.exit_code())))
}

fn resolve_metas(
    ctx: &Ctx,
    loaded: &config::Loaded,
    reference: Option<&str>,
) -> Result<Vec<repro::Meta>> {
    let Some(reference) = reference else {
        let all = repro::list(&loaded.root);
        if !all.is_empty() {
            return Ok(all);
        }
        if ctx.json {
            ctx.emit(&serde_json::json!({
                "command": "check",
                "repros": [],
                "outcome": "pass",
                "exit": 0,
            }));
            return Ok(Vec::new());
        }
        anyhow::bail!("no repros to check. Find some with `reproit fuzz`, then `reproit keep`.");
    };
    let meta = repro::resolve(&loaded.root, reference)
        .or_else(|| find_finding_by_id(loaded, reference).map(|finding| finding.pending_meta()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no repro or finding `{reference}` (by id or alias). List saved bugs with \
                 `reproit bugs`, or find some with `reproit fuzz`."
            )
        })?;
    Ok(vec![meta])
}

async fn run_repro_matrix(
    ctx: &Ctx,
    loaded: &config::Loaded,
    args: &CheckArgs,
    times: u32,
    metas: &[repro::Meta],
) -> Result<ExitCode> {
    if metas.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    let locales = args
        .locale
        .as_deref()
        .map(crate::domain::locale::parse_locales)
        .unwrap_or_default();
    let locale_runs = if locales.is_empty() {
        vec![None]
    } else {
        locales.iter().map(String::as_str).map(Some).collect()
    };
    let mut results = Vec::new();
    let mut cases = Vec::new();
    let mut worst = repro::Outcome::Pass;
    let mut failed_by_id = std::collections::BTreeMap::<String, Vec<String>>::new();
    for locale in &locale_runs {
        if let Some(locale) = locale {
            ctx.say(format!("\n=== locale {locale} ==="));
        }
        for meta in metas {
            let execution = execute_case(ctx, loaded, args, times, meta, *locale).await?;
            worst = worst.max(execution.effective);
            if execution.failed {
                if let Some(locale) = locale {
                    failed_by_id
                        .entry(meta.id.clone())
                        .or_default()
                        .push((*locale).to_string());
                }
            }
            cases.push(execution.case);
            results.push(execution.json);
        }
    }
    report_locale_diff(ctx, metas, locale_runs.len(), &failed_by_id);
    write_junit(ctx, args.junit.as_deref(), &cases);
    ctx.emit(&serde_json::json!({
        "command": "check",
        "repros": results,
        "outcome": worst.as_str(),
        "exit": worst.exit_code(),
    }));
    let verb = if args.inspect { "inspect" } else { "check" };
    ctx.say(format!(
        "\n{verb}: {} ({} repro(s))",
        worst.as_str().to_uppercase(),
        metas.len()
    ));
    Ok(exit_with(Exit::from(worst)))
}

struct CaseExecution {
    effective: repro::Outcome,
    failed: bool,
    case: junit::Case,
    json: serde_json::Value,
}

async fn execute_case(
    ctx: &Ctx,
    loaded: &config::Loaded,
    args: &CheckArgs,
    times: u32,
    meta: &repro::Meta,
    locale: Option<&str>,
) -> Result<CaseExecution> {
    let label = locale.map_or_else(
        || check_label(meta),
        |locale| format!("{} @{locale}", check_label(meta)),
    );
    let verb = if args.inspect { "inspect" } else { "check" };
    ctx.say(format!("{verb} {label}"));
    let (result, run_dir) = check_repro(
        loaded,
        &meta.id,
        times,
        args.devices,
        args.kind.as_deref(),
        locale,
        ctx.json || ctx.quiet,
        None,
        args.record_video,
    )
    .await?;
    let video_flicker = if args.flicker {
        let events = flicker::analyze_run(&run_dir, &flicker::FlickerCfg::default()).await?;
        !flicker::report(&events)
    } else {
        false
    };
    // Video analysis is supporting evidence. It must never replace the exact
    // repro's detector verdict or report an unrelated visual signal as this bug.
    let outcome = result.outcome;
    let blocks = args.strict || args.repro.is_some() || meta.status != repro::Status::Quarantined;
    let effective = if blocks {
        outcome
    } else {
        repro::Outcome::Pass
    };
    let mut updated = meta.clone();
    let promoted = !args.inspect
        && outcome == repro::Outcome::Pass
        && meta.status == repro::Status::Quarantined;
    if !args.inspect {
        updated.last_checked = Some(chrono::Local::now().to_rfc3339());
        updated.last_result = Some(outcome.as_str().to_string());
        if promoted {
            updated.status = repro::Status::Required;
        }
        repro::save_meta(&loaded.root, &updated)?;
    }
    ctx.say(format!(
        "  {} {} ({}){}",
        outcome.as_str().to_uppercase(),
        label,
        result.rate(),
        if promoted {
            "  promoted -> required"
        } else {
            ""
        }
    ));
    if args.inspect {
        super::inspect::write_fix_packet(loaded, meta, &result, &run_dir)?;
    }
    let case = junit::Case {
        name: format!("{verb} {label}"),
        passed: outcome == repro::Outcome::Pass,
        time_s: 0.0,
        message: format!(
            "{} ({}); evidence: {}",
            outcome.as_str(),
            result.rate(),
            run_dir.display()
        ),
    };
    let json = serde_json::json!({
        "id": public_json_id(meta),
        "kind": public_json_kind(meta),
        "alias": meta.alias,
        "locale": locale,
        "outcome": outcome.as_str(),
        "rate": result.rate(),
        "green": result.green,
        "total": result.total,
        "status": updated.status.as_str(),
        "promoted": promoted,
        "exit": outcome.exit_code(),
        "evidence": run_dir.to_string_lossy(),
        "videoFlicker": video_flicker,
    });
    Ok(CaseExecution {
        effective,
        failed: outcome != repro::Outcome::Pass,
        case,
        json,
    })
}

fn report_locale_diff(
    ctx: &Ctx,
    metas: &[repro::Meta],
    locale_count: usize,
    failed_by_id: &std::collections::BTreeMap<String, Vec<String>>,
) {
    if locale_count <= 1 {
        return;
    }
    let mut any = false;
    for meta in metas {
        let Some(failed) = failed_by_id.get(&meta.id) else {
            continue;
        };
        if failed.len() >= locale_count {
            continue;
        }
        if !any {
            ctx.say("\nlocale diff: locale-specific failures (i18n):");
            any = true;
        }
        ctx.say(format!(
            "  {} fails only in: {}",
            check_label(meta),
            failed.join(", ")
        ));
    }
    if !any {
        ctx.say("\nlocale diff: no locale-specific failures");
    }
}

fn write_junit(ctx: &Ctx, path: Option<&Path>, cases: &[junit::Case]) {
    let Some(path) = path else {
        return;
    };
    if let Err(error) = junit::write(path, "check", cases) {
        ctx.say(format!(
            "  warn: could not write junit {}: {error}",
            path.display()
        ));
    } else {
        ctx.say(format!("  junit: {}", path.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn loaded_at(root: PathBuf) -> config::Loaded {
        config::parse_str(
            "app:\n  platform: web\n  webRunnerDir: ./runners/web\n  url: http://localhost:3000\n\
             devices:\n  namePrefix: reproit\n\
             journeys:\n  dir: journeys\n  driver: explore\n  doneMarkers: [DONE]\n\
             evidence:\n  outDir: .reproit/runs\n  video: false\n",
            root,
        )
        .unwrap()
    }

    fn write_capture(path: &Path) {
        std::fs::write(
            path,
            json!({
                "format": "reproit-backend-capture",
                "version": 1,
                "operation": "createOrder",
                "oracle": "backend-server-error",
                "events": [{
                    "traceId": "t", "spanId": "t:createOrder", "actionIndex": 0,
                    "operation": "createOrder", "sequence": 1, "kind": "start",
                    "input": {}
                }]
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn capture_file_reference_routes_to_the_capture_re_evaluation() {
        let root = std::env::temp_dir().join(format!("reproit-check-cap-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("capture.json");
        write_capture(&file);
        let loaded = loaded_at(root.clone());
        assert!(routes_to_capture_file(&loaded, file.to_str().unwrap()));
        // A reference that is not a capture file never routes.
        assert!(!routes_to_capture_file(&loaded, "@login-crash"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The pinned disambiguation: a saved repro whose alias names an existing
    /// capture file still resolves as the repro; the file is only routed when
    /// nothing local matches.
    #[test]
    fn saved_repro_wins_over_a_same_named_capture_file() {
        let root = std::env::temp_dir().join(format!("reproit-check-amb-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("capture.json");
        write_capture(&file);
        let reference = file.to_str().unwrap().to_string();
        let loaded = loaded_at(root.clone());
        assert!(routes_to_capture_file(&loaded, &reference));
        let meta = repro::Meta {
            id: repro::repro_id(0, &["tap:key:save"]),
            alias: Some(reference.clone()),
            status: repro::Status::Quarantined,
            seed: 0,
            created: "2026-07-24T00:00:00+00:00".into(),
            last_checked: None,
            last_result: None,
            trigger_index: Some(1),
            trigger_sig: None,
            trigger_selector: None,
            trigger_fingerprint: None,
            oracle: Some("crash".into()),
            record_url: None,
            record_action: None,
        };
        repro::save_meta(&root, &meta).unwrap();
        assert!(!routes_to_capture_file(&loaded, &reference));
        let _ = std::fs::remove_dir_all(root);
    }
}
