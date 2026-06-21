//! MCP server (stdio, newline-delimited JSON-RPC 2.0): exposes reproit to
//! coding agents so they can loop edit -> verify -> fix against the
//! deterministic runner ("the acceptance oracle for agents").
//!
//! The agent-facing surface mirrors the new CLI (see docs/cli.md, "MCP"): the
//! deterministic core (map / fuzz / check / keep / repros / why / cloud) is
//! exposed; authoring, triage and fixing are NOT tools, the host agent does
//! those itself (no bundled LLM). `reproit_context` is the one composite tool:
//! it assembles the scoped graph + screen list + selectors the agent needs to
//! author or fix a target.
//!
//! Implementation note: each tool call re-invokes this same binary as a
//! subprocess with `--json` where available and captures its output. That keeps
//! stdout clean for the protocol and reuses every command without refactoring.

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::process::Command;

pub fn serve(config: Option<&std::path::Path>) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        // Notifications (no id) need no response.
        if id.is_none() {
            continue;
        }
        let id = id.unwrap();
        let response = match method {
            "initialize" => {
                let requested = msg
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-03-26");
                ok(
                    &id,
                    json!({
                        "protocolVersion": requested,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "reproit", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": "Deterministic E2E bug oracle. map -> fuzz -> check. \
                    Call reproit_context(target) to get the scoped graph + screens + selectors, \
                    then author or fix yourself (no bundled LLM here). reproit_fuzz finds repros; \
                    reproit_check classifies each pass/fail/flaky/stale (deterministic, so a green \
                    check means you really fixed it); reproit_keep saves a repro; reproit_why ranks \
                    suspect code. The cloud tools bridge production telemetry into the same loop."
                    }),
                )
            }
            "ping" => ok(&id, json!({})),
            "tools/list" => ok(&id, json!({ "tools": tool_defs() })),
            "tools/call" => {
                let name = msg
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let args = msg
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                let (text, is_error) = call_tool(config, name, &args);
                ok(
                    &id,
                    json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": is_error
                    }),
                )
            }
            _ => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("method not found: {method}") }
            }),
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }
    Ok(())
}

fn ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// The agent-facing tool list. Names + args track docs/cli.md ("MCP tools").
fn tool_defs() -> Value {
    json!([
        {
            "name": "reproit_context",
            "description": "The authoring/fixing entry point. Returns the scoped state graph plus the screen list and the addressable elements/selectors for a target, so the agent can emit actions by stable key (locale-invariant) without re-deriving structure. Built from the app map (`map show`); if no map exists, it reports that and points at reproit_map. `target` is an alias or node id to scope to (e.g. \"login\").",
            "inputSchema": { "type": "object", "properties": {
                "target": { "type": "string", "description": "Alias or node id to scope the graph + selectors to (e.g. \"login\"). Omit for the whole graph." }
            } }
        },
        {
            "name": "reproit_map",
            "description": "Build/refresh the app state graph (structural, locale-invariant): explores the app and records screens by signature plus the actions between them. Re-run when the app changes. `show=true` renders the existing graph + node aliases instead of rebuilding (fast, no run). A full build is slow (a real run).",
            "inputSchema": { "type": "object", "properties": {
                "show": { "type": "boolean", "description": "Render the existing graph (screens + aliases) instead of rebuilding. Fast." }
            } }
        },
        {
            "name": "reproit_accessibility",
            "description": "The accessibility audit. Returns reproit's UI-graph-vs-accessibility-graph diff per screen: the ground-truth-operable controls (what a pointer user can actually operate) that the accessibility/keyboard graph is MISSING. Each gap is GROUNDED and deterministic, not a lint guess: it carries the failing element's stable selector, which WCAG dimension(s) it fails -- pointer_only (2.1.1: operable by mouse, not keyboard), no_role (4.1.2: operable, no programmatic role/name), keyboard_unreachable (not in the Tab order), or a screen-level focus_trap -- AND a static source location (file:line) to fix it. Each screen also carries its route and a repro action-path to reach it. This closes the loop: locate the gap by file:line, fix the control, then reproit_check to deterministically confirm the gap closed. Read from the map's operability gaps, so run reproit_map first. `state` scopes to one screen (signature or name); `kind` filters to one dimension.",
            "inputSchema": { "type": "object", "properties": {
                "state": { "type": "string", "description": "Scope to one screen, by signature id or human name. Omit for all screens." },
                "kind": { "type": "string", "description": "Filter to one dimension: pointer_only | keyboard_unreachable | no_role | focus_trap." }
            } }
        },
        {
            "name": "reproit_coverage",
            "description": "Derive the CANDIDATE (hypothesized) map of every screen the app SHOULD have by reading its source with the LLM (no simulator), reconcile it against the verified map, and return the coverage ledger plus the pending WORKLIST: which screens aren't reached yet and why (needs_data | needs_peer | needs_login | frontier). Use it to know exactly which journeys to author next to close coverage, and which need seeding or a dual-user scenario. The candidate map is a worklist, never ground truth: only a driven run (reproit_check on a journey) promotes a screen to verified. Slow: an LLM pass over source.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "reproit_fuzz",
            "description": "The bug-FINDING step. Hunts over the existing map (run reproit_map first) and returns a DEDUPED unique-bugs work-list: findings are collected across the seed budget and grouped by crash signature, so the same bug reached by different paths is reported ONCE, with a canonical (shortest) repro id per bug. Pass an id to reproit_check (confirm it reproduces, before saving) or reproit_keep (save it as a guard), then reproit_simplify to clean the repro. All oracles on by default (crash/jank/leak/visual/divergence/a11y/i18n). `target` concentrates the hunt on an alias/node; `platform` selects ios|android|web|all (multi -> run all + diff for divergence). Slow: real runs.",
            "inputSchema": { "type": "object", "properties": {
                "target": { "type": "string", "description": "Alias/node to concentrate the hunt on (e.g. \"login\")." },
                "platform": { "type": "string", "description": "ios|android|web|all (comma list -> run all + divergence diff)." }
            } }
        },
        {
            "name": "reproit_check",
            "description": "Run a repro and classify it: pass / fail (regression, exit 1) / flaky (app race, exit 2) / stale (UI changed, couldn't replay, exit 3). `repro` is a saved repro (id/alias) OR a pending fuzz finding id from reproit_fuzz, so you can confirm a finding reproduces BEFORE reproit_keep. With no `repro`, runs the whole committed suite and reports the worst. Deterministic, so a green check means the bug is really fixed. `record=true` produces an annotated video (taps, seed, crash moment).",
            "inputSchema": { "type": "object", "properties": {
                "repro": { "type": "string", "description": "Saved repro id/alias, or a pending finding id from reproit_fuzz. Omit to run the whole saved suite." },
                "record": { "type": "boolean", "description": "Run once with full evidence capture + annotated video." }
            } }
        },
        {
            "name": "reproit_keep",
            "description": "Save a repro from the latest fuzz run into the committed suite (.reproit/repros/<content-hash>/), stable across machines and self-deduping. `id` is the finding id from reproit_fuzz (uses the sole finding if omitted). `as` assigns a human alias used by reproit_check. A kept repro lands quarantined and auto-promotes to required on its first green.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string", "description": "Finding id (dirname) from the latest fuzz run. Omit to use the sole finding." },
                "as": { "type": "string", "description": "Human alias for the kept repro (used in reproit_check)." }
            } }
        },
        {
            "name": "reproit_repros",
            "description": "List the saved repros under .reproit/repros/ with each one's last status, plus each repro's action sequence (so you can see what to simplify with reproit_simplify).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "reproit_simplify",
            "description": "Replace a repro with a SHORTER, cleaner action sequence YOU propose, but only if reproit can deterministically verify it still reproduces the same finding. Use it to clean up a tangled fuzz-found repro (e.g. one that ends on a positional `role:button#4` selector or post-crash UI). Read the repro's actions (reproit_repros), propose a minimal equivalent using KEYED selectors (`tap:key:...`), and reproit verify-and-adopts it, or rejects it if it doesn't reproduce or isn't shorter. The engine verifies, so your simplification can never be wrong (you propose, reproit disposes). Slow: it replays the candidate.",
            "inputSchema": { "type": "object", "properties": {
                "repro": { "type": "string", "description": "Repro id/alias (or a pending finding id) to simplify." },
                "actions": { "type": "array", "items": { "type": "string" }, "description": "Candidate action sequence, e.g. [\"tap:key:testid:add\",\"tap:key:testid:open-cart\",\"tap:key:testid:remove\"]." }
            }, "required": ["repro", "actions"] }
        },
        {
            "name": "reproit_why",
            "description": "Rank suspect code for a failure: spectrum-based fault localization (Ochiai) contrasting coverage of passing vs failing runs. Needs both (reproit_fuzz produces them) and coverage snapshots. Feeds the agent evidence for where to fix. `repro` scopes to one repro's coverage when given.",
            "inputSchema": { "type": "object", "properties": {
                "repro": { "type": "string", "description": "Repro/alias to scope fault localization to. Omit for all runs under .reproit/runs." }
            } }
        },
        {
            "name": "reproit_journeys",
            "description": "List the saved scripted journeys (declarative YAML paths through the app) with each one's step count and auth setup. The authoring complement to reproit_repros: journeys are hand/agent-authored repros with assertions. Run one with reproit_check <name>.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "reproit_journey_save",
            "description": "Author a scripted journey: write journeys/<name>.yaml from a structured spec, then confirm it with reproit_check <name> (pass/fail/flaky/stale, deterministic). GROUND IT FIRST with reproit_context. Address elements by visible name with tap:label:<text> (works on uninstrumented apps, like Playwright/Appium), or by tap:key:<id> which is the durable upgrade: prefer keys for committed journeys you'll re-run across locales or when a label is ambiguous (two \"OK\" buttons). A journey is a list of `steps`, each EXACTLY ONE of: {\"do\":\"tap:key:testid:add\"} or {\"do\":\"tap:label:Send Code\"} explicit action (or \"back\"); {\"goto\":\"<screen>\"} pathfind the map to a screen; {\"expect\":{...}} assert one of state/text/count: {\"state\":\"<screen>\"} | {\"text\":\"<visible substring>\"} | {\"count\":{\"<finder>\":N}}; {\"fill\":{\"<finder>\":\"<value>\"}} type into fields, where a value of \"secret:password\"/\"secret:username\" is injected from the auth vault (never hardcode credentials). Optional top-level \"setup\":\"login(<account>)\" (drive the login UI first) or \"auth(<account>)\" (restore a saved session, skip the UI). MULTI-USER: to test two+ logged-in users interacting (e.g. one posts, another sees it), add \"actors\" and tag every step with the actor that performs it. \"actors\" is either a bare list [\"alice\",\"bob\"] or a map binding each actor to its login {\"alice\":{\"login\":\"alice\"},\"bob\":{\"auth\":\"bob\"}} (login = drive the UI with that account's vault creds; auth = restore its session). reproit launches one device per actor, runs steps in the listed order across them (so alice's effect is observable to bob), and a `secret:` fill in a step binds to that step's actor account. Multi-actor steps support do/expect(text|count)/fill only (no goto/expect:state). A failed assertion or unreachable step reports STALE, not pass.",
            "inputSchema": { "type": "object", "properties": {
                "name": { "type": "string", "description": "Journey name (the file stem under journeys/)." },
                "journey": { "type": "object", "description": "The spec. Single-user: {\"setup\"?: \"login(guest)\", \"steps\": [ {\"do\":...} | {\"goto\":...} | {\"expect\":...} | {\"fill\":...} ]}. Multi-user: {\"actors\": {\"alice\":{\"login\":\"alice\"}, \"bob\":{\"auth\":\"bob\"}}, \"steps\": [ {\"actor\":\"alice\", \"do\":...}, {\"actor\":\"bob\", \"expect\":{\"text\":...}} ]}." }
            }, "required": ["name", "journey"] }
        },
        {
            "name": "reproit_cloud_buckets",
            "description": "List grouped finding buckets + counts from the cloud (fuzz + production telemetry). `app` is the cloud app id (defaults to $REPROIT_CLOUD_APP); `query` filters buckets by message substring.",
            "inputSchema": { "type": "object", "properties": {
                "app": { "type": "string", "description": "Cloud app id (default: $REPROIT_CLOUD_APP)." },
                "query": { "type": "string", "description": "Filter buckets by message substring." }
            } }
        },
        {
            "name": "reproit_cloud_blast_radius",
            "description": "Who's affected by a bucket: cohorts, percentages, versions. `bucket` is the bucket index (or signature). `app` is the cloud app id (defaults to $REPROIT_CLOUD_APP).",
            "inputSchema": { "type": "object", "properties": {
                "bucket": { "type": "string", "description": "Bucket index (integer) or signature." },
                "app": { "type": "string", "description": "Cloud app id (default: $REPROIT_CLOUD_APP)." }
            }, "required": ["bucket"] }
        },
        {
            "name": "reproit_cloud_reproduce",
            "description": "Pull a real user session for a bucket and replay it locally, reporting whether it reproduced (an exception fired) or replayed clean (likely data-dependent). `bucket` is the bucket index. `app` is the cloud app id (defaults to $REPROIT_CLOUD_APP). Slow: a real run.",
            "inputSchema": { "type": "object", "properties": {
                "bucket": { "type": "string", "description": "Bucket index (integer) to reproduce." },
                "app": { "type": "string", "description": "Cloud app id (default: $REPROIT_CLOUD_APP)." }
            }, "required": ["bucket"] }
        }
    ])
}

fn call_tool(config: Option<&std::path::Path>, name: &str, args: &Value) -> (String, bool) {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(String::from);
    let b = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    // Cloud app id: explicit arg wins, else $REPROIT_CLOUD_APP.
    let cloud_app = |a: &Value| -> Option<String> {
        a.get("app")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| std::env::var("REPROIT_CLOUD_APP").ok())
    };

    // Globals go before the subcommand. `--json` gives the agent structured
    // output wherever a command supports it.
    let mut argv: Vec<String> = Vec::new();
    if let Some(cfg) = config {
        argv.push("--config".into());
        argv.push(cfg.to_string_lossy().into_owned());
    }
    argv.push("--json".into());
    // Never block on a prompt: the MCP bridge is non-interactive.
    argv.push("--yes".into());

    match name {
        "reproit_context" => {
            // The scoped graph + screen list + selectors, rendered by `map show`.
            argv.push("map".into());
            argv.push("show".into());
        }
        "reproit_map" => {
            // `map show` renders the existing graph; bare build is `map structural`.
            argv.push("map".into());
            argv.push(if b("show") { "show" } else { "structural" }.into());
        }
        "reproit_accessibility" => {
            // The UI-vs-a11y diff, read from the map's operability gaps.
            argv.push("map".into());
            argv.push("accessibility".into());
            if let Some(st) = s("state") {
                argv.extend(["--state".into(), st]);
            }
            if let Some(k) = s("kind") {
                argv.extend(["--kind".into(), k]);
            }
        }
        "reproit_coverage" => {
            argv.push("map".into());
            argv.push("semantic".into());
        }
        "reproit_fuzz" => {
            argv.push("fuzz".into());
            // The agent wants the whole deduped work-list: collect findings
            // across the seed budget and group them into unique bugs (same bug
            // reached by different paths counts once).
            argv.push("--all".into());
            // Minimize each repro (ddmin) so the agent gets the SHORTEST
            // reproducing action sequence, not the raw exploration walk (a
            // 19-action path shrinks to the 2-action one that actually matters).
            argv.push("--shrink".into());
            if let Some(t) = s("target") {
                argv.extend(["--journey".into(), t]);
            }
            if let Some(p) = s("platform") {
                argv.extend(["--target".into(), p]);
            }
        }
        "reproit_check" => {
            argv.push("check".into());
            if let Some(r) = s("repro") {
                argv.push(r);
            }
            if b("record") {
                argv.push("--record".into());
            }
        }
        "reproit_keep" => {
            argv.push("keep".into());
            if let Some(id) = s("id") {
                argv.push(id);
            }
            if let Some(alias) = s("as") {
                argv.extend(["--as".into(), alias]);
            }
        }
        "reproit_simplify" => {
            argv.push("simplify".into());
            let Some(r) = s("repro") else {
                return (missing("repro"), true);
            };
            argv.push(r);
            let Some(actions) = args.get("actions").filter(|v| v.is_array()) else {
                return (missing("actions (a JSON array of action strings)"), true);
            };
            argv.extend(["--to".into(), actions.to_string()]);
        }
        "reproit_repros" => argv.push("repros".into()),
        "reproit_journeys" => argv.extend(["journey".into(), "list".into()]),
        "reproit_journey_save" => {
            let Some(name) = s("name") else {
                return (missing("name"), true);
            };
            let Some(spec) = args.get("journey").filter(|v| v.is_object()) else {
                return (
                    missing("journey (a JSON object with a `steps` array)"),
                    true,
                );
            };
            argv.extend([
                "journey".into(),
                "save".into(),
                name,
                "--spec".into(),
                spec.to_string(),
            ]);
        }
        "reproit_why" => {
            // `why` localizes over coverage under a dir; a repro scopes to its
            // own run dir so the contrast is that repro's passing vs failing.
            argv.push("why".into());
            if let Some(r) = s("repro") {
                argv.extend(["--dir".into(), format!(".reproit/repros/{r}")]);
            }
        }
        "reproit_cloud_buckets" => {
            let Some(app) = cloud_app(args) else {
                return (missing_app(), true);
            };
            argv.extend(["cloud".into(), "findings".into(), "--app".into(), app]);
            if let Some(q) = s("query") {
                argv.extend(["--query".into(), q]);
            }
        }
        "reproit_cloud_blast_radius" => {
            let Some(app) = cloud_app(args) else {
                return (missing_app(), true);
            };
            let Some(bucket) = s("bucket") else {
                return (missing("bucket"), true);
            };
            argv.extend(["cloud".into(), "blast-radius".into(), "--app".into(), app]);
            // A bucket is an index (integer) by default, or a signature.
            if bucket.chars().all(|c| c.is_ascii_digit()) {
                argv.extend(["--idx".into(), bucket]);
            } else {
                argv.extend(["--sig".into(), bucket]);
            }
        }
        "reproit_cloud_reproduce" => {
            let Some(app) = cloud_app(args) else {
                return (missing_app(), true);
            };
            let Some(bucket) = s("bucket") else {
                return (missing("bucket"), true);
            };
            argv.extend([
                "cloud".into(),
                "reproduce".into(),
                "--app".into(),
                app,
                "--idx".into(),
                bucket,
                "--run".into(),
            ]);
        }
        other => return (format!("unknown tool: {other}"), true),
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return (format!("cannot locate reproit binary: {e}"), true),
    };
    match Command::new(exe).args(&argv).output() {
        Ok(out) => {
            // `check` is a VERDICT command: fail(1)/flaky(2)/stale(3) are the CI
            // exit contract, not tool failures. A check that produced a verdict
            // is a SUCCESSFUL tool call, the agent must SEE the verdict (e.g.
            // confirm a bug reproduces = a FAIL) rather than a tool error. Detect
            // the verdict from the --json `outcome` field; only a check that
            // produced NO verdict (bad config, repro not found) is a real error.
            let is_error = if name == "reproit_check" {
                !json_has_field(&out.stdout, "outcome")
            } else {
                !out.status.success()
            };
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            // For a check that produced a verdict, prepend a one-line actionable
            // gloss so the agent reads the outcome unambiguously without parsing
            // the enum: PASS / FAIL (reproduced) / FLAKY (race) / STALE (could not
            // run, re-record). Without this the four outcomes are just bare
            // strings in the JSON and a STALE can read like a soft pass.
            if name == "reproit_check" && !is_error {
                if let Some(g) = check_gloss(&out.stdout) {
                    text = format!("{g}\n{text}");
                }
            }
            let err = String::from_utf8_lossy(&out.stderr);
            // Surface the human progress log (stderr) to the agent ONLY when it
            // adds signal: the call errored, or stdout carried no structured
            // output (e.g. `fuzz`, whose findings print to stderr). On a
            // successful --json call the progress chatter is just noise and can
            // mislead: a `check` whose verdict is `fail` still prints per-run
            // "PASS" drive lines to stderr, which read as a contradiction.
            if (is_error || text.trim().is_empty()) && !err.trim().is_empty() {
                if !text.trim().is_empty() {
                    text.push('\n');
                }
                text.push_str("--- stderr ---\n");
                text.push_str(err.trim());
            }
            (text, is_error)
        }
        Err(e) => (format!("failed to spawn reproit: {e}"), true),
    }
}

/// A one-line, actionable gloss for a `check` verdict, derived from its --json
/// `outcome`. Keeps the four outcomes legible and DISTINCT for the agent: a STALE
/// is "could not run, re-record", never a soft pass; a FAIL is a confirmed
/// reproduction. Returns None when the output carries no outcome (a real error
/// the caller already surfaces) or an unknown outcome string.
fn check_gloss(stdout: &[u8]) -> Option<String> {
    let v = serde_json::from_slice::<serde_json::Value>(stdout).ok()?;
    let outcome = v.get("outcome").and_then(Value::as_str)?;
    let msg = match outcome {
        "pass" => "PASS: replayed clean and reached the trigger context -- the bug is fixed (deterministic, so this is a real green).",
        "fail" => "FAIL: the finding REPRODUCED (a confirmed regression). If you were confirming a fuzz finding, this is the expected signal to keep it; if you were verifying a fix, the fix did not hold.",
        "flaky" => "FLAKY: the finding reproduced on SOME replays but not all (a non-deterministic app race) -- not a clean reproduction and not a clean fix.",
        "stale" => "STALE: could not run the case to its trigger -- the UI path to the bug moved or the runner could not replay it. This is NOT a pass or a verdict on the bug: rebuild the map (reproit_map) and re-record/retry; the change may also have fixed it.",
        _ => return None,
    };
    Some(msg.to_string())
}

/// True if the command's stdout is a JSON object carrying `field` (used to tell
/// "the command produced a structured result" from "the command failed before
/// emitting one").
fn json_has_field(stdout: &[u8], field: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(stdout)
        .ok()
        .and_then(|v| v.get(field).cloned())
        .is_some()
}

fn missing(what: &str) -> String {
    format!("missing required argument: {what}")
}

fn missing_app() -> String {
    "missing cloud app id: pass `app` or set $REPROIT_CLOUD_APP".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_has_field_distinguishes_a_verdict_from_a_failure() {
        // A check that produced a verdict carries `outcome` -> NOT a tool error,
        // even when the CLI exited non-zero (fail/flaky/stale are verdicts).
        assert!(json_has_field(
            br#"{"command":"check","outcome":"fail"}"#,
            "outcome"
        ));
        assert!(json_has_field(br#"{"outcome":"stale"}"#, "outcome"));
        // No outcome (a real failure: bad config, repro not found, or non-JSON
        // error text) -> a tool error.
        assert!(!json_has_field(br#"{"command":"check"}"#, "outcome"));
        assert!(!json_has_field(b"Error: no repro `x`", "outcome"));
        assert!(!json_has_field(b"", "outcome"));
    }

    #[test]
    fn check_gloss_distinguishes_all_four_outcomes() {
        // Each of the four verdicts gets a distinct, actionable leading line so an
        // agent reads the outcome without parsing the enum.
        let pass = check_gloss(br#"{"command":"check","outcome":"pass"}"#).unwrap();
        let fail = check_gloss(br#"{"outcome":"fail"}"#).unwrap();
        let flaky = check_gloss(br#"{"outcome":"flaky"}"#).unwrap();
        let stale = check_gloss(br#"{"outcome":"stale"}"#).unwrap();
        assert!(pass.starts_with("PASS"));
        assert!(fail.starts_with("FAIL"));
        assert!(flaky.starts_with("FLAKY"));
        assert!(stale.starts_with("STALE"));
        // All four are different text (no two outcomes collapse to one signal).
        let all = [&pass, &fail, &flaky, &stale];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j]);
            }
        }
    }

    #[test]
    fn check_gloss_stale_is_actionable_and_not_a_pass() {
        // The stale gloss must read as "could not run, re-record", never a soft
        // pass: it tells the agent to rebuild the map and that it is NOT a verdict.
        let stale = check_gloss(br#"{"outcome":"stale"}"#).unwrap();
        assert!(stale.contains("NOT a pass"));
        assert!(stale.contains("reproit_map"));
        // It must not contain "PASS"/"FAIL" as a leading verdict that could be
        // misread as a clean/confirmed result.
        assert!(!stale.starts_with("PASS"));
        assert!(!stale.starts_with("FAIL"));
    }

    #[test]
    fn check_gloss_absent_for_non_verdict_output() {
        // No outcome (a real error: bad config / unresolvable repro) -> no gloss,
        // so the error path (stderr surfaced as a tool error) is untouched.
        assert!(check_gloss(br#"{"command":"check"}"#).is_none());
        assert!(check_gloss(b"Error: no repro `x`").is_none());
        assert!(check_gloss(b"").is_none());
        // An unknown outcome string also yields no gloss (we never invent a label).
        assert!(check_gloss(br#"{"outcome":"weird"}"#).is_none());
    }
}
