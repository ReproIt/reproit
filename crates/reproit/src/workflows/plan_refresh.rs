//! Re-pin a plan-backed guard's mechanism against the current checkout.
//!
//! A plan guard pins the code that reaches its failure: the provider source by
//! digest, and that provider definition by digest again inside the plan. When
//! the checkout legitimately changes underneath it (a renamed CLI command
//! inside a verifier, a rewritten fixture) the guard is stranded. It still
//! asserts the right thing and can no longer reach it.
//!
//! What this does NOT do is re-capture. A regression guard's failure is already
//! fixed, so re-running its trigger must not reproduce it and there is no fresh
//! observation to record. The occurrence and its recorded artifacts are
//! evidence of what was actually observed and are left untouched: recomputing
//! them would not re-derive an observation, it would invent one.
//!
//! What it does is accept a reviewed mechanism. `templateDigest` is checked at
//! execution time as tamper-evidence for the mechanism (see `catalog.rs`), not
//! as provenance for the occurrence, so advancing it after a human looks at the
//! diff is exactly what it is for. The guard's identity, alias, status and
//! check history survive, because a guard is anchored on the failure it
//! preserves rather than on the mechanism that reaches it.

use crate::adapters::execution;
use crate::domain::repro;
use crate::interface::cli::context::{exit_with, Ctx, Exit};
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::ExitCode;

pub(super) async fn refresh_plan_guard(
    ctx: &Ctx,
    reference: &str,
    meta: &repro::Meta,
) -> Result<ExitCode> {
    let root = std::env::current_dir()?;
    let directory = repro::repro_dir(&root, &meta.id);
    if !directory.join("plan.json").is_file() {
        bail!(
            "`{reference}` is not a plan-backed guard (no plan.json in {})",
            directory.display()
        );
    }

    let mut package = load_guard_package(&directory)?;
    let previous =
        execution::pinned_provider_digest(&package).context("the guard plan carries no binding")?;

    ctx.say(format!("refresh {}", repro::display_repro_id(&meta.id)));
    let drift = source_status(&root, &directory)?;
    for line in &drift {
        ctx.say(format!("  {line}"));
    }
    if drift.iter().all(|line| line.ends_with("unchanged")) {
        ctx.say("  the recorded mechanism already matches this checkout; nothing to re-pin");
        return Ok(ExitCode::SUCCESS);
    }
    if !ctx.confirmed() {
        ctx.say("  re-run with --yes to accept this mechanism for the guard");
        return Ok(exit_with(Exit::Stale));
    }

    let refreshed =
        execution::repin_guard_providers(&root, &directory, &package.occurrence.occurrence_id)?;
    execution::repin_package_mechanism(&mut package, &refreshed)?;
    let plan = package
        .plan
        .clone()
        .context("the guard package carries no plan")?;
    write_json(&directory.join("package.json"), &package)?;
    write_json(&directory.join("plan.json"), &plan)?;

    ctx.say(format!(
        "  re-pinned {}; identity, alias and history preserved",
        repro::display_repro_id(&meta.id)
    ));
    ctx.emit(&serde_json::json!({
        "command": "keep",
        "mode": "refresh",
        "kind": "plan-guard",
        "id": repro::display_repro_id(&meta.id),
        "occurrence": package.occurrence.occurrence_id,
        "mechanism": {"from": previous, "to": refreshed},
    }));
    Ok(ExitCode::SUCCESS)
}

fn load_guard_package(directory: &Path) -> Result<reproit_protocol::ReproductionPackage> {
    let path = directory.join("package.json");
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading the guard package {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing the guard package {}", path.display()))
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
}

/// Which pinned provider sources actually moved. That is the reason a refresh
/// is being run, so it is reported rather than left for the reader to infer.
fn source_status(root: &Path, directory: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(directory.join("providers.yaml"))?;
    let mut lines = Vec::new();
    let mut source: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("path: ") {
            source = Some(value.trim().to_string());
            continue;
        }
        let (Some(pinned), Some(path)) = (trimmed.strip_prefix("sha256: "), source.as_deref())
        else {
            continue;
        };
        let file = root.join(path);
        lines.push(if !file.is_file() {
            format!("{path}: MISSING from this checkout")
        } else if execution::source_digest(&file)? == pinned.trim() {
            format!("{path}: unchanged")
        } else {
            format!("{path}: CHANGED since the guard was recorded")
        });
        source = None;
    }
    Ok(lines)
}
