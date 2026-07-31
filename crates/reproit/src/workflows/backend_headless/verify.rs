use super::*;
use crate::interface::junit;

/// Batch proof-of-fix. Every confirmed finding persists a self-contained,
/// replayable artifact in the project's findings store (`layout::finding_dir`);
/// together they are a durable regression suite that grows with every bug ever
/// found. `verify`
/// replays each one against the live target and asserts none still reproduces,
/// so an agent that claims "fixed" is checked against the exact recorded repro
/// (batch, or one id at a time). A held finding is machine-checkable proof the
/// defect is gone; a reproducing one exits non-zero, just like the CI gate.
pub async fn run(
    ctx: &Ctx,
    config_path: Option<&Path>,
    ids: &[String],
    junit_path: Option<&Path>,
    prune_retracted: bool,
) -> Result<ExitCode> {
    let Some(root) = project_root_with_findings()? else {
        ctx.say(format!(
            "no findings to verify (nothing under {})",
            layout::findings_dir_rel()
        ));
        return Ok(ExitCode::SUCCESS);
    };
    // Authenticate exactly like the scan: an auth-gated finding replayed
    // unauthenticated returns 401 and its contract cannot be evaluated, which
    // would otherwise be misread as "held". Fails closed on a bad login.
    install_identity_pool_for_verify(ctx, config_path).await?;
    // What the project asserts right now, read once for the whole batch: a
    // finding proven against a claim the schema has since withdrawn is retracted
    // rather than replayed forever.
    let current = CurrentContracts::load(config_path);
    let wanted: BTreeSet<String> = ids.iter().map(|id| id.trim().to_string()).collect();
    let mut artifacts = collect_artifacts(&root)?;
    artifacts.sort();

    let mut held = Vec::new();
    let mut reproducing = Vec::new();
    let mut inconclusive = Vec::new();
    let mut retracted = Vec::new();
    let mut cases = Vec::new();
    for artifact_path in &artifacts {
        // Select before replaying. Naming ids must narrow the work, not just the
        // report: every replay re-sends setup and the failing request at the live
        // target, so filtering afterwards would exercise the whole suite to
        // answer a question about one finding.
        if !wanted.is_empty()
            && !wanted.contains(&replay_command::artifact_finding_id(artifact_path)?)
        {
            continue;
        }
        let outcome = replay_command::replay_artifact(artifact_path, &current).await?;
        let id = outcome
            .finding
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("fnd_unknown")
            .to_string();
        let operation = outcome
            .finding
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("operation")
            .to_string();
        let entry = json!({ "id": id, "operation": operation });
        // One source for pass/fail: the verdict itself. The arms only choose
        // which bucket the entry lands in and how it reads.
        let passed = !outcome.verdict.blocks();
        let message = match &outcome.verdict {
            ArtifactVerdict::Fixed => {
                held.push(entry);
                format!("{id} held (no longer reproduces) on {operation}")
            }
            ArtifactVerdict::Reproduced => {
                reproducing.push(entry);
                format!("{id} still reproduces on {operation}")
            }
            ArtifactVerdict::Inconclusive => {
                inconclusive.push(entry);
                format!("{id} inconclusive on {operation} (could not evaluate)")
            }
            // Passing, but never counted as held: nothing was proven about the
            // implementation, the claim was withdrawn.
            ArtifactVerdict::Retracted(reason) => {
                let mut entry = entry;
                entry["reason"] = Value::String(reason.clone());
                entry["path"] = Value::String(artifact_path.to_string_lossy().into_owned());
                retracted.push(entry);
                format!("{id} retracted on {operation}: {reason}")
            }
        };
        cases.push(junit::Case {
            name: format!("{operation} [{id}]"),
            passed,
            time_s: 0.0,
            message,
        });
    }
    let pruned = if prune_retracted {
        prune(ctx, &retracted)?
    } else {
        0
    };

    if let Some(path) = junit_path {
        if let Err(error) = junit::write(path, "reproit-verify", &cases) {
            eprintln!(
                "warn: could not write verify junit {}: {error}",
                path.display()
            );
        }
    }

    let report = json!({
        "command": "backend verify",
        "held": held,
        "reproducing": reproducing,
        "inconclusive": inconclusive,
        "retracted": retracted,
        "pruned": pruned,
        "counts": {
            "held": held.len(),
            "reproducing": reproducing.len(),
            "inconclusive": inconclusive.len(),
            "retracted": retracted.len(),
        },
    });
    if ctx.json {
        ctx.emit(&report);
    } else if held.is_empty()
        && reproducing.is_empty()
        && inconclusive.is_empty()
        && retracted.is_empty()
    {
        ctx.say("verify: no matching findings".to_string());
    } else {
        ctx.say(format!(
            "verify: {} held, {} still reproducing, {} inconclusive, {} retracted",
            held.len(),
            reproducing.len(),
            inconclusive.len(),
            retracted.len()
        ));
        for finding in &reproducing {
            ctx.say(format!(
                "  {} still reproduces on {}",
                finding["id"].as_str().unwrap_or(""),
                finding["operation"].as_str().unwrap_or("")
            ));
        }
        for finding in &inconclusive {
            ctx.say(format!(
                "  {} inconclusive (could not evaluate) on {}",
                finding["id"].as_str().unwrap_or(""),
                finding["operation"].as_str().unwrap_or("")
            ));
        }
        for finding in &retracted {
            ctx.say(format!(
                "  {} retracted on {}: {}",
                finding["id"].as_str().unwrap_or(""),
                finding["operation"].as_str().unwrap_or(""),
                finding["reason"].as_str().unwrap_or("")
            ));
        }
        if !retracted.is_empty() {
            ctx.say(if prune_retracted {
                format!("  removed {pruned} retracted finding(s)")
            } else {
                "  a retracted finding proves nothing about the implementation; \
                 remove them with `reproit verify --prune-retracted`"
                    .to_string()
            });
        }
    }
    // Only an all-held run is a pass: a still-reproducing finding is a live bug,
    // and an inconclusive one means verify could not certify the fix, so it fails
    // closed rather than issuing a false all-clear. Retracted findings do not
    // block, because withdrawing a claim is an explicit schema edit that already
    // makes `scan` stop reporting them.
    Ok(if reproducing.is_empty() && inconclusive.is_empty() {
        ExitCode::SUCCESS
    } else {
        Exit::Regression.code()
    })
}

/// Delete the finding directories of retracted findings. The artifact is only
/// meaningful as a constraint, and its constraint no longer exists; leaving it
/// on disk means every later run re-reports it forever.
fn prune(ctx: &Ctx, retracted: &[Value]) -> Result<usize> {
    let mut removed = 0;
    for finding in retracted {
        let Some(path) = finding["path"].as_str().map(Path::new) else {
            continue;
        };
        let Some(directory) = path.parent() else {
            continue;
        };
        match std::fs::remove_dir_all(directory) {
            Ok(()) => removed += 1,
            Err(error) => ctx.say(format!(
                "warn: could not remove {}: {error}",
                directory.display()
            )),
        }
    }
    Ok(removed)
}

/// Build and install the identity pool from the project's backend auth config so
/// the replay is authenticated. No config or no auth is fine (a public target);
/// a configured-but-failing login propagates as an error (fail closed).
async fn install_identity_pool_for_verify(ctx: &Ctx, config_path: Option<&Path>) -> Result<()> {
    let Some((_, config)) = crate::workflows::backend_target::resolve(config_path)? else {
        return Ok(());
    };
    let Some(auth) = config.auth.as_ref() else {
        return Ok(());
    };
    let base_url = std::env::var("REPROIT_BACKEND_URL")
        .ok()
        .or_else(|| config.target.clone());
    let Some(base_url) = base_url else {
        return Ok(());
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let pool = build_identity_pool(&client, &base_url, auth).await?;
    install_identity_pool(pool);
    let count = identity_count();
    ctx.say(format!(
        "verify authenticated {count} identit{}",
        if count == 1 { "y" } else { "ies" }
    ));
    Ok(())
}

/// The nearest ancestor of the cwd that has a findings store.
fn project_root_with_findings() -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir()?;
    for root in cwd.ancestors() {
        if layout::findings_dir(root).is_dir() {
            return Ok(Some(root.to_path_buf()));
        }
    }
    Ok(None)
}

/// Every backend finding artifact under the project's findings store.
fn collect_artifacts(root: &Path) -> Result<Vec<PathBuf>> {
    let mut artifacts = Vec::new();
    let findings = layout::findings_dir(root);
    let Ok(entries) = std::fs::read_dir(&findings) else {
        return Ok(artifacts);
    };
    for entry in entries.flatten() {
        let directory = entry.path();
        if !directory.is_dir() {
            continue;
        }
        for name in ["backend.json", "backend-schema.json"] {
            let artifact = directory.join(name);
            if artifact.is_file() {
                artifacts.push(artifact);
                break;
            }
        }
    }
    Ok(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_artifacts_finds_backend_findings_and_skips_the_rest() {
        let dir = std::env::temp_dir().join(format!("reproit-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let findings = layout::findings_dir(&dir);
        for (name, file) in [
            ("fnd_a", Some("backend.json")),
            ("fnd_b", Some("backend-schema.json")),
            ("fnd_c", None), // an unrelated finding with no backend artifact
        ] {
            let entry = findings.join(name);
            std::fs::create_dir_all(&entry).unwrap();
            if let Some(file) = file {
                std::fs::write(entry.join(file), b"{}").unwrap();
            }
        }
        let mut names: Vec<String> = collect_artifacts(&dir)
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["backend-schema.json", "backend.json"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
