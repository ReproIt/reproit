use super::*;
use crate::interface::junit;

/// Batch proof-of-fix. Every confirmed finding persists a self-contained,
/// replayable artifact under `.reproit/findings/<id>/`; together they are a
/// durable regression suite that grows with every bug ever found. `verify`
/// replays each one against the live target and asserts none still reproduces,
/// so an agent that claims "fixed" is checked against the exact recorded repro
/// (batch, or one id at a time). A held finding is machine-checkable proof the
/// defect is gone; a reproducing one exits non-zero, just like the CI gate.
pub async fn run(ctx: &Ctx, ids: &[String], junit_path: Option<&Path>) -> Result<ExitCode> {
    let Some(root) = project_root_with_findings()? else {
        ctx.say("no findings to verify (nothing under .reproit/findings)".to_string());
        return Ok(ExitCode::SUCCESS);
    };
    let wanted: BTreeSet<String> = ids.iter().map(|id| id.trim().to_string()).collect();
    let mut artifacts = collect_artifacts(&root)?;
    artifacts.sort();

    let mut held = Vec::new();
    let mut reproducing = Vec::new();
    let mut cases = Vec::new();
    for artifact_path in &artifacts {
        let outcome = replay_command::replay_artifact(artifact_path).await?;
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
        if !wanted.is_empty() && !wanted.contains(&id) {
            continue;
        }
        cases.push(junit::Case {
            name: format!("{operation} [{id}]"),
            passed: !outcome.reproduced,
            time_s: 0.0,
            message: if outcome.reproduced {
                format!("{id} still reproduces on {operation}")
            } else {
                format!("{id} held (no longer reproduces) on {operation}")
            },
        });
        if outcome.reproduced {
            reproducing.push(json!({ "id": id, "operation": operation }));
        } else {
            held.push(json!({ "id": id, "operation": operation }));
        }
    }

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
        "counts": { "held": held.len(), "reproducing": reproducing.len() },
    });
    if ctx.json {
        ctx.emit(&report);
    } else if held.is_empty() && reproducing.is_empty() {
        ctx.say("verify: no matching findings".to_string());
    } else {
        ctx.say(format!(
            "verify: {} held, {} still reproducing",
            held.len(),
            reproducing.len()
        ));
        for finding in &reproducing {
            ctx.say(format!(
                "  {} still reproduces on {}",
                finding["id"].as_str().unwrap_or(""),
                finding["operation"].as_str().unwrap_or("")
            ));
        }
    }
    Ok(if reproducing.is_empty() {
        ExitCode::SUCCESS
    } else {
        Exit::Regression.code()
    })
}

/// The nearest ancestor of the cwd that has a `.reproit/findings` directory.
fn project_root_with_findings() -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir()?;
    for root in cwd.ancestors() {
        if layout::findings_dir(root).is_dir() {
            return Ok(Some(root.to_path_buf()));
        }
    }
    Ok(None)
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
