//! Keeping a process capsule as a regression test.
//!
//! Without this route the capsule could REPRODUCE a failure and never be
//! RETAINED, which breaks the product's find-then-keep-then-check loop for
//! exactly the class of program the capsule exists to serve: the ones that
//! are not request shaped. A backend capture lands in `.reproit/repros/<id>/`
//! as `capture.json` plus a `hermetic.json` boot recipe; a process capsule
//! lands beside it as `capsule.json` plus the same recipe file.
//!
//! The two names are deliberately distinct. `reproit check <id>` sniffs the
//! guard directory, and one file name per format keeps that routing a lookup
//! rather than a guess.

use crate::interface::cli::context::Ctx;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::process::ExitCode;

use crate::workflows::backend_headless::HermeticVerdict;

/// The capsule inside a kept guard. Distinct from the backend guard's
/// `capture.json` so a directory names its own format.
const GUARD_CAPSULE: &str = "capsule.json";
const GUARD_RECIPE: &str = "hermetic.json";

/// `reproit keep <capsule.json> --exec "<command>"`: land a process capsule
/// in `.reproit/repros/<id>/` so `reproit check` re-executes it on every run.
///
/// The guard is proven live BEFORE it is kept, exactly as the backend guard
/// is: a capsule whose current verdict is diverged or inconclusive would be
/// dead on arrival, so it is refused with the verdict named rather than
/// stored as a test that can never mean anything.
pub async fn keep_capsule_guard(
    ctx: &Ctx,
    file: &Path,
    exec: &str,
    alias: Option<&str>,
    strict: bool,
) -> Result<ExitCode> {
    let capsule = super::parse(file)?;
    let replayed = super::replay(&capsule, exec).await?;
    match replayed.verdict {
        HermeticVerdict::Reproduced | HermeticVerdict::Fixed => {}
        verdict => bail!(
            "refusing to keep a guard whose current process verdict is {}; a guard must \
             reproduce (bug present) or hold (bug fixed) at keep time to mean anything in CI",
            verdict.as_str()
        ),
    }
    let bytes = std::fs::read(file)?;
    let id = super::hex_digest(&bytes)[..12].to_string();
    let root = std::env::current_dir()?;
    let directory = crate::domain::repro::repro_dir(&root, &id);
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join(GUARD_CAPSULE), &bytes)?;
    std::fs::write(
        directory.join(GUARD_RECIPE),
        serde_json::to_vec_pretty(&json!({ "exec": exec }))?,
    )?;
    let meta = crate::domain::repro::Meta {
        id: id.clone(),
        alias: alias.map(str::to_string),
        status: if strict {
            crate::domain::repro::Status::Required
        } else {
            crate::domain::repro::Status::Quarantined
        },
        seed: 0,
        created: chrono::Utc::now().to_rfc3339(),
        last_checked: None,
        last_result: None,
        trigger_index: None,
        trigger_sig: None,
        trigger_selector: None,
        trigger_fingerprint: None,
        oracle: Some(capsule.oracle.clone()),
        record_url: None,
        record_action: None,
    };
    crate::domain::repro::save_meta(&root, &meta)?;
    ctx.emit(&json!({
        "command": "keep",
        "source": "process-capsule",
        "id": id,
        "alias": alias,
        "status": meta.status.as_str(),
        "directory": directory,
        "verdictAtKeep": replayed.verdict.as_str(),
        "oracle": capsule.oracle,
    }));
    ctx.say(format!("Kept process capsule guard {id}"));
    ctx.say(format!("  verdict now: {}", replayed.verdict.as_str()));
    ctx.say(format!("  status:      {}", meta.status.as_str()));
    ctx.say(format!("  guard:       {}", directory.display()));
    // The checkable reference, spelled out. `reproit check` resolves a repro
    // by its PREFIXED id or by an alias, so printing the bare directory name
    // would hand the operator a string that does not resolve.
    ctx.say(format!(
        "  reproit check {} replays it hermetically, with no capsule path and no --exec",
        alias
            .map(str::to_string)
            .unwrap_or_else(|| crate::domain::repro::display_repro_id(&id))
    ));
    Ok(ExitCode::SUCCESS)
}

/// `reproit check <id>` for a kept process capsule: replay through the stored
/// exec recipe. `None` when the reference is not a process guard, so the
/// caller falls through to its other routes.
pub async fn try_replay_process_guard(ctx: &Ctx, reference: &str) -> Result<Option<ExitCode>> {
    let root = std::env::current_dir()?;
    let Some(meta) = crate::domain::repro::resolve(&root, reference) else {
        return Ok(None);
    };
    let directory = crate::domain::repro::repro_dir(&root, &meta.id);
    let file = directory.join(GUARD_CAPSULE);
    let recipe = directory.join(GUARD_RECIPE);
    if !file.is_file() || !recipe.is_file() {
        return Ok(None);
    }
    let exec = serde_json::from_slice::<Value>(&std::fs::read(&recipe)?)
        .ok()
        .and_then(|recipe| {
            recipe
                .get("exec")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .with_context(|| format!("{} has no `exec` command", recipe.display()))?;
    let capsule = super::parse(&file)?;
    let replayed = super::replay(&capsule, &exec).await?;
    Ok(Some(super::report(ctx, &file, &capsule, &replayed)))
}
