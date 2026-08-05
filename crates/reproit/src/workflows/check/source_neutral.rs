//! The config-less suite: a checkout whose committed guard store IS the
//! project (no reproit.yaml anywhere up the tree). Every guard replays
//! through its stored route with no app, no device, no config.

use super::{
    execute_kept_guard, execute_plan_guard, exit_with, has_compiled_plan, kept_route,
    not_applicable_execution, CheckArgs, Ctx, Exit,
};
use crate::domain::repro;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(super) fn source_neutral_suite() -> Result<Option<(PathBuf, Vec<repro::Meta>)>> {
    let Ok(mut root) = std::env::current_dir() else {
        return Ok(None);
    };
    loop {
        let has_config =
            root.join("reproit.yaml").is_file() || root.join(".reproit/reproit.yaml").is_file();
        if !has_config {
            // Strict enumeration: a malformed store is an error here, not a
            // reason to keep walking, or a broken guard would silently drop
            // out of the suite it is supposed to gate.
            let metas = repro::load_corpus(&root)?;
            if !metas.is_empty() {
                return Ok(Some((root, metas)));
            }
        }
        if !root.pop() {
            return Ok(None);
        }
    }
}

pub(super) async fn run_source_neutral_suite(
    ctx: &Ctx,
    root: &Path,
    args: &CheckArgs,
    metas: &[repro::Meta],
) -> Result<ExitCode> {
    let times = args.runs.unwrap_or(1).max(1);
    let mut executed = 0usize;
    let mut results = Vec::new();
    let mut worst = repro::Outcome::Pass;
    for meta in metas {
        let execution = if let Some(execution) = not_applicable_execution(ctx, meta) {
            execution
        } else if has_compiled_plan(root, meta) {
            execute_plan_guard(ctx, root, args, times, meta, None).await?
        } else if let Some(route) = kept_route(root, meta) {
            execute_kept_guard(ctx, root, args, meta, None, route).await?
        } else {
            anyhow::bail!(
                "guard {} has no recognized replay route (compiled plan, process capsule, or \
                 hermetic capture); a guard that cannot replay fails the suite, it never skips",
                repro::display_repro_id(&meta.id)
            );
        };
        worst = worst.max(execution.effective);
        executed += usize::from(execution.executed);
        results.push(execution.json);
    }
    let not_applicable = metas.len() - executed;
    ctx.emit(&serde_json::json!({
        "command": "check",
        "repros": results,
        "not_applicable": not_applicable,
        "outcome": worst.as_str(),
        "exit": worst.exit_code(),
    }));
    ctx.say(format!(
        "\ncheck: {} ({} repro(s){})",
        worst.as_str().to_uppercase(),
        executed,
        if not_applicable > 0 {
            format!(", {not_applicable} not applicable here")
        } else {
            String::new()
        }
    ));
    Ok(exit_with(Exit::from(worst)))
}
