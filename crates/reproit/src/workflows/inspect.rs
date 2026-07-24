//! Human review for one deterministic platform-native replay.
//!
//! Inspection deliberately reuses the standard check engine and verdict. The
//! only behavioral difference is action gating on the configured real execution
//! tier. Because human pacing changes timing, an inspection writes evidence but
//! never updates or promotes the saved guard.

use super::check::{self, CheckArgs};
use crate::adapters::config;
use crate::domain::repro;
use crate::interface::cli::context::Ctx;
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_JSONL_ITEMS: usize = 256;
const MAX_TIMELINE_STEPS: usize = 128;

pub(super) async fn run(
    ctx: &Ctx,
    config_path: Option<&Path>,
    raw_reference: &str,
) -> Result<ExitCode> {
    if ctx.json || ctx.quiet {
        anyhow::bail!("`reproit inspect` is interactive and does not support --json or --quiet");
    }
    let loaded = config::load(config_path)?;
    if loaded.config.app.platform == "flutter" {
        let runner = loaded
            .root
            .join(&loaded.config.app.project_dir)
            .join(&loaded.config.journeys.dir)
            .join("reproit_explorer/runner.dart");
        ensure_flutter_inspection_runner(&runner)?;
    }
    let reference = raw_reference.strip_prefix('@').unwrap_or(raw_reference);
    if reference.starts_with("bkt_") && repro::resolve(&loaded.root, reference).is_none() {
        let (cloud, key) = super::cloud::cloud_creds(None, None);
        super::triage::pull_global(&loaded.root, reference, reference, false, cloud, key).await?;
    }
    let _control = (loaded.config.app.platform != "web")
        .then(|| InspectionControl::start(&loaded.root, &loaded.config.app.platform))
        .transpose()?;
    ctx.say(format!(
        "Opening the replay on the configured `{}` target.",
        loaded.config.app.platform
    ));
    if loaded.config.app.platform == "web" {
        ctx.say("Use Run next action, Continue to failure, or the Enter and C shortcuts.\n");
    } else {
        ctx.say("Use Enter to run the next action or C to continue to failure.\n");
    }
    check::run(
        ctx,
        config_path,
        CheckArgs {
            repro: Some(reference.to_string()),
            devices: 1,
            kind: None,
            runs: Some(1),
            junit: None,
            strict: true,
            locale: None,
            target: None,
            device: None,
            record_video: false,
            flicker: false,
            changed: None,
            inspect: true,
        },
    )
    .await
}

fn ensure_flutter_inspection_runner(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let runner =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if runner.contains("inspectPlatformStep(") {
        return Ok(());
    }
    anyhow::bail!(
        "the Flutter project has an older ReproIt explorer without action inspection support. \
         Refresh the generated scaffold with `reproit init --platform flutter --force`, review \
         the scaffold changes, then run inspect again"
    )
}

struct InspectionControl {
    dir: PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    _env: crate::adapters::scoped_env::ScopedEnv,
}

impl InspectionControl {
    fn start(root: &Path, platform: &str) -> Result<Self> {
        let parent = root.join(".reproit/inspect");
        std::fs::create_dir_all(&parent)?;
        let dir = claim_control_dir(&parent)?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_dir = dir.clone();
        let platform = platform.to_string();
        let worker = std::thread::spawn(move || {
            control_loop(&worker_dir, &platform, &worker_stop);
        });
        let env = crate::adapters::scoped_env::ScopedEnv::set(vec![(
            "REPROIT_INSPECT_CONTROL".to_string(),
            dir.to_string_lossy().into_owned(),
        )]);
        Ok(Self {
            dir,
            stop,
            worker: Some(worker),
            _env: env,
        })
    }
}

impl Drop for InspectionControl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn claim_control_dir(parent: &Path) -> Result<PathBuf> {
    let base = format!("session-{}", std::process::id());
    for suffix in 0..100 {
        let name = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}-{suffix}")
        };
        let candidate = parent.join(name);
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("could not claim a bounded inspection control directory")
}

fn control_loop(dir: &Path, platform: &str, stop: &AtomicBool) {
    let request_path = dir.join("request.json");
    let response_path = dir.join("response.json");
    let mut last_sequence = 0_u64;
    while !stop.load(Ordering::Acquire) {
        let Some(request) = read_json(&request_path) else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let sequence = request["sequence"].as_u64().unwrap_or(0);
        if sequence == 0 || sequence == last_sequence {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        last_sequence = sequence;
        let decision = prompt_for_action(platform, &request);
        let response = json!({"sequence": sequence, "decision": decision});
        let _ = write_control_response(dir, &response_path, &response);
    }
}

fn write_control_response(dir: &Path, path: &Path, response: &Value) -> std::io::Result<()> {
    let temp = dir.join("response.json.tmp");
    std::fs::write(&temp, response.to_string())?;
    // Windows rename does not replace an existing destination. A brief gap is
    // safe because runners poll for a response with the matching sequence.
    let _ = std::fs::remove_file(path);
    std::fs::rename(temp, path)
}

fn prompt_for_action(platform: &str, request: &Value) -> &'static str {
    let step = request["step"].as_u64().unwrap_or(0);
    let total = request["total"].as_u64().unwrap_or(0);
    let action = request["action"].as_str().unwrap_or("unknown action");
    println!("\ninspect [{platform}] {step}/{total}: {action}");
    if let Some(target) = request["target"].as_str().filter(|value| !value.is_empty()) {
        println!("  target: {target}");
    }
    if let Some(state) = request["state"].as_str().filter(|value| !value.is_empty()) {
        println!("{state}");
    }
    print!("  Enter: next, C: continue, Q: stop > ");
    let _ = std::io::stdout().flush();
    if !std::io::stdin().is_terminal() {
        println!("continue");
        return "continue";
    }
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return "abort";
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "c" | "continue" => "continue",
        "q" | "quit" | "stop" => "abort",
        _ => "step",
    }
}

pub(super) fn write_fix_packet(
    loaded: &config::Loaded,
    meta: &repro::Meta,
    result: &repro::CheckResult,
    run_dir: &Path,
) -> Result<()> {
    let replay_path = repro::repro_dir(&loaded.root, &meta.id).join("replay.json");
    let replay: Value = read_json(&replay_path).unwrap_or_else(|| json!({}));
    let actions = replay["replay"].as_array().cloned().unwrap_or_default();
    let trigger_index = meta
        .trigger_index
        .unwrap_or(actions.len())
        .min(actions.len());
    let log = read_bounded_text(&run_dir.join("drive-a.log"), MAX_LOG_BYTES)?;
    let exceptions = read_json_lines(
        &run_dir.join("exceptions.jsonl"),
        MAX_JSONL_ITEMS,
        MAX_LOG_BYTES,
    )?;
    let network = network_near_trigger(run_dir, trigger_index)?;
    let timeline = parse_timeline(&log, trigger_index);
    let expected = expected_failure(loaded, meta, &exceptions);
    let sources = source_candidates(&exceptions);
    let packet = json!({
        "version": 1,
        "repro": {
            "id": repro::display_repro_id(&meta.id),
            "alias": meta.alias,
            "platform": loaded.config.app.platform,
            "status": meta.status.as_str(),
            "oracle": meta.oracle,
            "signature": meta.trigger_sig,
        },
        "result": {
            "outcome": result.outcome.as_str(),
            "green": result.green,
            "total": result.total,
        },
        "expected": expected,
        "trigger": {
            "index": trigger_index,
            "action": actions.get(trigger_index.saturating_sub(1)),
        },
        "steps": timeline,
        "exceptions": exceptions,
        "sourceCandidates": sources,
        "networkNearTrigger": network,
        "evidence": {
            "runDir": run_dir,
            "driveLog": run_dir.join("drive-a.log"),
            "actions": run_dir.join("actions.jsonl"),
            "exceptions": run_dir.join("exceptions.jsonl"),
            "network": run_dir.join("network-a.jsonl"),
        },
        "verify": format!("reproit @{}", meta.alias.as_deref().unwrap_or(&meta.id)),
    });
    let json_path = run_dir.join("fix-packet.json");
    let markdown_path = run_dir.join("fix-packet.md");
    std::fs::write(&json_path, serde_json::to_string_pretty(&packet)?)
        .with_context(|| format!("writing {}", json_path.display()))?;
    std::fs::write(&markdown_path, render_markdown(&packet))
        .with_context(|| format!("writing {}", markdown_path.display()))?;
    println!("\nFix packet:");
    println!("  human:   {}", markdown_path.display());
    println!("  agent:   {}", json_path.display());
    Ok(())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&read_bounded_text(path, MAX_LOG_BYTES).ok()?).ok()
}

fn read_bounded_text(path: &Path, max_bytes: u64) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut text = String::new();
    file.take(max_bytes).read_to_string(&mut text)?;
    Ok(text)
}

fn read_json_lines(path: &Path, max_items: usize, max_bytes: u64) -> Result<Vec<Value>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let mut items = VecDeque::with_capacity(max_items);
    let reader = BufReader::new(file.take(max_bytes));
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(value) = serde_json::from_str(&line) else {
            continue;
        };
        if items.len() == max_items {
            items.pop_front();
        }
        items.push_back(value);
    }
    Ok(items.into())
}

fn network_near_trigger(run_dir: &Path, trigger_index: usize) -> Result<Vec<Value>> {
    let facts = read_json_lines(
        &run_dir.join("network-a.jsonl"),
        MAX_JSONL_ITEMS,
        MAX_LOG_BYTES,
    )?;
    Ok(facts
        .into_iter()
        .filter(|fact| {
            fact["actionIndex"].as_u64().is_some_and(|index| {
                let index = index as usize;
                index >= trigger_index.saturating_sub(1) && index <= trigger_index
            })
        })
        .take(32)
        .map(compact_network_fact)
        .collect())
}

fn compact_network_fact(fact: Value) -> Value {
    json!({
        "actionIndex": fact["actionIndex"],
        "method": fact["method"],
        "url": fact["url"],
        "status": fact["status"],
        "requestBody": fact["requestBody"],
        "responseBody": fact["responseBody"],
    })
}

fn parse_timeline(log: &str, trigger_index: usize) -> Vec<Value> {
    let mut steps = Vec::<Value>::new();
    let mut current = None;
    let mut pending: Option<usize> = None;
    for line in log.lines() {
        if let Some(raw) = line.strip_prefix("FUZZ:OBS ") {
            let Ok(observation) = serde_json::from_str::<Value>(raw) else {
                continue;
            };
            let observation = compact_observation(&observation);
            if let Some(index) = pending.take() {
                steps[index]["after"] = observation.clone();
                steps[index]["stateDiff"] = state_diff(&steps[index]["before"], &observation);
            }
            current = Some(observation);
        } else if let Some(action) = line.strip_prefix("FUZZ:ACT ") {
            if steps.len() == MAX_TIMELINE_STEPS {
                break;
            }
            let index = steps.len() + 1;
            steps.push(json!({
                "index": index,
                "action": action,
                "trigger": index == trigger_index,
                "before": current,
                "after": null,
                "stateDiff": null,
            }));
            pending = Some(steps.len() - 1);
        }
    }
    steps
}

fn compact_observation(observation: &Value) -> Value {
    let labels = observation["labels"]
        .as_array()
        .map(|items| items.iter().take(32).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let roles = observation["elements"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .take(48)
                .filter_map(|element| element["role"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "signature": observation["sig"],
        "route": observation["route"],
        "labels": labels,
        "roles": roles,
    })
}

fn state_diff(before: &Value, after: &Value) -> Value {
    let before_labels = string_set(&before["labels"]);
    let after_labels = string_set(&after["labels"]);
    json!({
        "labelsAdded": after_labels.difference(&before_labels).take(32).collect::<Vec<_>>(),
        "labelsRemoved": before_labels.difference(&after_labels).take(32).collect::<Vec<_>>(),
        "routeBefore": before["route"],
        "routeAfter": after["route"],
        "signatureChanged": before["signature"] != after["signature"],
    })
}

fn string_set(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn expected_failure(loaded: &config::Loaded, meta: &repro::Meta, exceptions: &[Value]) -> String {
    let cloud = repro::repro_dir(&loaded.root, &meta.id).join("cloud.json");
    read_json(&cloud)
        .and_then(|value| value["expectedError"].as_str().map(str::to_string))
        .or_else(|| {
            exceptions
                .first()
                .and_then(|value| value["message"].as_str())
                .map(exception_summary)
        })
        .unwrap_or_else(|| meta.trigger_sig.clone().unwrap_or_else(|| "unknown".into()))
}

fn exception_summary(message: &str) -> String {
    let message = message.lines().next().unwrap_or(message).trim();
    const STACK_PREFIXES: &[&str] = &[
        " TypeError:",
        " ReferenceError:",
        " RangeError:",
        " SyntaxError:",
        " URIError:",
        " EvalError:",
        " AggregateError:",
        " DOMException:",
        " Error:",
    ];
    let stack_start = STACK_PREFIXES
        .iter()
        .filter_map(|prefix| message.find(prefix))
        .min()
        .unwrap_or(message.len());
    message[..stack_start].trim().chars().take(500).collect()
}

fn source_candidates(exceptions: &[Value]) -> Vec<String> {
    let frame = Regex::new(
        r"(?:https?://|file://|package:)[^\s)]+:\d+:\d+|(?:[A-Za-z]:[\\/]|/)[^\s)]+:\d+(?::\d+)?",
    )
    .expect("static source-frame regex");
    let mut sources = Vec::new();
    let mut seen = BTreeSet::new();
    for exception in exceptions {
        if let Some(message) = exception["message"].as_str() {
            for candidate in frame.find_iter(message).map(|item| item.as_str()) {
                push_source_candidate(&mut sources, &mut seen, candidate);
            }
        }
        if let Some(frames) = exception["frames"].as_array() {
            for candidate in frames.iter().filter_map(Value::as_str) {
                push_source_candidate(&mut sources, &mut seen, candidate);
            }
        }
    }
    sources
}

fn push_source_candidate(sources: &mut Vec<String>, seen: &mut BTreeSet<String>, candidate: &str) {
    if sources.len() < 16 && seen.insert(candidate.to_string()) {
        sources.push(candidate.to_string());
    }
}

fn render_markdown(packet: &Value) -> String {
    let outcome = packet["result"]["outcome"].as_str().unwrap_or("unknown");
    let expected = packet["expected"].as_str().unwrap_or("unknown");
    let trigger = packet["trigger"]["action"]
        .as_str()
        .unwrap_or("(no trigger action)");
    let mut text = format!(
        "# ReproIt fix packet\n\nOutcome: **{outcome}**\n\nExpected failure: {expected}\n\n\
         Trigger: `{trigger}`\n\n## Replay\n\n"
    );
    if let Some(steps) = packet["steps"].as_array() {
        for step in steps {
            let marker = if step["trigger"].as_bool() == Some(true) {
                " (trigger)"
            } else {
                ""
            };
            text.push_str(&format!(
                "{}. `{}`{marker}\n",
                step["index"].as_u64().unwrap_or(0),
                step["action"].as_str().unwrap_or("?")
            ));
            let added = markdown_labels(&step["stateDiff"]["labelsAdded"]);
            let removed = markdown_labels(&step["stateDiff"]["labelsRemoved"]);
            if !added.is_empty() || !removed.is_empty() {
                text.push_str("   - State:");
                if !added.is_empty() {
                    text.push_str(&format!(" added {added}"));
                }
                if !removed.is_empty() {
                    text.push_str(&format!(" removed {removed}"));
                }
                text.push('\n');
            }
            let route_before = step["stateDiff"]["routeBefore"].as_str();
            let route_after = step["stateDiff"]["routeAfter"].as_str();
            if let (Some(before), Some(after)) = (route_before, route_after) {
                if before != after {
                    text.push_str(&format!("   - Route: `{before}` to `{after}`\n"));
                }
            }
        }
    }
    text.push_str("\n## Source candidates\n\n");
    if let Some(sources) = packet["sourceCandidates"].as_array() {
        for source in sources.iter().filter_map(Value::as_str) {
            text.push_str(&format!("- `{source}`\n"));
        }
    }
    text.push_str(&format!(
        "\n## Verify\n\n```sh\n{}\n```\n",
        packet["verify"].as_str().unwrap_or("reproit check")
    ));
    text
}

fn markdown_labels(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(6)
        .map(|label| format!("`{}`", label.replace('`', "'")))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_marks_the_trigger_and_diffs_adjacent_states() {
        let log = concat!(
            "FUZZ:OBS {\"sig\":\"a\",\"route\":\"/\",\"labels\":[\"Cart 2\"],\"elements\":[]}\n",
            "FUZZ:ACT tap:key:remove\n",
            "FUZZ:OBS {\"sig\":\"b\",\"route\":\"/\",\"labels\":[\"Cart 1\"],\"elements\":[]}\n",
            "FUZZ:ACT tap:key:checkout\n",
            "FUZZ:OBS {\"sig\":\"b\",\"route\":\"/\",\"labels\":[\"Cart 1\"],\"elements\":[]}\n",
        );
        let steps = parse_timeline(log, 2);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["stateDiff"]["labelsAdded"], json!(["Cart 1"]));
        assert_eq!(steps[0]["stateDiff"]["labelsRemoved"], json!(["Cart 2"]));
        assert_eq!(steps[1]["trigger"], json!(true));
    }

    #[test]
    fn source_candidates_extract_web_and_native_frames_once() {
        let exceptions = vec![json!({
            "message": "at checkout (https://app.test/model.js:95:15) \
                        at click (https://app.test/app.js:236:21) \
                        at C:\\src\\cart.rs:51:9 \
                        at /Users/dev/Cart.swift:42:7",
            "frames": ["package:app/cart.dart:42:7", "package:app/cart.dart:42:7"],
        })];
        assert_eq!(
            source_candidates(&exceptions),
            vec![
                "https://app.test/model.js:95:15",
                "https://app.test/app.js:236:21",
                r"C:\src\cart.rs:51:9",
                "/Users/dev/Cart.swift:42:7",
                "package:app/cart.dart:42:7",
            ]
        );
    }

    #[test]
    fn exception_summary_removes_the_flattened_stack() {
        let message = "Loyalty allocation references a removed cart item TypeError: Loyalty \
                       allocation references a removed cart item at checkout \
                       (https://app.test/model.js:95:15)";
        assert_eq!(
            exception_summary(message),
            "Loyalty allocation references a removed cart item"
        );
    }

    #[test]
    fn packet_render_names_the_trigger_and_verification_command() {
        let packet = json!({
            "result": {"outcome": "fail"},
            "expected": "cart invariant",
            "trigger": {"action": "tap:key:checkout"},
            "steps": [{
                "index": 1,
                "action": "tap:key:checkout",
                "trigger": true,
                "stateDiff": {
                    "labelsAdded": ["Checkout failed"],
                    "labelsRemoved": ["Ready"],
                    "routeBefore": "/cart",
                    "routeAfter": "/cart",
                },
            }],
            "sourceCandidates": ["src/cart.ts:42:7"],
            "verify": "reproit @cart",
        });
        let markdown = render_markdown(&packet);
        assert!(markdown.contains("`tap:key:checkout` (trigger)"));
        assert!(markdown.contains("added `Checkout failed` removed `Ready`"));
        assert!(markdown.contains("`src/cart.ts:42:7`"));
        assert!(markdown.contains("reproit @cart"));
    }

    #[test]
    fn control_response_replaces_the_previous_step() {
        let dir = std::env::temp_dir().join(format!("reproit-control-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("response.json");
        std::fs::write(&path, r#"{"sequence":1,"decision":"step"}"#).unwrap();

        write_control_response(&dir, &path, &json!({"sequence": 2, "decision": "continue"}))
            .unwrap();

        let response: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(response["sequence"], 2);
        assert_eq!(response["decision"], "continue");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_flutter_runner_requires_an_explicit_scaffold_refresh() {
        let dir =
            std::env::temp_dir().join(format!("reproit-flutter-runner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let runner = dir.join("runner.dart");
        std::fs::write(&runner, "Future<void> runExplorer() async {}\n").unwrap();

        let error = ensure_flutter_inspection_runner(&runner).unwrap_err();

        assert!(error.to_string().contains("older ReproIt explorer"));
        assert!(error.to_string().contains("--force"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
