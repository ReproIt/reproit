//! `reproit keep`: persist a finding as a committed regression guard AND wire
//! the CI check in the same command. "Now there is a reproit regression test
//! in the PR" must be one command, not a manual git dance, so keep writes both
//! the guard store (`.reproit/repros/<id>/`) and the GitHub Actions workflow
//! that runs `reproit check` with the four-way exit-code mapping.

use super::{find_finding_by_id, latest_finding, parse_fuzz_oracle};
use crate::adapters::config;
use crate::domain::repro;
use crate::interface::cli::context::Ctx;
use crate::runtime::project_layout as layout;
use crate::workflows::record::web_record_metadata;
use anyhow::Result;
use std::path::Path;

/// `keep`: take a finding from the latest fuzz artifact, compute its content
/// hash id, and write the committed store dir + meta.json. The store dir name
/// IS the content hash (stable across machines, self-deduping). Default status
/// is quarantined; `--strict` lands it required. `--as` sets the alias.
pub(in crate::workflows) fn keep_repro(
    ctx: &Ctx,
    loaded: &config::Loaded,
    id: Option<&str>,
    as_name: Option<&str>,
    strict: bool,
) -> Result<()> {
    let root = loaded.root.as_path();
    // Resolve the finding to keep: a specific one by id (any finding the last
    // fuzz reported, so `keep <id>` pairs with `reproit <id>`), or the latest when
    // no id is given.
    let finding = match id {
        Some(want) => find_finding_by_id(loaded, want).ok_or_else(|| {
            anyhow::anyhow!(
                "no fuzz finding with id `{want}` under {}. List ids from the last `reproit \
                 fuzz`, or omit the id to keep the latest finding.",
                loaded.config.evidence.out_dir
            )
        })?,
        None => latest_finding(loaded).ok_or_else(|| {
            anyhow::anyhow!(
                "no fuzz finding under {}. Run `reproit fuzz` first.",
                loaded.config.evidence.out_dir
            )
        })?,
    };
    let computed = finding.id();
    let dir = repro::repro_dir(root, &computed);
    // Repros are content-addressed, so the same case keeps to the same id:
    // re-keeping is a no-op-ish "already saved" that must PRESERVE the existing
    // guard's history (status promotion, check results, created stamp, alias)
    // rather than clobber it back to a fresh quarantine.
    let existing = repro::load_meta(root, &computed);
    std::fs::create_dir_all(&dir)?;
    // Store the replay config so `check` can reproduce the case deterministically.
    let replay = serde_json::json!({ "seed": finding.seed, "replay": finding.actions });
    std::fs::write(
        dir.join("replay.json"),
        serde_json::to_string_pretty(&replay)?,
    )?;
    // Carry the discovering report for human reference (best-effort).
    let _ = std::fs::copy(finding.run_dir.join("fuzz.md"), dir.join("fuzz.md"));
    let finding_evidence = layout::finding_dir(root, &computed).join("run-evidence.json");
    if finding_evidence.exists() {
        std::fs::copy(finding_evidence, dir.join("run-evidence.json"))?;
    }
    let finding_capsule = layout::finding_dir(root, &computed).join("capsule-id");
    if let Ok(id) = std::fs::read_to_string(finding_capsule) {
        std::fs::write(dir.join("capsule-id"), id)?;
    }
    let finding_contract = layout::finding_dir(root, &computed).join("contract.json");
    if finding_contract.exists() {
        std::fs::copy(finding_contract, dir.join("contract.json"))?;
    }
    let finding_backend_contract =
        layout::finding_dir(root, &computed).join("backend-contract.json");
    if finding_backend_contract.exists() {
        std::fs::copy(finding_backend_contract, dir.join("backend-contract.json"))?;
    }
    // A backend finding's replay artifact IS the guard: copy it into the
    // committed store so `reproit check` replays it from a fresh checkout,
    // where local-only `.reproit/` state does not exist.
    for name in ["backend.json", "backend-schema.json"] {
        let source = finding.run_dir.join(name);
        let source = if source.is_file() {
            source
        } else {
            layout::finding_dir(root, &computed).join(name)
        };
        if source.is_file() {
            std::fs::copy(&source, dir.join(name))?;
        }
    }

    // Status: a fresh keep lands quarantined (or required with --strict); a
    // RE-keep preserves the existing status, so re-running keep never demotes a
    // guard that already went green (--strict can still upgrade it to required).
    let status = if strict {
        repro::Status::Required
    } else {
        existing
            .as_ref()
            .map(|m| m.status)
            .unwrap_or(repro::Status::Quarantined)
    };
    // Alias: an explicit `--as` sets (or renames) the alias; without it, an
    // existing alias is kept rather than wiped.
    let alias = as_name
        .map(String::from)
        .or_else(|| existing.as_ref().and_then(|m| m.alias.clone()));
    // Record the finding's TRIGGER POINT so `check` can tell "the fix changed
    // downstream navigation" (a miss AFTER the trigger -> still PASS) from "the
    // path to the bug is gone" (a miss BEFORE the trigger -> STALE). The saved
    // `actions` are the minimized sequence that LEADS TO the finding, so the
    // finding fired after performing all of them: the trigger index is that
    // count. (The fuzz report does not currently carry the trigger state sig, so
    // `trigger_sig` stays None and the index does the work.)
    let trigger_index = Some(repro::normalize_actions(&finding.actions).len());
    // Record the finding's ORACLE category and violating state sig. `keep` reads
    // these from the `## oracle` block fuzz.md emits.
    let md = std::fs::read_to_string(finding.run_dir.join("fuzz.md")).unwrap_or_default();
    let (oracle, finding_sig, trigger_selector, trigger_fingerprint) = parse_fuzz_oracle(&md);
    // Crash findings use the exception path; state findings retain the signature
    // for direct recording and existing sig-reached logic.
    let trigger_sig = finding_sig.filter(|s| !s.is_empty());
    let log = std::fs::read_to_string(finding.run_dir.join("drive-a.log")).unwrap_or_default();
    let (record_url, record_action) = web_record_metadata(
        loaded.config.app.url.as_deref(),
        oracle.as_deref(),
        trigger_sig.as_deref(),
        &log,
    );
    let meta = repro::Meta {
        id: computed.clone(),
        alias: alias.clone(),
        status,
        seed: finding.seed,
        // Preserve the original creation stamp on a re-keep; stamp now on a fresh
        // save.
        created: existing
            .as_ref()
            .map(|m| m.created.clone())
            .unwrap_or_else(|| chrono::Local::now().to_rfc3339()),
        last_checked: existing.as_ref().and_then(|m| m.last_checked.clone()),
        last_result: existing.as_ref().and_then(|m| m.last_result.clone()),
        trigger_index,
        trigger_sig,
        trigger_selector,
        trigger_fingerprint,
        oracle,
        record_url,
        record_action,
    };
    repro::save_meta(root, &meta)?;
    let (ci_workflow, ci_wiring) = wire_ci(root)?;

    // Was this already in the suite? If so, report it as "already saved" (and
    // note an alias rename) instead of pretending it's a fresh keep.
    let prior_alias = existing.as_ref().and_then(|m| m.alias.clone());
    let renamed = match (&prior_alias, as_name) {
        (Some(old), Some(new)) if old != new => Some((old.clone(), new.to_string())),
        _ => None,
    };
    let public_id = repro::display_repro_id(&computed);
    let source_id = repro::display_finding_id(&computed);
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "keep",
            "id": public_id,
            "kind": "repro",
            "source_id": source_id,
            "alias": meta.alias,
            "status": status.as_str(),
            "already_saved": existing.is_some(),
            "renamed_from": renamed.as_ref().map(|(old, _)| old.clone()),
            "seed": finding.seed,
            "actions": finding.actions,
            "dir": dir.to_string_lossy(),
            "ci_workflow": ci_workflow,
            "ci_wiring": ci_wiring.as_str(),
        }));
        return Ok(());
    }
    if existing.is_some() {
        match &renamed {
            Some((old, new)) => ctx.say(format!(
                "  already saved ({}); alias {old} -> {new}",
                public_id
            )),
            None => {
                let label = alias.as_deref().unwrap_or(&public_id);
                ctx.say(format!("  already saved as {label} ({})", status.as_str()));
            }
        }
        ctx.say(format!("  reproduce: reproit {public_id}"));
    } else {
        ctx.say(format!("  kept {} ({})", public_id, status.as_str()));
        if let Some(a) = &alias {
            ctx.say(format!("  alias: {a}"));
        }
        ctx.say(format!("  verify: reproit {public_id}"));
    }
    match ci_wiring {
        CiWiring::Written => ctx.say(format!(
            "  write {ci_workflow} (CI runs `reproit check`: 0 pass, 1 regression, 2 flaky, \
             3 stale)"
        )),
        CiWiring::Appended => ctx.say(format!(
            "  appended the reproit-check job to {ci_workflow}"
        )),
        CiWiring::AlreadyWired => ctx.say(format!("  CI already runs `reproit check` \
             ({ci_workflow})")),
    }
    Ok(())
}

const CI_WORKFLOW_REL: &str = ".github/workflows/reproit.yml";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CiWiring {
    Written,
    Appended,
    AlreadyWired,
}

impl CiWiring {
    fn as_str(self) -> &'static str {
        match self {
            CiWiring::Written => "written",
            CiWiring::Appended => "appended",
            CiWiring::AlreadyWired => "already-wired",
        }
    }
}

/// Ensure the project's GitHub Actions workflow runs `reproit check` on every
/// push and pull request. A missing workflow is written whole; an existing
/// `reproit.yml` without the check gets the job appended; a workflow that
/// already runs the check is left untouched. Idempotent across re-keeps.
fn wire_ci(root: &Path) -> Result<(String, CiWiring)> {
    let path = root.join(CI_WORKFLOW_REL);
    if path.is_file() {
        let body = std::fs::read_to_string(&path)?;
        if body.contains("reproit check") {
            return Ok((CI_WORKFLOW_REL.to_string(), CiWiring::AlreadyWired));
        }
        if body.contains("jobs:") {
            let mut appended = body;
            if !appended.ends_with('\n') {
                appended.push('\n');
            }
            appended.push_str(&check_job(root));
            std::fs::write(&path, appended)?;
            return Ok((CI_WORKFLOW_REL.to_string(), CiWiring::Appended));
        }
        // No jobs mapping to extend: leave the file alone rather than corrupt
        // it; the fresh-workflow path stays available under another name only
        // by hand, so report it as already handled by the user.
        return Ok((CI_WORKFLOW_REL.to_string(), CiWiring::AlreadyWired));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let workflow = format!(
        "# Generated by `reproit keep`. Replays the kept regression guards and runs\n\
         # the reproit gate on every push and pull request.\n\
         name: reproit\non:\n  push:\n  pull_request:\njobs:\n{}",
        check_job(root)
    );
    std::fs::write(&path, workflow)?;
    Ok((CI_WORKFLOW_REL.to_string(), CiWiring::Written))
}

/// The `reproit check` job body (everything under `jobs:`), shared by the
/// fresh-workflow and append paths so the two can never drift.
fn check_job(root: &Path) -> String {
    let install_dependencies = if root.join("package.json").is_file() {
        "      - name: Install dependencies\n        run: npm install --no-audit --no-fund\n"
    } else {
        ""
    };
    format!(
        "  # `reproit check` exit codes: 0 pass, 1 regression, 2 flaky, 3 stale.\n\
         \x20 reproit-check:\n\
         \x20   runs-on: ubuntu-latest\n\
         \x20   steps:\n\
         \x20     - uses: actions/checkout@v4\n\
         {install_dependencies}\
         \x20     - name: Install reproit\n\
         \x20       run: |\n\
         \x20         export REPROIT_BIN_DIR=\"$HOME/.local/bin\"\n\
         \x20         curl -fsSL \
         https://raw.githubusercontent.com/ReproIt/reproit/main/install.sh | sh\n\
         \x20         echo \"$REPROIT_BIN_DIR\" >> \"$GITHUB_PATH\"\n\
         \x20     - name: reproit check\n\
         \x20       run: reproit check\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("reproit-keep-ci-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_workflow_is_written_whole_and_rewiring_is_idempotent() {
        let root = temp_root("fresh");
        let (path, wiring) = wire_ci(&root).unwrap();
        assert_eq!(wiring, CiWiring::Written);
        let body = std::fs::read_to_string(root.join(&path)).unwrap();
        assert!(body.contains("run: reproit check"), "{body}");
        assert!(body.contains("0 pass, 1 regression, 2 flaky, 3 stale"), "{body}");
        assert!(body.contains("pull_request:"), "{body}");
        // A repo without a package.json gets no npm step.
        assert!(!body.contains("npm install"), "{body}");
        let (_, again) = wire_ci(&root).unwrap();
        assert_eq!(again, CiWiring::AlreadyWired);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_workflow_without_the_check_gets_the_job_appended() {
        let root = temp_root("append");
        std::fs::write(root.join("package.json"), "{}").unwrap();
        let dir = root.join(".github/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("reproit.yml"),
            "name: reproit\non:\n  push:\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             \x20   steps:\n      - uses: actions/checkout@v4\n",
        )
        .unwrap();
        let (path, wiring) = wire_ci(&root).unwrap();
        assert_eq!(wiring, CiWiring::Appended);
        let body = std::fs::read_to_string(root.join(&path)).unwrap();
        // The existing job survives, the check job lands after it, and the
        // node project gets its dependency install step.
        assert!(body.contains("  build:"), "{body}");
        assert!(body.contains("  reproit-check:"), "{body}");
        assert!(body.contains("npm install"), "{body}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
