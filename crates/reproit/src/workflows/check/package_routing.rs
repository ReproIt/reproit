//! Route a saved guard to its replay mechanism, fail closed.
//!
//! A guard directory replays as exactly one of: a compiled reproduction plan,
//! a kept process capsule, or a kept hermetic backend capture. Suite runs use
//! these routes so every guard format checks under plain `reproit check`; a
//! guard with no route is a corpus error, never a skip. A guard whose typed
//! environment requirement does not hold on this host is NOT APPLICABLE:
//! reported loudly and never counted as an executed run, let alone a pass.

use super::{CaseExecution, CheckArgs};
use crate::domain::repro;
use crate::interface::cli::context::{Ctx, Exit};
use crate::workflows::backend_headless;
use anyhow::Result;
use std::path::Path;
use std::process::ExitCode;

/// The two kept-guard formats that replay through a stored exec recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeptRoute {
    /// Kept process capsule (capsule.json + hermetic.json exec recipe).
    Process,
    /// Kept hermetic backend capture (capture.json + hermetic.json).
    Hermetic,
}

pub(super) fn kept_route(root: &Path, meta: &repro::Meta) -> Option<KeptRoute> {
    let directory = repro::repro_dir(root, &meta.id);
    if !directory.join("hermetic.json").is_file() {
        return None;
    }
    if directory.join("capsule.json").is_file() {
        return Some(KeptRoute::Process);
    }
    if directory.join("capture.json").is_file() {
        return Some(KeptRoute::Hermetic);
    }
    None
}

/// The loud non-execution for a guard whose environment requirement does not
/// hold here. A case that did not execute must never read as a pass; the
/// effective outcome stays Pass so the guard gates only where it runs.
pub(super) fn not_applicable_execution(ctx: &Ctx, meta: &repro::Meta) -> Option<CaseExecution> {
    if meta.applicable_here() {
        return None;
    }
    let requirement = meta
        .requires
        .as_ref()
        .map(repro::Requires::describe)
        .unwrap_or_default();
    ctx.say(format!(
        "  NOT APPLICABLE {} (requires {requirement}; not executed on this host)",
        super::check_label(meta)
    ));
    Some(CaseExecution {
        effective: repro::Outcome::Pass,
        failed: false,
        executed: false,
        json: serde_json::json!({
            "id": repro::display_repro_id(&meta.id),
            "alias": meta.alias,
            "outcome": "not_applicable",
            "requires": requirement,
            "status": meta.status.as_str(),
        }),
    })
}

/// Replay routes speak the stable exit contract (context.rs): 0 pass,
/// 1 regression, 2 flaky, 3 stale. Anything else is a broken replay and
/// gates as a regression, never as a pass.
fn outcome_from_exit(code: ExitCode) -> repro::Outcome {
    if code == ExitCode::from(Exit::Clean as u8) {
        repro::Outcome::Pass
    } else if code == ExitCode::from(Exit::Flaky as u8) {
        repro::Outcome::Flaky
    } else if code == ExitCode::from(Exit::Stale as u8) {
        repro::Outcome::Stale
    } else {
        repro::Outcome::Fail
    }
}

/// Replay a kept process-capsule or hermetic-capture guard as one suite case,
/// with the same status lifecycle as a plan guard (a quarantined keep still
/// auto-promotes to required on its first green).
pub(super) async fn execute_kept_guard(
    ctx: &Ctx,
    root: &Path,
    args: &CheckArgs,
    meta: &repro::Meta,
    locale: Option<&str>,
    route: KeptRoute,
) -> Result<CaseExecution> {
    if locale.is_some() {
        anyhow::bail!("locale matrices cannot override a kept guard's stored exec recipe");
    }
    let reference = repro::display_repro_id(&meta.id);
    let code = match route {
        KeptRoute::Process => {
            crate::workflows::process_capsule::try_replay_process_guard(ctx, &reference).await?
        }
        KeptRoute::Hermetic => {
            backend_headless::try_replay_hermetic_guard(ctx, &reference, true).await?
        }
    };
    let Some(code) = code else {
        anyhow::bail!("guard {reference} stopped routing as a kept guard mid-suite");
    };
    let outcome = outcome_from_exit(code);
    let blocks = args.repro.is_some() || meta.status != repro::Status::Quarantined;
    let effective = if blocks {
        outcome
    } else {
        repro::Outcome::Pass
    };
    let (updated, promoted) = super::mark_checked(root, meta, outcome)?;
    let label = super::check_label(meta);
    if promoted {
        ctx.say(format!("  {label} promoted -> required"));
    }
    Ok(CaseExecution {
        effective,
        failed: outcome != repro::Outcome::Pass,
        executed: true,
        json: serde_json::json!({
            "id": reference,
            "alias": meta.alias,
            "outcome": outcome.as_str(),
            "status": updated.status.as_str(),
            "promoted": promoted,
            "exit": outcome.exit_code(),
        }),
    })
}

pub(super) fn has_compiled_plan(root: &Path, meta: &repro::Meta) -> bool {
    let directory = repro::repro_dir(root, &meta.id);
    let Ok(package) = std::fs::read(directory.join("package.json")) else {
        return false;
    };
    let Ok(package) = serde_json::from_slice::<serde_json::Value>(&package) else {
        return false;
    };
    package
        .get("plan")
        .is_some_and(serde_json::Value::is_object)
        && directory.join("plan.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_action_package_does_not_override_standard_replay() {
        let root = std::env::temp_dir().join(format!(
            "reproit-check-package-routing-{}",
            std::process::id()
        ));
        let meta = repro::Meta {
            id: repro::repro_id(0, &["tap:key:save"]),
            alias: Some("cloud-action-replay".into()),
            status: repro::Status::Quarantined,
            seed: 0,
            created: "2026-07-29T00:00:00Z".into(),
            last_checked: None,
            last_result: None,
            trigger_index: Some(1),
            trigger_sig: Some("crash:save".into()),
            trigger_selector: None,
            trigger_fingerprint: None,
            oracle: Some("crash".into()),
            record_url: None,
            record_action: None,
            requires: None,
        };
        let directory = repro::repro_dir(&root, &meta.id);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("package.json"), "{}").unwrap();
        assert!(!has_compiled_plan(&root, &meta));
        std::fs::write(directory.join("plan.json"), "{}").unwrap();
        assert!(!has_compiled_plan(&root, &meta));
        std::fs::write(directory.join("package.json"), r#"{"plan": {}}"#).unwrap();
        assert!(has_compiled_plan(&root, &meta));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Every kept-guard format routes; a directory with no route is the
    /// caller's cue to fail the suite, never to skip the guard.
    #[test]
    fn kept_guards_route_by_their_stored_files() {
        let root =
            std::env::temp_dir().join(format!("reproit-check-kept-route-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let meta = repro::Meta {
            id: "abcdefabcdef".into(),
            alias: None,
            status: repro::Status::Required,
            seed: 0,
            created: "2026-08-05T00:00:00Z".into(),
            last_checked: None,
            last_result: None,
            trigger_index: None,
            trigger_sig: None,
            trigger_selector: None,
            trigger_fingerprint: None,
            oracle: None,
            record_url: None,
            record_action: None,
            requires: None,
        };
        let directory = repro::repro_dir(&root, &meta.id);
        std::fs::create_dir_all(&directory).unwrap();
        assert_eq!(kept_route(&root, &meta), None);
        std::fs::write(directory.join("hermetic.json"), r#"{"exec":"true"}"#).unwrap();
        assert_eq!(kept_route(&root, &meta), None);
        std::fs::write(directory.join("capture.json"), "{}").unwrap();
        assert_eq!(kept_route(&root, &meta), Some(KeptRoute::Hermetic));
        std::fs::write(directory.join("capsule.json"), "{}").unwrap();
        assert_eq!(kept_route(&root, &meta), Some(KeptRoute::Process));
        let _ = std::fs::remove_dir_all(root);
    }
}
