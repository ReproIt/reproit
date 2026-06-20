//! `reproit import maestro <flow.yaml>`: translate a Maestro flow into a reproit
//! journey, so switching off Maestro costs ~0. The common commands map onto the
//! reproit action/assert grammar (tap:/type:/back/shoot:/assert:); anything
//! without a clean equivalent is emitted as a `# TODO(maestro): ...` line so a
//! command is never silently dropped, and a summary is printed.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_yaml::Value;
use std::path::Path;

use crate::Ctx;

/// One line of the generated journey.
#[derive(Debug, PartialEq)]
enum Line {
    /// A real step body, e.g. `tap:label:Sign in` or `assert: textPresent:Hi`.
    /// The leading `do: ` / `assert: ` is part of the string so render is dumb.
    Step(String),
    /// A command with no clean reproit equivalent: emitted as a comment.
    Todo(String),
    /// A command reproit handles structurally (e.g. launchApp): a note comment.
    Note(String),
}

pub fn run(
    ctx: &Ctx,
    tool: &str,
    path: &Path,
    name: Option<String>,
    out: Option<&Path>,
) -> Result<()> {
    if tool != "maestro" {
        bail!("unknown import tool {tool:?}; supported: maestro");
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (app_id, commands) =
        parse_maestro(&raw).with_context(|| format!("parsing Maestro flow {}", path.display()))?;

    let journey_name = name.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(sanitize_name)
            .unwrap_or_else(|| "imported".to_string())
    });

    let lines: Vec<Line> = commands.iter().map(translate).collect();
    let yaml = render_journey(&journey_name, app_id.as_deref(), &lines);

    let (steps, todos, notes) = lines.iter().fold((0, 0, 0), |(s, t, n), l| match l {
        Line::Step(_) => (s + 1, t, n),
        Line::Todo(_) => (s, t + 1, n),
        Line::Note(_) => (s, t, n + 1),
    });

    match out {
        Some(p) => {
            std::fs::write(p, &yaml).with_context(|| format!("writing {}", p.display()))?;
            ctx.say(format!("wrote journey {} to {}", journey_name, p.display()));
        }
        None => print!("{yaml}"),
    }
    ctx.say(format!(
        "imported {steps} step(s), {todos} TODO (no clean equivalent), {notes} handled by reproit"
    ));
    Ok(())
}

/// Split a Maestro flow into (appId, command list). A flow is the config mapping
/// (appId, tags, ...), a `---` separator, then a sequence of commands. Either doc
/// may be absent; we take appId from the first mapping and commands from the
/// first sequence.
fn parse_maestro(raw: &str) -> Result<(Option<String>, Vec<Value>)> {
    let mut app_id = None;
    let mut commands = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(raw) {
        let v = Value::deserialize(doc).context("invalid YAML document")?;
        match v {
            Value::Mapping(m) => {
                if app_id.is_none() {
                    if let Some(Value::String(a)) = m.get(Value::from("appId")) {
                        app_id = Some(a.clone());
                    }
                }
            }
            Value::Sequence(s) if commands.is_empty() => commands = s,
            _ => {}
        }
    }
    if commands.is_empty() {
        bail!("no command sequence found (expected a Maestro flow: config, `---`, then a list of commands)");
    }
    Ok((app_id, commands))
}

/// Translate one Maestro command (a bare string like `back`, or a single-key map
/// like `{tapOn: "Sign in"}`) into a journey line.
fn translate(cmd: &Value) -> Line {
    use Line::{Note, Todo};
    match cmd {
        Value::String(s) => match s.as_str() {
            "launchApp" => Note("launchApp (reproit launches the app itself)".into()),
            "back" => Line::Step("do: back".into()),
            "stopApp" | "clearState" | "clearKeychain" => {
                Note(format!("{s} (use reproit reset: steps in config)"))
            }
            "waitForAnimationToEnd" | "hideKeyboard" => {
                Note(format!("{s} (reproit auto-settles between actions)"))
            }
            "scroll" | "scrollUntilVisible" => Todo(format!(
                "{s} (no reproit scroll action; reach the target with goto: or a tap:)"
            )),
            other => Todo(other.to_string()),
        },
        Value::Mapping(m) if m.len() == 1 => {
            let (k, v) = m.iter().next().unwrap();
            let key = k.as_str().unwrap_or("");
            match key {
                "tapOn" | "longPressOn" => match tap_selector(v) {
                    Some(sel) => Line::Step(format!("do: {sel}")),
                    None => Todo(format!("{key} {} (no text/id selector)", summarize(v))),
                },
                "inputText" => match v.as_str() {
                    Some(t) => Line::Step(format!("do: type:{t}")),
                    None => Todo(format!("inputText {}", summarize(v))),
                },
                "assertVisible" => match assert_selector(v) {
                    Some(a) => Line::Step(format!("assert: {a}")),
                    None => Todo(format!("assertVisible {}", summarize(v))),
                },
                "takeScreenshot" => Line::Step(format!("do: shoot:{}", shot_name(v))),
                "launchApp" => Note("launchApp (reproit launches the app itself)".into()),
                "runFlow" => Todo(format!(
                    "runFlow {} (import the sub-flow separately)",
                    summarize(v)
                )),
                other => Todo(format!("{other} {}", summarize(v))),
            }
        }
        other => Todo(summarize(other)),
    }
}

/// Maestro tap selector -> a reproit tap finder. Text -> tap:label:, id -> tap:key:.
fn tap_selector(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(format!("tap:label:{s}")),
        Value::Mapping(m) => {
            if let Some(Value::String(id)) = m.get(Value::from("id")) {
                Some(format!("tap:key:{id}"))
            } else if let Some(Value::String(t)) = m.get(Value::from("text")) {
                Some(format!("tap:label:{t}"))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Maestro assertVisible selector -> a reproit assertion. Text -> textPresent;
/// id -> count:key:<id>:1 (visible == at least the keyed element present).
fn assert_selector(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(format!("textPresent:{s}")),
        Value::Mapping(m) => {
            if let Some(Value::String(t)) = m.get(Value::from("text")) {
                Some(format!("textPresent:{t}"))
            } else if let Some(Value::String(id)) = m.get(Value::from("id")) {
                Some(format!("count:key:{id}:1"))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// takeScreenshot name (a string, or {path|name: ...}), sanitized to the SHOOT
/// charset. Empty -> "screen".
fn shot_name(v: &Value) -> String {
    let raw = match v {
        Value::String(s) => s.clone(),
        Value::Mapping(m) => m
            .get(Value::from("path"))
            .or_else(|| m.get(Value::from("name")))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    };
    let cleaned: String = raw
        .trim_end_matches(".png")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-'))
        .collect();
    if cleaned.is_empty() {
        "screen".into()
    } else {
        cleaned
    }
}

fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A short, single-line description of a value for TODO comments.
fn summarize(v: &Value) -> String {
    let s = serde_yaml::to_string(v).unwrap_or_default();
    s.replace('\n', " ").trim().chars().take(80).collect()
}

fn render_journey(name: &str, app_id: Option<&str>, lines: &[Line]) -> String {
    let mut out = String::new();
    out.push_str("# Imported from a Maestro flow by `reproit import maestro`.\n");
    if let Some(a) = app_id {
        out.push_str(&format!(
            "# Maestro appId: {a} (set app.platform / app.bundleId in reproit.yaml).\n"
        ));
    }
    out.push_str("# Review before running. `# TODO(maestro)` lines had no clean reproit\n");
    out.push_str("# equivalent and were left as comments.\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str("steps:\n");
    for line in lines {
        match line {
            Line::Step(s) => out.push_str(&format!("  - {s}\n")),
            Line::Todo(t) => out.push_str(&format!("  # TODO(maestro): {t}\n")),
            Line::Note(n) => out.push_str(&format!("  # note: {n}\n")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(yaml: &str) -> Line {
        let v: Value = serde_yaml::from_str(yaml).unwrap();
        translate(&v)
    }

    #[test]
    fn maps_common_commands() {
        assert_eq!(
            tr("tapOn: \"Sign in\""),
            Line::Step("do: tap:label:Sign in".into())
        );
        assert_eq!(
            tr("tapOn: {id: submit}"),
            Line::Step("do: tap:key:submit".into())
        );
        assert_eq!(tr("inputText: hello"), Line::Step("do: type:hello".into()));
        assert_eq!(
            tr("assertVisible: Welcome"),
            Line::Step("assert: textPresent:Welcome".into())
        );
        assert_eq!(
            tr("assertVisible: {id: list}"),
            Line::Step("assert: count:key:list:1".into())
        );
        assert_eq!(
            tr("takeScreenshot: home"),
            Line::Step("do: shoot:home".into())
        );
        assert_eq!(
            tr("takeScreenshot: {path: \"shots/detail.png\"}"),
            Line::Step("do: shoot:shots/detail".into())
        );
        assert_eq!(tr("back"), Line::Step("do: back".into()));
    }

    #[test]
    fn unmappable_commands_become_todos_not_dropped() {
        assert!(matches!(tr("scroll"), Line::Todo(_)));
        assert!(matches!(tr("swipe: {direction: UP}"), Line::Todo(_)));
        assert!(matches!(tr("runFlow: sub.yaml"), Line::Todo(_)));
        assert!(matches!(tr("launchApp"), Line::Note(_)));
    }

    #[test]
    fn parses_two_doc_flow_and_renders() {
        let flow = "appId: com.example.app\n---\n- launchApp\n- tapOn: \"Sign in\"\n- inputText: user@example.com\n- assertVisible: Welcome\n- takeScreenshot: home\n";
        let (app_id, cmds) = parse_maestro(flow).unwrap();
        assert_eq!(app_id.as_deref(), Some("com.example.app"));
        assert_eq!(cmds.len(), 5);
        let lines: Vec<Line> = cmds.iter().map(translate).collect();
        let y = render_journey("login", app_id.as_deref(), &lines);
        assert!(y.contains("name: login"));
        assert!(y.contains("do: tap:label:Sign in"));
        assert!(y.contains("do: type:user@example.com"));
        assert!(y.contains("assert: textPresent:Welcome"));
        assert!(y.contains("do: shoot:home"));
        assert!(y.contains("# Maestro appId: com.example.app"));
    }
}
