//! Read-only projections for the outcome-oriented `list` command.

use crate::adapters::config;
use crate::domain::repro;
use crate::interface::cli::context::Ctx;
use anyhow::Result;
use std::process::ExitCode;

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
