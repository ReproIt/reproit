//! `reproit import maestro <flow.yaml>`: translate a Maestro flow into a reproit
//! journey, so switching off Maestro costs ~0. The common commands map onto the
//! reproit action/assert grammar (tap:/type:/back/shoot:/assert:); `runFlow`
//! sub-flows are inlined and literal `repeat` loops unrolled, so real (modular)
//! suites convert, not just toy flows. Anything without a faithful reproit
//! equivalent is emitted as a `# TODO(maestro): ...` line so a command is never
//! silently dropped, and an import summary (mapped vs unsupported) prints at the
//! end.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::path::Path;

use crate::appmap::AppMap;
use crate::layout;
use crate::Ctx;

/// One line of the generated journey.
#[derive(Debug, PartialEq)]
enum Line {
    /// A real step body, e.g. `tap:key:testid:sign-in` or `assert: textPresent:Hi`.
    /// The leading `do: ` / `assert: ` is part of the string so render is dumb.
    Step(String),
    /// A command with no faithful reproit equivalent: emitted as a comment. The
    /// string starts with the Maestro command name (used by the report).
    Todo(String),
    /// Informational comment (a command reproit handles structurally, or a
    /// runFlow/repeat boundary).
    Note(String),
}

#[derive(Debug, Default)]
struct TapResolver {
    by_label: BTreeMap<String, Option<String>>,
}

impl TapResolver {
    fn from_current_map() -> Self {
        let Ok(root) = std::env::current_dir() else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(layout::appmap_path(&root)) else {
            return Self::default();
        };
        let Ok(map) = serde_json::from_str::<AppMap>(&raw) else {
            return Self::default();
        };
        Self::from_map(&map)
    }

    fn from_map(map: &AppMap) -> Self {
        let mut by_label: BTreeMap<String, Option<String>> = BTreeMap::new();
        for state in map.states.values() {
            for element in &state.elements {
                let key = normalize_label(&element.label);
                if key.is_empty() || element.sel.is_empty() {
                    continue;
                }
                add_resolution(&mut by_label, key, &element.sel);
            }
            for text in &state.texts {
                let key = normalize_label(&text.text);
                let Some(text_bounds) = text.bounds else {
                    continue;
                };
                let mut matches = std::collections::BTreeSet::new();
                for element in &state.elements {
                    let Some(element_bounds) = element.bounds else {
                        continue;
                    };
                    if element.sel.is_empty() {
                        continue;
                    }
                    if bounds_match(element_bounds, text_bounds) {
                        matches.insert(element.sel.clone());
                    }
                }
                if matches.len() == 1 {
                    add_resolution(&mut by_label, key, matches.iter().next().unwrap());
                }
            }
        }
        Self { by_label }
    }

    fn resolve(&self, label: &str) -> Option<&str> {
        self.by_label
            .get(&normalize_label(label))
            .and_then(Option::as_deref)
    }
}

fn add_resolution(index: &mut BTreeMap<String, Option<String>>, key: String, sel: &str) {
    index
        .entry(key)
        .and_modify(|existing| {
            if existing.as_deref() != Some(sel) {
                *existing = None;
            }
        })
        .or_insert_with(|| Some(sel.to_string()));
}

fn normalize_label(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn bounds_match(element: [i64; 4], text: [i64; 4]) -> bool {
    let [ex, ey, ew, eh] = element;
    let [tx, ty, tw, th] = text;
    let contains = tx >= ex && ty >= ey && tx + tw <= ex + ew && ty + th <= ey + eh;
    if contains {
        return true;
    }
    let ix = (ex + ew).min(tx + tw) - ex.max(tx);
    let iy = (ey + eh).min(ty + th) - ey.max(ty);
    if ix <= 0 || iy <= 0 {
        return false;
    }
    let intersection = ix * iy;
    let text_area = tw * th;
    text_area > 0 && intersection * 2 >= text_area
}

/// Cap on runFlow include depth, so a cyclic `runFlow` chain cannot loop forever.
const MAX_DEPTH: usize = 8;
/// Cap on `repeat` unrolling, so a huge literal count cannot explode the journey.
const MAX_REPEAT: u64 = 20;

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
    let (app_id, commands) =
        load_flow(path).with_context(|| format!("reading Maestro flow {}", path.display()))?;
    let base = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let journey_name = name.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(sanitize_name)
            .unwrap_or_else(|| "imported".to_string())
    });

    let resolver = TapResolver::from_current_map();
    let lines = translate_all(&commands, &base, 0, &resolver);
    let yaml = render_journey(&journey_name, app_id.as_deref(), &lines);

    // Import summary: count mapped steps and, per unsupported Maestro command,
    // how many times it appeared.
    let mut steps = 0usize;
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    for l in &lines {
        match l {
            Line::Step(_) => steps += 1,
            Line::Todo(t) => {
                let cmd = t.split_whitespace().next().unwrap_or("?").to_string();
                *unsupported.entry(cmd).or_insert(0) += 1;
            }
            Line::Note(_) => {}
        }
    }
    let todo_total: usize = unsupported.values().sum();

    match out {
        Some(p) => {
            std::fs::write(p, &yaml).with_context(|| format!("writing {}", p.display()))?;
            ctx.say(format!("wrote journey {journey_name} to {}", p.display()));
        }
        None => print!("{yaml}"),
    }

    // The import summary is the trust feature: measurable, honest.
    let total = steps + todo_total;
    let pct = (steps * 100).checked_div(total).unwrap_or(100);
    ctx.say(format!(
        "\nMaestro import: {steps}/{total} commands mapped ({pct}%)."
    ));
    if unsupported.is_empty() {
        ctx.say("  every command had a reproit equivalent.");
    } else {
        ctx.say("  unsupported (left as # TODO(maestro) comments):");
        for (cmd, n) in &unsupported {
            ctx.say(format!("    {cmd} x{n}"));
        }
        ctx.say(
            "  fixes: run the import again after reproit refreshes its internal model so unique text taps resolve to \
             structural selectors, or give tapped elements stable ids; replace \
             scroll/swipe with a goto: or a keyed tap:; port runScript logic into \
             a reset: step or an invariant.",
        );
    }
    // The handoff: a converted flow isn't just a replayable script, it's a
    // launchpad. `fuzz --from` replays it to its end state, then hunts the bugs
    // it never covered. (Only meaningful once the journey lives on disk.)
    if out.is_some() {
        ctx.say(format!(
            "\nNext: `reproit check {journey_name}` to replay it, or \
             `reproit fuzz --from {journey_name}` to find the bugs it never covered."
        ));
    }
    Ok(())
}

/// Read + parse a Maestro flow file into (appId, command list).
fn load_flow(path: &Path) -> Result<(Option<String>, Vec<Value>)> {
    let raw = std::fs::read_to_string(path)?;
    parse_maestro(&raw)
}

/// Split a Maestro flow into (appId, command list): the config mapping (appId,
/// tags, ...), a `---` separator, then a sequence of commands. We take appId from
/// the first mapping and commands from the first sequence (either may be absent).
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

/// Translate a command list, recursing into runFlow / repeat / when, into the
/// flattened journey lines.
fn translate_all(cmds: &[Value], base: &Path, depth: usize, resolver: &TapResolver) -> Vec<Line> {
    cmds.iter()
        .flat_map(|c| translate(c, base, depth, resolver))
        .collect()
}

/// Translate one Maestro command. Returns multiple lines because `runFlow`
/// inlines a sub-flow and `repeat` unrolls.
fn translate(cmd: &Value, base: &Path, depth: usize, resolver: &TapResolver) -> Vec<Line> {
    use Line::{Note, Step, Todo};
    match cmd {
        Value::String(s) => vec![match s.as_str() {
            "launchApp" => Note("launchApp (reproit launches the app itself)".into()),
            "back" => Step("do: back".into()),
            "stopApp" | "clearState" | "clearKeychain" => {
                Note(format!("{s} (use reproit reset: steps in config)"))
            }
            "waitForAnimationToEnd" | "hideKeyboard" => {
                Note(format!("{s} (reproit auto-settles between actions)"))
            }
            "scroll" | "scrollUntilVisible" | "swipe" => Todo(format!(
                "{s} (no reproit scroll action; reach the target with goto: or a keyed tap:)"
            )),
            "inputRandomEmail" => return random_input("demo@example.com", "inputRandomEmail"),
            "inputRandomText" => return random_input("Sample text", "inputRandomText"),
            "inputRandomPersonName" => return random_input("Jane Doe", "inputRandomPersonName"),
            "inputRandomNumber" => return random_input("12345", "inputRandomNumber"),
            other => Todo(other.to_string()),
        }],
        Value::Mapping(m) if m.len() == 1 => {
            let (k, v) = m.iter().next().unwrap();
            let key = k.as_str().unwrap_or("");
            match key {
                "tapOn" | "longPressOn" => match tap_selector(v, resolver) {
                    Some(sel) => vec![Step(format!("do: {sel}"))],
                    None => vec![Todo(format!(
                        "{key} {} (no text/id selector)",
                        summarize(v)
                    ))],
                },
                "inputText" => match v.as_str() {
                    Some(t) => vec![Step(format!("do: type:{t}"))],
                    None => vec![Todo(format!("inputText {}", summarize(v)))],
                },
                "assertVisible" => match assert_selector(v) {
                    Some(a) => vec![Step(format!("assert: {a}"))],
                    None => vec![Todo(format!("assertVisible {}", summarize(v)))],
                },
                // Maestro waits-until-visible map to a reproit assertion: reproit
                // already settles before asserting, so the wait is implicit.
                "extendedWaitUntil" | "waitUntilVisible" => match wait_target(v) {
                    Some(a) => vec![Step(format!("assert: {a}"))],
                    None => vec![Todo(format!("{key} {}", summarize(v)))],
                },
                "takeScreenshot" => vec![Step(format!("do: shoot:{}", shot_name(v)))],
                "pressKey" => match v.as_str().map(|k| k.to_ascii_lowercase()) {
                    Some(ref k) if k == "back" => vec![Step("do: back".into())],
                    _ => vec![Todo(format!(
                        "pressKey {} (no reproit key action)",
                        summarize(v)
                    ))],
                },
                "runFlow" => run_flow(v, base, depth, resolver),
                "repeat" => repeat_block(v, base, depth, resolver),
                "launchApp" => vec![Note("launchApp (reproit launches the app itself)".into())],
                other => vec![Todo(format!("{other} {}", summarize(v)))],
            }
        }
        other => vec![Todo(summarize(other))],
    }
}

/// inputRandom* -> a literal type plus a note (Maestro generates a fresh value
/// each run; reproit is deterministic, so we substitute a stable placeholder).
fn random_input(literal: &str, cmd: &str) -> Vec<Line> {
    vec![
        Line::Note(format!("{cmd} -> deterministic literal (edit if needed)")),
        Line::Step(format!("do: type:{literal}")),
    ]
}

/// `runFlow` -> inline a sub-flow in place, bounded by note comments. Maestro has
/// three shapes: a bare `runFlow: sub.yaml`, `runFlow: {file: sub.yaml, env: ...}`,
/// and an inline `runFlow: {commands: [...]}`. An optional `when:` makes it
/// conditional, which is a runtime decision reproit cannot make deterministically,
/// so the body is inlined unconditionally with a note. A missing file / too-deep
/// chain becomes a TODO so it is never silently dropped.
fn run_flow(v: &Value, base: &Path, depth: usize, resolver: &TapResolver) -> Vec<Line> {
    if depth >= MAX_DEPTH {
        return vec![Line::Todo(format!(
            "runFlow {} (include depth > {MAX_DEPTH}; inline by hand)",
            summarize(v)
        ))];
    }
    let conditional = matches!(v, Value::Mapping(m) if m.contains_key(Value::from("when")));

    // Inline `commands:` defined in place (no separate file).
    if let Value::Mapping(m) = v {
        if let Some(Value::Sequence(cmds)) = m.get(Value::from("commands")) {
            let mut out = Vec::new();
            if conditional {
                out.push(Line::Note(
                    "runFlow when:... (runtime condition dropped; body inlined)".into(),
                ));
            }
            out.push(Line::Note("runFlow (inline commands)".into()));
            out.extend(translate_all(cmds, base, depth + 1, resolver));
            out.push(Line::Note("end runFlow".into()));
            return out;
        }
    }

    // Otherwise it references a file: bare string, or `{file: ...}`.
    let (sub_path, has_env) = match v {
        Value::String(s) => (Some(s.clone()), false),
        Value::Mapping(m) => (
            m.get(Value::from("file"))
                .and_then(|x| x.as_str())
                .map(str::to_string),
            m.contains_key(Value::from("env")),
        ),
        _ => (None, false),
    };
    let Some(sub) = sub_path else {
        return vec![Line::Todo(format!(
            "runFlow {} (no file or commands)",
            summarize(v)
        ))];
    };
    let full = base.join(&sub);
    let (_app, cmds) = match load_flow(&full) {
        Ok(x) => x,
        Err(_) => {
            return vec![Line::Todo(format!(
                "runFlow {sub} (sub-flow not found at {})",
                full.display()
            ))]
        }
    };
    let mut out = Vec::new();
    if conditional {
        out.push(Line::Note(format!(
            "runFlow {sub} when:... (condition dropped; inlined)"
        )));
    }
    out.push(Line::Note(format!("runFlow {sub} (inlined)")));
    if has_env {
        out.push(Line::Note(format!(
            "runFlow {sub} passed env: vars; reproit journeys have no per-flow env, \
             wire them via config defines/secrets if needed"
        )));
    }
    let inner_base = full.parent().unwrap_or(base).to_path_buf();
    out.extend(translate_all(&cmds, &inner_base, depth + 1, resolver));
    out.push(Line::Note(format!("end runFlow {sub}")));
    out
}

/// `repeat: {times: N, commands: [...]}` -> unroll N times (literal N only, capped
/// at MAX_REPEAT). A `while:` condition cannot be unrolled deterministically, so
/// the body is inlined once with a note.
fn repeat_block(v: &Value, base: &Path, depth: usize, resolver: &TapResolver) -> Vec<Line> {
    let Value::Mapping(m) = v else {
        return vec![Line::Todo(format!("repeat {}", summarize(v)))];
    };
    let body = match m.get(Value::from("commands")) {
        Some(Value::Sequence(s)) => s.clone(),
        _ => return vec![Line::Todo("repeat (no commands block)".into())],
    };
    if m.contains_key(Value::from("while")) {
        let mut out = vec![Line::Note(
            "repeat while:... (runtime condition; body inlined once)".into(),
        )];
        out.extend(translate_all(&body, base, depth, resolver));
        return out;
    }
    let times = m
        .get(Value::from("times"))
        .and_then(|x| x.as_u64())
        .unwrap_or(1);
    let capped = times.min(MAX_REPEAT);
    let mut out = vec![Line::Note(format!(
        "repeat x{times}{} (unrolled)",
        if capped < times {
            format!(", capped at {capped}")
        } else {
            String::new()
        }
    ))];
    for _ in 0..capped {
        out.extend(translate_all(&body, base, depth, resolver));
    }
    out
}

/// Maestro tap selector -> a reproit tap finder. Text selectors are not mapped:
/// reproit tap actions use structural selectors only.
fn tap_selector(v: &Value, resolver: &TapResolver) -> Option<String> {
    match v {
        Value::String(s) => resolver.resolve(s).map(|sel| format!("tap:{sel}")),
        Value::Mapping(m) => {
            if let Some(Value::String(id)) = m.get(Value::from("id")) {
                Some(format!("tap:key:{id}"))
            } else if let Some(Value::String(t)) = m.get(Value::from("text")) {
                resolver.resolve(t).map(|sel| format!("tap:{sel}"))
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

/// extendedWaitUntil/waitUntilVisible target -> an assertion. Maestro nests the
/// matcher under `visible:`; we accept that or a bare matcher.
fn wait_target(v: &Value) -> Option<String> {
    if let Value::Mapping(m) = v {
        if let Some(inner) = m.get(Value::from("visible")) {
            return assert_selector(inner);
        }
    }
    assert_selector(v)
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

/// A short, single-line description of a value for TODO/Note comments.
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
    out.push_str("# Review before running. `# TODO(maestro)` lines had no faithful reproit\n");
    out.push_str("# equivalent and were left as comments (see the import summary).\n");
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

    fn tr(yaml: &str) -> Vec<Line> {
        let v: Value = serde_yaml::from_str(yaml).unwrap();
        translate(&v, Path::new("."), 0, &TapResolver::default())
    }

    fn resolver_with(label: &str, sel: &str) -> TapResolver {
        resolver_with_element_and_text(label, sel, vec![])
    }

    fn resolver_with_element_and_text(
        element_label: &str,
        sel: &str,
        texts: Vec<crate::appmap::StateText>,
    ) -> TapResolver {
        let mut states = BTreeMap::new();
        states.insert(
            "home".to_string(),
            crate::appmap::State {
                description: "home".to_string(),
                signature: crate::appmap::StateSignature {
                    screenshot_phash: None,
                    semantics_hash: Some("sig-home".to_string()),
                    route: None,
                },
                elements: vec![crate::appmap::StateElement {
                    sel: sel.to_string(),
                    role: "button".to_string(),
                    label: element_label.to_string(),
                    input_purpose: None,
                    bounds: Some([10, 10, 100, 30]),
                }],
                texts,
                parameters: vec![],
                operability_gaps: Default::default(),
            },
        );
        TapResolver::from_map(&AppMap {
            app: "demo".to_string(),
            version: 1,
            states,
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        })
    }

    #[test]
    fn maps_common_commands() {
        assert_eq!(
            tr("tapOn: \"Sign in\"")[0],
            Line::Todo("tapOn Sign in (no text/id selector)".into())
        );
        assert_eq!(
            tr("tapOn: {id: submit}")[0],
            Line::Step("do: tap:key:submit".into())
        );
        assert_eq!(
            tr("inputText: hello")[0],
            Line::Step("do: type:hello".into())
        );
        assert_eq!(
            tr("assertVisible: Welcome")[0],
            Line::Step("assert: textPresent:Welcome".into())
        );
        assert_eq!(
            tr("assertVisible: {id: list}")[0],
            Line::Step("assert: count:key:list:1".into())
        );
        assert_eq!(
            tr("takeScreenshot: home")[0],
            Line::Step("do: shoot:home".into())
        );
        assert_eq!(tr("back")[0], Line::Step("do: back".into()));
        assert_eq!(tr("pressKey: Back")[0], Line::Step("do: back".into()));
    }

    #[test]
    fn text_taps_resolve_through_the_existing_map_when_unique() {
        let resolver = resolver_with("Sign in", "key:testid:sign-in");
        let v: Value = serde_yaml::from_str("tapOn: \"Sign in\"").unwrap();
        assert_eq!(
            translate(&v, Path::new("."), 0, &resolver)[0],
            Line::Step("do: tap:key:testid:sign-in".into())
        );
        let v: Value = serde_yaml::from_str("tapOn: {text: \"Sign in\"}").unwrap();
        assert_eq!(
            translate(&v, Path::new("."), 0, &resolver)[0],
            Line::Step("do: tap:key:testid:sign-in".into())
        );
    }

    #[test]
    fn text_taps_resolve_through_text_bounds_when_label_is_missing() {
        let resolver = resolver_with_element_and_text(
            "",
            "role:button#0",
            vec![crate::appmap::StateText {
                text: "Sign in".to_string(),
                bounds: Some([25, 16, 50, 14]),
            }],
        );
        let v: Value = serde_yaml::from_str("tapOn: \"Sign in\"").unwrap();
        assert_eq!(
            translate(&v, Path::new("."), 0, &resolver)[0],
            Line::Step("do: tap:role:button#0".into())
        );
    }

    #[test]
    fn waits_become_assertions() {
        assert_eq!(
            tr("extendedWaitUntil: {visible: {text: Loaded}, timeout: 5000}")[0],
            Line::Step("assert: textPresent:Loaded".into())
        );
    }

    #[test]
    fn random_inputs_become_deterministic_literals() {
        let v = tr("inputRandomEmail");
        assert_eq!(v.len(), 2);
        assert!(matches!(v[0], Line::Note(_)));
        assert_eq!(v[1], Line::Step("do: type:demo@example.com".into()));
    }

    #[test]
    fn repeat_unrolls_a_literal_count() {
        let v = tr("repeat: {times: 3, commands: [{tapOn: Next}]}");
        assert_eq!(v.len(), 4); // 1 note + 3 unrolled taps
        assert!(matches!(v[0], Line::Note(_)));
        assert_eq!(v[1], Line::Todo("tapOn Next (no text/id selector)".into()));
        assert_eq!(v[3], Line::Todo("tapOn Next (no text/id selector)".into()));
    }

    #[test]
    fn run_flow_inlines_inline_commands_and_drops_the_when_condition() {
        // runFlow with an inline commands: block and a when: modifier. The body is
        // inlined (a note flags the dropped condition; the tap comes through).
        let v = tr("runFlow: {when: {visible: Banner}, commands: [{tapOn: Dismiss}]}");
        assert!(v
            .iter()
            .any(|l| matches!(l, Line::Todo(s) if s == "tapOn Dismiss (no text/id selector)")));
        assert!(v
            .iter()
            .any(|l| matches!(l, Line::Note(n) if n.contains("condition"))));
    }

    #[test]
    fn unsupported_commands_become_todos_not_dropped() {
        assert!(matches!(tr("scroll")[0], Line::Todo(_)));
        assert!(matches!(tr("swipe: {direction: UP}")[0], Line::Todo(_)));
        assert!(matches!(tr("assertNotVisible: Spinner")[0], Line::Todo(_)));
        assert!(matches!(tr("runScript: foo.js")[0], Line::Todo(_)));
        assert!(matches!(tr("launchApp")[0], Line::Note(_)));
    }

    #[test]
    fn parses_two_doc_flow_and_renders() {
        let flow = "appId: com.example.app\n---\n- launchApp\n- tapOn: \"Sign in\"\n- inputText: user@example.com\n- assertVisible: Welcome\n- takeScreenshot: home\n";
        let (app_id, cmds) = parse_maestro(flow).unwrap();
        assert_eq!(app_id.as_deref(), Some("com.example.app"));
        let lines = translate_all(&cmds, Path::new("."), 0, &TapResolver::default());
        let y = render_journey("login", app_id.as_deref(), &lines);
        assert!(y.contains("name: login"));
        assert!(y.contains("# TODO(maestro): tapOn Sign in (no text/id selector)"));
        assert!(y.contains("assert: textPresent:Welcome"));
        assert!(y.contains("do: shoot:home"));
        assert!(y.contains("# Maestro appId: com.example.app"));
    }
}
