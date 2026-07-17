use crate::cli::context::{Ctx, Exit};
use crate::model::repro;
use crate::{config, layout};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const OPERATIONS: [&str; 4] = [
    "createSurface",
    "updateComponents",
    "updateDataModel",
    "deleteSurface",
];

pub fn looks_like_target(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let messages = parse_messages_for_detection(&text);
    !messages.is_empty()
        && messages.iter().all(Value::is_object)
        && messages.iter().any(looks_like_a2ui_message)
}

fn looks_like_a2ui_message(message: &Value) -> bool {
    if OPERATIONS.iter().any(|key| message.get(key).is_some()) {
        return true;
    }

    let version_is_a2ui = message
        .get("version")
        .and_then(Value::as_str)
        .is_some_and(|version| version.starts_with("v0."));
    version_is_a2ui && contains_a2ui_shape(message)
}

fn contains_a2ui_shape(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, child)| {
            let normalized = key
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>();
            let structural_key = normalized.contains("surface")
                || normalized.starts_with("component")
                || normalized.contains("datamodel")
                || normalized.contains("catalog")
                || [
                    "createsurface",
                    "updatecomponents",
                    "updatedatamodel",
                    "deletesurface",
                ]
                .iter()
                .any(|operation| normalized.starts_with(operation));
            structural_key || contains_a2ui_shape(child)
        }),
        Value::Array(values) => values.iter().any(contains_a2ui_shape),
        Value::String(text) => text.contains("a2ui.org") || text.contains("a2ui.dev"),
        _ => false,
    }
}

fn parse_messages_for_detection(text: &str) -> Vec<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(messages) = value.as_array() {
            return messages.clone();
        }
        if let Some(messages) = value.get("messages").and_then(Value::as_array) {
            return messages.clone();
        }
    }
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap_or_default()
}

pub fn run_target(
    ctx: &Ctx,
    target: &Path,
    command: &str,
    seed: u64,
    runs: u32,
) -> Result<ExitCode> {
    let root = std::env::current_dir()?;
    let runner_dir = config::ensure_web_runner_dir(crate::VERSION, &|message| ctx.say(message))?;
    let mut args = vec![
        runner_dir.join("a2ui-runner.mjs").into_os_string(),
        command.into(),
        target.as_os_str().to_owned(),
        "--seed".into(),
        seed.to_string().into(),
    ];
    if command == "fuzz" {
        args.extend(["--runs".into(), runs.to_string().into()]);
    }
    let output = Command::new("node")
        .args(&args)
        .current_dir(&root)
        .output()
        .context("running the A2UI renderer harness")?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        bail!(
            "A2UI runner failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if output.stdout.is_empty() {
        bail!(
            "A2UI runner failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut report: Value =
        serde_json::from_slice(&output.stdout).context("A2UI runner returned invalid JSON")?;
    persist_findings(&root, target, seed, &mut report)?;
    emit_report(ctx, command, &report);
    let has_findings = report
        .get("findings")
        .and_then(Value::as_array)
        .is_some_and(|findings| !findings.is_empty());
    Ok(if has_findings {
        Exit::Regression.code()
    } else {
        ExitCode::SUCCESS
    })
}

fn persist_findings(root: &Path, target: &Path, seed: u64, report: &mut Value) -> Result<()> {
    let source_hash = report
        .get("messagesSha256")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let original_messages = report.get("messages").cloned();
    let Some(findings) = report.get_mut("findings").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let mut public_ids = Vec::new();
    for finding in findings {
        let signature = finding
            .get("signature")
            .and_then(Value::as_str)
            .context("A2UI finding has no signature")?;
        let raw_id = repro::finding_id(&source_hash, signature, seed, &[] as &[String]);
        let public_id = repro::display_finding_id(&raw_id);
        finding["id"] = Value::String(public_id.clone());
        let messages = finding
            .get("minimalMessages")
            .cloned()
            .or_else(|| original_messages.clone())
            .context("A2UI finding has no reproducible message stream")?;
        let directory = layout::finding_dir(root, &raw_id);
        std::fs::create_dir_all(&directory)?;
        let artifact = json!({
            "format": "reproit-a2ui-finding",
            "version": 1,
            "source": target.to_string_lossy(),
            "sourceSha256": source_hash,
            "messages": messages,
            "finding": finding,
        });
        std::fs::write(
            directory.join("a2ui.json"),
            serde_json::to_vec_pretty(&artifact)?,
        )?;
        std::fs::write(
            directory.join("fuzz.md"),
            reproduction_markdown(finding, seed, &raw_id, &public_id),
        )?;
        public_ids.push(Value::String(public_id));
    }
    report["findingIds"] = Value::Array(public_ids);
    Ok(())
}

fn reproduction_markdown(finding: &Value, seed: u64, raw_id: &str, public_id: &str) -> String {
    let actions = finding
        .get("reproductionActions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let steps = actions
        .iter()
        .filter_map(|action| {
            let kind = action.get("kind")?.as_str()?;
            let component = action.get("componentId")?.as_str()?;
            match kind {
                "fill" => Some(format!(
                    "fill {component} with {}",
                    action.get("value").and_then(Value::as_str).unwrap_or("")
                )),
                "activate" => Some(format!("activate {component}")),
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    let body = steps.join("\n");
    format!(
        "# A2UI finding (seed {seed})\n\n<!-- finding-id: {raw_id} -->\n\n## confirmed repro ({} \
         actions)\n\n```\n{}\n```\n\nReplay: `reproit {public_id}`\n",
        steps.len(),
        body
    )
}

fn emit_report(ctx: &Ctx, command: &str, report: &Value) {
    if ctx.json {
        ctx.emit(report);
        return;
    }
    let findings = report
        .get("findings")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if findings.is_empty() {
        ctx.say(format!(
            "A2UI {command}: clean across the official React and Lit renderers"
        ));
        return;
    }
    ctx.say(format!(
        "A2UI {command}: {} confirmed finding(s)",
        findings.len()
    ));
    for finding in findings {
        let id = finding
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("fnd_unknown");
        let kind = finding.get("kind").and_then(Value::as_str).unwrap_or("bug");
        let renderer = finding
            .get("renderer")
            .and_then(Value::as_str)
            .unwrap_or("protocol");
        let reason = finding.get("reason").and_then(Value::as_str).unwrap_or("");
        ctx.say(format!("  {id}  {kind} in {renderer}: {reason}"));
    }
    ctx.say("Replay any finding with `reproit fnd_...`");
}

pub fn try_replay(ctx: &Ctx, id: &str) -> Result<Option<ExitCode>> {
    let Some(raw_id) = repro::raw_finding_id(id) else {
        return Ok(None);
    };
    let Some((root, artifact)) = find_artifact(raw_id)? else {
        return Ok(None);
    };
    let document: Value = serde_json::from_slice(&std::fs::read(&artifact)?)?;
    let signature = document
        .get("finding")
        .and_then(|finding| finding.get("signature"))
        .and_then(Value::as_str)
        .context("A2UI finding artifact has no signature")?;
    let runner_dir = config::ensure_web_runner_dir(crate::VERSION, &|message| ctx.say(message))?;
    let output = Command::new("node")
        .arg(runner_dir.join("a2ui-runner.mjs"))
        .args(["replay"])
        .arg(&artifact)
        .args(["--expect", signature])
        .current_dir(&root)
        .output()
        .context("replaying the A2UI finding")?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        bail!(
            "A2UI replay failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if output.stdout.is_empty() {
        bail!(
            "A2UI replay failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let report: Value =
        serde_json::from_slice(&output.stdout).context("A2UI replay returned invalid JSON")?;
    let reproduced = report
        .get("reproduced")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if ctx.json {
        ctx.emit(&report);
    } else if reproduced {
        ctx.say(format!("{id}: reproduced exactly"));
    } else {
        ctx.say(format!("{id}: no longer reproduces"));
    }
    Ok(Some(if reproduced {
        Exit::Regression.code()
    } else {
        ExitCode::SUCCESS
    }))
}

fn find_artifact(raw_id: &str) -> Result<Option<(PathBuf, PathBuf)>> {
    let cwd = std::env::current_dir()?;
    for root in cwd.ancestors() {
        let artifact = layout::finding_dir(root, raw_id).join("a2ui.json");
        if artifact.is_file() {
            return Ok(Some((root.to_path_buf(), artifact)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_json_array_object_and_jsonl_streams_structurally() {
        let dir = std::env::temp_dir().join(format!("reproit-a2ui-detect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in [
            (
                "array.json",
                r#"[{"version":"v0.9","createSurface":{"surfaceId":"x"}}]"#,
            ),
            (
                "object.json",
                r#"{"messages":[{"version":"v0.9","deleteSurface":{"surfaceId":"x"}}]}"#,
            ),
            (
                "stream.jsonl",
                "{\"version\":\"v0.9\",\"createSurface\":{\"surfaceId\":\"x\"}}\n{\"version\":\"\
                 v0.9\",\"deleteSurface\":{\"surfaceId\":\"x\"}}\n",
            ),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            assert!(looks_like_target(&path), "{name}");
        }
        let ordinary = dir.join("ordinary.json");
        std::fs::write(&ordinary, r#"{"name":"not A2UI"}"#).unwrap();
        assert!(!looks_like_target(&ordinary));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn detects_malformed_but_intended_a2ui_without_claiming_ordinary_json() {
        let dir = std::env::temp_dir().join(format!(
            "reproit-a2ui-tolerant-detect-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in [
            (
                "misspelled-operation.json",
                r#"[{"version":"v0.9","create_surface":{"surfaceId":"x"}}]"#,
            ),
            (
                "misspelled-payload.json",
                r#"[{"version":"v0.9","updateComponents":{"surfaceId":"x","componentz":[]}}]"#,
            ),
            (
                "message-envelope.json",
                r#"{"messages":[{"version":"v0.9","surfaceId":"x","components":[]}]}"#,
            ),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            assert!(looks_like_target(&path), "{name}");
        }

        for (name, text) in [
            (
                "business-version.json",
                r#"[{"version":"1.0","components":[{"name":"billing"}]}]"#,
            ),
            (
                "unrelated-draft.json",
                r#"[{"version":"v0.9","records":[{"name":"ordinary"}]}]"#,
            ),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            assert!(!looks_like_target(&path), "{name}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn writes_behavioral_actions_into_the_saved_reproduction() {
        let finding = json!({
            "reproductionActions": [
                {"kind": "fill", "componentId": "email", "value": "reproit+fixed@example.test"},
                {"kind": "activate", "componentId": "submit"}
            ]
        });
        let markdown = reproduction_markdown(&finding, 7, "raw", "fnd_public");
        assert!(markdown.contains("confirmed repro (2 actions)"));
        assert!(markdown.contains("fill email with reproit+fixed@example.test"));
        assert!(markdown.contains("activate submit"));
        assert!(markdown.contains("reproit fnd_public"));
    }
}
