//! Read-only projections for the outcome-oriented `list` command.

use crate::adapters::config;
use crate::domain::repro;
use crate::interface::cli::context::Ctx;
use anyhow::Result;
use std::path::Path;
use std::process::ExitCode;

/// Load the project for the read-only list projections. A backend-only
/// reproit.yaml (what `reproit init` writes for a service) has no `app`
/// section, so the app loader rejects it with "missing field 'app'" although
/// repro and finding state live under the same root and default evidence
/// layout. Stand in a minimal read-only view rooted at the backend project
/// instead of erroring on a config init itself wrote.
pub(super) fn load_read_view(config_path: Option<&Path>) -> Result<config::Loaded> {
    let app_error = match config::load(config_path) {
        Ok(loaded) => return Ok(loaded),
        Err(error) => error,
    };
    let Some(project) = super::backend_target::find(config_path)? else {
        return Err(app_error);
    };
    // The platform is a stand-in: list never launches a runner, it only needs
    // the root and the default evidence layout the parse supplies.
    let yaml = "app:\n  platform: web\n  defines: {}\ndevices:\n  namePrefix: backend\n\
                journeys:\n  driver: \"\"\n  doneMarkers: [\"All tests passed\"]\n";
    config::parse_str(yaml, project.root)
}

pub(super) fn guards(ctx: &Ctx, loaded: &config::Loaded, command: &str) -> Result<ExitCode> {
    let metas = repro::list(&loaded.root);
    if ctx.json {
        let items: Vec<serde_json::Value> = metas
            .iter()
            .map(|meta| {
                let actions =
                    super::repro::load_repro_actions(loaded, &meta.id).unwrap_or_default();
                serde_json::json!({
                    "id": repro::display_repro_id(&meta.id),
                    "kind": "repro",
                    "alias": meta.alias,
                    "status": meta.status.as_str(),
                    "seed": meta.seed,
                    "created": meta.created,
                    "last_checked": meta.last_checked,
                    "last_result": meta.last_result,
                    "actions": actions,
                })
            })
            .collect();
        ctx.emit(&serde_json::json!({ "command": command, "repros": items }));
        return Ok(ExitCode::SUCCESS);
    }
    if metas.is_empty() {
        ctx.say("no saved guards. Find failures with `reproit find`, then run `reproit keep`.");
        return Ok(ExitCode::SUCCESS);
    }
    ctx.say(format!(
        "  {:<14} {:<18} {:<12} {}",
        "ID", "ALIAS", "STATUS", "LAST CHECK"
    ));
    for meta in &metas {
        ctx.say(format!(
            "  {:<14} {:<18} {:<12} {}",
            repro::display_repro_id(&meta.id),
            meta.alias.as_deref().unwrap_or("-"),
            meta.status.as_str(),
            meta.last_result.as_deref().unwrap_or("never"),
        ));
    }
    Ok(ExitCode::SUCCESS)
}
