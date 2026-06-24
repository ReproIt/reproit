//! MCP server (stdio, newline-delimited JSON-RPC 2.0): exposes reproit to
//! coding agents so they can loop edit -> verify -> fix against the
//! deterministic runner ("the acceptance oracle for agents").
//!
//! The agent-facing surface mirrors the new CLI (see docs/cli.md, "MCP"): the
//! deterministic core (map / sweep / fuzz / check / keep / record / repro /
//! cloud) is exposed; authoring, triage and fixing are NOT tools, the host does
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
                        "instructions": "Deterministic E2E bug oracle. map -> sweep -> check. \
                    Call reproit_context(target) to get the scoped graph + screens + selectors, \
                    then author or fix yourself (no bundled LLM here). reproit_sweep is the default \
                    finder (state-present bugs visible on each screen); reproit_fuzz is the deep \
                    search for sequence bugs (crash/jank/hang). reproit_check classifies each \
                    pass/fail/flaky/stale (deterministic, so a green check means you really fixed \
                    it); reproit_keep saves a repro; reproit_record clips it; reproit_why ranks \
                    suspect code. The cloud tools close the FULL production loop, so an agent can \
                    MANAGE + MONITOR bugs, not just fix them: reproit_cloud_buckets lists \
                    impact-ranked bugs -> reproit_cloud_pull the top one -> reproit_check (reproduce) \
                    -> fix -> reproit_check (verify) -> reproit_keep -> reproit_cloud_triage \
                    status=fixed --fixed-in-build <ver> to record the fix intent -> then watch \
                    reproit_cloud_resolution_events for a regression (prod contradicting the claim)."
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
            "description": "The authoring/fixing entry point. Returns the scoped state graph plus the screen list and the addressable elements/selectors for a target, so the agent can emit actions by stable key (locale-invariant) without re-deriving structure. Built from the app map (`map show`); if no map exists, it reports that and points at reproit_map. `target` is an alias or node id to scope to (e.g. \"login\"). FULL CLOUD LOOP (manage + monitor bugs, not just fix them): reproit_cloud_buckets (impact-ranked) -> reproit_cloud_pull the top -> reproit_check (reproduce) -> fix -> reproit_check (verify) -> reproit_keep -> reproit_cloud_triage status=fixed --fixed-in-build <ver> (record the fix intent) -> reproit_cloud_resolution_events to monitor for a regression (prod contradicting the fix claim).",
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
            "name": "reproit_sweep",
            "description": "The DEFAULT \"what's wrong on every screen\" finder. One coverage crawl that visits each reachable screen once and reports the STATE-PRESENT bugs simply visible on each (overflow / broken content / a11y unlabeled / choice-anomaly), one finding per (screen x issue) -- grouped by screen, nothing collapsed. Prefer this over reproit_fuzz for \"audit this app / find the visible bugs\": it is deterministic, doesn't permute action sequences, and surfaces every per-screen issue (reproit_fuzz reports one finding per seed and drops most of these). Pass a URL (zero-config, deployed app) or an alias/node to scope. Pair the findings to reproit_keep / reproit_record. Use reproit_fuzz for the DEEPER sequence-dependent bugs (crash/jank/hang). Slow: a real run.",
            "inputSchema": { "type": "object", "properties": {
                "target": { "type": "string", "description": "A URL (https://app.com, zero-config) or an alias/node to scope the crawl to." },
                "record": { "type": "boolean", "description": "Also save an annotated clip (red box on the bug) per boxable finding, into .reproit/sweep-clips/. Web only." }
            } }
        },
        {
            "name": "reproit_fuzz",
            "description": "The DEEP, sequence-dependent bug search: combinatorially permutes action sequences to provoke bugs that only appear after the right actions in the right order (crash / jank / hang / leak). Hunts over the existing map (run reproit_map first) and returns a DEDUPED unique-bugs work-list grouped by signature, with a canonical (shortest) repro id per bug. For bugs simply VISIBLE on a screen (overflow / content / a11y / choice-anomaly) prefer reproit_sweep -- it is faster and reports every per-screen issue, where fuzz collapses to one finding per seed. Pass an id to reproit_check (confirm), reproit_keep (save), then reproit_simplify to clean the repro. `target` concentrates the hunt on an alias/node; `platform` selects ios|android|web|all (multi -> run all + diff for divergence). Slow: real runs.",
            "inputSchema": { "type": "object", "properties": {
                "target": { "type": "string", "description": "Alias/node to concentrate the hunt on (e.g. \"login\")." },
                "platform": { "type": "string", "description": "ios|android|web|all (comma list -> run all + divergence diff)." }
            } }
        },
        {
            "name": "reproit_check",
            "description": "Run a repro and classify it: pass / fail (regression, exit 1) / flaky (app race, exit 2) / stale (UI changed, couldn't replay, exit 3). `repro` is a saved repro (id/alias) OR a pending fuzz finding id from reproit_fuzz, so you can confirm a finding reproduces BEFORE reproit_keep. With no `repro`, runs the whole committed suite and reports the worst. Deterministic, so a green check means the bug is really fixed. For an annotated video use reproit_record; for a baseline pixel diff use reproit_baseline.",
            "inputSchema": { "type": "object", "properties": {
                "repro": { "type": "string", "description": "Saved repro id/alias, or a pending finding id from reproit_fuzz. Omit to run the whole saved suite." }
            } }
        },
        {
            "name": "reproit_record",
            "description": "Record a repro ONCE with full evidence + an annotated video (paced action HUD + a red box scoped to the repro's oracle, marking the bug's effect). `repro` is a saved repro (id/alias) or a pending fuzz finding id. Use it to produce a shareable clip of a confirmed bug. `flicker=true` also scans the recorded video for transient render glitches (a frame that diverges then snaps back). Slow: a real run.",
            "inputSchema": { "type": "object", "properties": {
                "repro": { "type": "string", "description": "Saved repro id/alias, or a pending finding id from reproit_fuzz." },
                "flicker": { "type": "boolean", "description": "Also scan the recorded video for intra-run flicker." }
            }, "required": ["repro"] }
        },
        {
            "name": "reproit_baseline",
            "description": "The visual-regression oracle: diff the current capture against the committed baseline (per-pixel tolerance + ignore regions), driven by the `visual` section in reproit.yaml. `update=true` accepts the current capture as the new baseline (use after an intended UI change). Was `check --visual`.",
            "inputSchema": { "type": "object", "properties": {
                "update": { "type": "boolean", "description": "Accept the current capture as the new baseline." }
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
            "description": "The IMPACT-RANKED bug list and the loop's STARTING point: returns each bucket's content-addressed `bucketId`, impact score + severity, resolution status, count, and message, already sorted by impact (highest first). This is the ONLY tool that surfaces the `bucketId` -- the id reproit_cloud_pull / reproit_cloud_triage / reproit_cloud_timeline take via `bucket`. Distinct from reproit_cloud_blast_radius (the cohort who's-affected lens, which has no bucket id). `app` is the cloud app id (defaults to $REPROIT_CLOUD_APP); `query` filters buckets by message substring.",
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
        },
        {
            "name": "reproit_cloud_pull",
            "description": "Pull a production bug from the cloud as a FIRST-CLASS LOCAL repro you can then verify + fix offline. The autonomous fix loop: reproit_cloud_buckets (impact-ranked) -> reproit_cloud_pull the top bucket -> reproit_check <as> (reproduces it locally, NETWORK-FREE) -> fix the code -> reproit_check <as> again (proves the fix) -> reproit_keep. `bucket` is the content-addressed bucket id from reproit_cloud_buckets; `as` is a short local name used in reproit_check; `app` defaults to $REPROIT_CLOUD_APP.",
            "inputSchema": { "type": "object", "properties": {
                "bucket": { "type": "string", "description": "Content-addressed bucket id (from reproit_cloud_buckets)." },
                "as": { "type": "string", "description": "A short local name for the pulled repro (used as reproit_check <name>)." },
                "app": { "type": "string", "description": "Cloud app id (default: $REPROIT_CLOUD_APP)." }
            }, "required": ["bucket", "as"] }
        },
        {
            "name": "reproit_cloud_triage",
            "description": "READ or SET a bucket's triage status: the MANAGEMENT state an agent owns (where in the lifecycle, who's on it), distinct from the prod-truth resolution the system computes. With only `bucket`, READS + returns the current state. With `status`, SETS it: new | triaged | assigned | fixed | wontfix. This is how an agent RECORDS its intent in the loop: after reproit_check proves a fix holds locally, call this with status=fixed and `fixed_in_build`=<the build you shipped the fix in> to anchor the claim; production then confirms (resolved) or contradicts (regressed) it, which you read back via reproit_cloud_resolution_events. `assignee` (an org member id) is only valid with status=assigned; `fixed_in_build` only with status=fixed (and defaults server-side to the newest build seen). `bucket` is the content-addressed bucket id from reproit_cloud_buckets; `app` defaults to $REPROIT_CLOUD_APP.",
            "inputSchema": { "type": "object", "properties": {
                "bucket": { "type": "string", "description": "Content-addressed bucket id (from reproit_cloud_buckets)." },
                "status": { "type": "string", "description": "Set the status: new | triaged | assigned | fixed | wontfix. Omit to READ the current state." },
                "fixed_in_build": { "type": "string", "description": "The build the fix shipped in (the prod-resolution anchor). Only meaningful with status=fixed; defaults to the newest build seen for the bucket." },
                "assignee": { "type": "integer", "description": "Org member id to assign. Required by, and only valid for, status=assigned." },
                "app": { "type": "string", "description": "Cloud app id (default: $REPROIT_CLOUD_APP)." }
            }, "required": ["bucket"] }
        },
        {
            "name": "reproit_cloud_resolution_events",
            "description": "MONITOR the loop: list recent PROD-TRUTH transitions (resolved->regressed, resolving->resolved, ...), newest first, computed from production telemetry against each bucket's fix anchor. This is what an autonomous monitor reads to catch a REGRESSION: a bucket you marked fixed (reproit_cloud_triage status=fixed) that started firing again in production. A `regressed` event is the signal to reproit_cloud_pull it again and re-open the fix loop. `app` defaults to $REPROIT_CLOUD_APP.",
            "inputSchema": { "type": "object", "properties": {
                "app": { "type": "string", "description": "Cloud app id (default: $REPROIT_CLOUD_APP)." }
            } }
        },
        {
            "name": "reproit_cloud_timeline",
            "description": "The per-bucket OCCURRENCE time-series (segmented by build) plus the computed prod-truth resolution status. Shows whether occurrences dropped after a fix anchor (resolving/resolved) or returned (regressed). Use it to confirm a specific bucket's fix is actually holding in production, or to see the shape of a regression. `bucket` is the content-addressed bucket id; `app` defaults to $REPROIT_CLOUD_APP.",
            "inputSchema": { "type": "object", "properties": {
                "bucket": { "type": "string", "description": "Content-addressed bucket id (from reproit_cloud_buckets)." },
                "app": { "type": "string", "description": "Cloud app id (default: $REPROIT_CLOUD_APP)." }
            }, "required": ["bucket"] }
        }
    ])
}

fn call_tool(config: Option<&std::path::Path>, name: &str, args: &Value) -> (String, bool) {
    let argv = match build_argv(config, name, args) {
        Ok(argv) => argv,
        Err((msg, is_error)) => return (msg, is_error),
    };

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

/// Build the CLI argv for a tool call (the PURE half of `call_tool`): map the
/// MCP tool name + arguments onto the subcommand + flags this same binary
/// understands. No I/O, so it is unit-tested for tool-presence + dispatch. On a
/// missing required argument (or unknown tool) it returns the `(message, true)`
/// pair `call_tool` surfaces directly as a tool error.
fn build_argv(
    config: Option<&std::path::Path>,
    name: &str,
    args: &Value,
) -> Result<Vec<String>, (String, bool)> {
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
        "reproit_sweep" => {
            argv.push("sweep".into());
            if let Some(t) = s("target") {
                argv.push(t);
            }
            if b("record") {
                argv.push("--record".into());
            }
        }
        "reproit_check" => {
            argv.push("check".into());
            if let Some(r) = s("repro") {
                argv.push(r);
            }
        }
        "reproit_record" => {
            argv.push("record".into());
            let Some(r) = s("repro") else {
                return Err((missing("repro"), true));
            };
            argv.push(r);
            if b("flicker") {
                argv.push("--flicker".into());
            }
        }
        "reproit_baseline" => {
            argv.push("baseline".into());
            // No positional `repro`: the CLI `baseline` command takes only
            // `--update` (the visual config selects what is compared). Pushing a
            // repro arg made clap reject the call with "unexpected argument".
            if b("update") {
                argv.push("--update".into());
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
            argv.extend(["repro".into(), "simplify".into()]);
            let Some(r) = s("repro") else {
                return Err((missing("repro"), true));
            };
            argv.push(r);
            let Some(actions) = args.get("actions").filter(|v| v.is_array()) else {
                return Err((missing("actions (a JSON array of action strings)"), true));
            };
            argv.extend(["--to".into(), actions.to_string()]);
        }
        "reproit_repros" => argv.push("repros".into()),
        "reproit_journeys" => argv.extend(["journey".into(), "list".into()]),
        "reproit_journey_save" => {
            let Some(name) = s("name") else {
                return Err((missing("name"), true));
            };
            let Some(spec) = args.get("journey").filter(|v| v.is_object()) else {
                return Err((
                    missing("journey (a JSON object with a `steps` array)"),
                    true,
                ));
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
            // `repro why` localizes over coverage under a dir; a repro scopes to
            // its own run dir so the contrast is that repro's passing vs failing.
            argv.extend(["repro".into(), "why".into()]);
            if let Some(r) = s("repro") {
                argv.extend(["--dir".into(), format!(".reproit/repros/{r}")]);
            }
        }
        "reproit_cloud_buckets" => {
            let Some(app) = cloud_app(args) else {
                return Err((missing_app(), true));
            };
            // The impact-ranked list at GET /v1/apps/:app/buckets -- the ONLY
            // command that surfaces the `bucketId` the rest of the loop keys off.
            argv.extend(["cloud".into(), "buckets".into(), "--app".into(), app]);
            if let Some(q) = s("query") {
                argv.extend(["--query".into(), q]);
            }
        }
        "reproit_cloud_blast_radius" => {
            let Some(app) = cloud_app(args) else {
                return Err((missing_app(), true));
            };
            let Some(bucket) = s("bucket") else {
                return Err((missing("bucket"), true));
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
                return Err((missing_app(), true));
            };
            let Some(bucket) = s("bucket") else {
                return Err((missing("bucket"), true));
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
        "reproit_cloud_pull" => {
            let Some(app) = cloud_app(args) else {
                return Err((missing_app(), true));
            };
            let Some(bucket) = s("bucket") else {
                return Err((missing("bucket"), true));
            };
            let Some(as_name) = s("as") else {
                return Err((missing("as"), true));
            };
            argv.extend([
                "cloud".into(),
                "pull".into(),
                "--app".into(),
                app,
                "--bucket".into(),
                bucket,
                "--as".into(),
                as_name,
            ]);
        }
        "reproit_cloud_triage" => {
            let Some(app) = cloud_app(args) else {
                return Err((missing_app(), true));
            };
            let Some(bucket) = s("bucket") else {
                return Err((missing("bucket"), true));
            };
            argv.extend([
                "cloud".into(),
                "triage".into(),
                "--app".into(),
                app,
                "--bucket".into(),
                bucket,
            ]);
            // No status => READ the current triage state. With a status => SET it,
            // forwarding the optional fix anchor / assignee (the cloud enforces
            // which status each is valid for).
            if let Some(status) = s("status") {
                argv.extend(["--status".into(), status]);
            }
            if let Some(fib) = s("fixed_in_build") {
                argv.extend(["--fixed-in-build".into(), fib]);
            }
            if let Some(a) = args.get("assignee").and_then(Value::as_i64) {
                argv.extend(["--assignee".into(), a.to_string()]);
            }
        }
        "reproit_cloud_resolution_events" => {
            let Some(app) = cloud_app(args) else {
                return Err((missing_app(), true));
            };
            argv.extend([
                "cloud".into(),
                "resolution-events".into(),
                "--app".into(),
                app,
            ]);
        }
        "reproit_cloud_timeline" => {
            let Some(app) = cloud_app(args) else {
                return Err((missing_app(), true));
            };
            let Some(bucket) = s("bucket") else {
                return Err((missing("bucket"), true));
            };
            argv.extend([
                "cloud".into(),
                "timeline".into(),
                "--app".into(),
                app,
                "--bucket".into(),
                bucket,
            ]);
        }
        other => return Err((format!("unknown tool: {other}"), true)),
    }
    Ok(argv)
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

    /// The set of tool names `tool_defs()` advertises.
    fn tool_names() -> Vec<String> {
        tool_defs()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    /// The argv `build_argv` produces for a tool call (panicking the test on the
    /// error path so a dispatch test reads cleanly).
    fn argv(name: &str, args: Value) -> Vec<String> {
        build_argv(None, name, &args).expect("build_argv should not error for a valid call")
    }

    #[test]
    fn the_full_cloud_loop_tools_are_present() {
        // Every tool the manage+monitor loop needs is advertised: pull (already
        // wired) plus the new triage (set fixed), resolution-events (monitor
        // regressions), and timeline.
        let names = tool_names();
        for want in [
            "reproit_cloud_buckets",
            "reproit_cloud_pull",
            "reproit_cloud_triage",
            "reproit_cloud_resolution_events",
            "reproit_cloud_timeline",
        ] {
            assert!(names.contains(&want.to_string()), "missing tool {want}");
        }
    }

    #[test]
    fn cloud_triage_read_dispatches_without_status() {
        // No status => READ: the CLI gets `cloud triage --app A --bucket B` with no
        // --status, and the bridge's --json / --yes globals are present.
        let argv = argv(
            "reproit_cloud_triage",
            json!({ "app": "demo", "bucket": "b00b" }),
        );
        assert!(argv.contains(&"--json".to_string()));
        assert!(argv.contains(&"--yes".to_string()));
        assert!(argv.windows(2).any(|w| w == ["cloud", "triage"]));
        assert!(argv.windows(2).any(|w| w == ["--app", "demo"]));
        assert!(argv.windows(2).any(|w| w == ["--bucket", "b00b"]));
        assert!(!argv.iter().any(|a| a == "--status"));
    }

    #[test]
    fn cloud_triage_set_fixed_forwards_status_and_anchor() {
        // status=fixed + fixed_in_build => SET: forwards --status and
        // --fixed-in-build (the prod-resolution anchor) to the CLI.
        let argv = argv(
            "reproit_cloud_triage",
            json!({ "app": "demo", "bucket": "b00b", "status": "fixed", "fixed_in_build": "1.4.2" }),
        );
        assert!(argv.windows(2).any(|w| w == ["--status", "fixed"]));
        assert!(argv.windows(2).any(|w| w == ["--fixed-in-build", "1.4.2"]));
    }

    #[test]
    fn cloud_triage_assigned_forwards_assignee() {
        // An integer assignee is forwarded as a string arg (clap parses it back).
        let argv = argv(
            "reproit_cloud_triage",
            json!({ "app": "demo", "bucket": "b00b", "status": "assigned", "assignee": 42 }),
        );
        assert!(argv.windows(2).any(|w| w == ["--status", "assigned"]));
        assert!(argv.windows(2).any(|w| w == ["--assignee", "42"]));
    }

    #[test]
    fn cloud_triage_requires_app_and_bucket() {
        // Missing the bucket is a tool error (app supplied so we isolate bucket).
        let err = build_argv(None, "reproit_cloud_triage", &json!({ "app": "demo" }))
            .expect_err("missing bucket should error");
        assert!(err.1);
        assert!(err.0.contains("bucket"));
    }

    #[test]
    fn cloud_buckets_dispatches_to_cloud_buckets_not_findings() {
        // The loop-breaker fix: reproit_cloud_buckets must hit the impact-ranked
        // `cloud buckets` (GET /v1/apps/:app/buckets, surfaces the bucketId), NOT
        // `cloud findings` (the cohort lens, which has no bucket id).
        let argv = argv("reproit_cloud_buckets", json!({ "app": "demo" }));
        assert!(
            argv.windows(2).any(|w| w == ["cloud", "buckets"]),
            "expected `cloud buckets`, got {argv:?}"
        );
        assert!(
            !argv.windows(2).any(|w| w == ["cloud", "findings"]),
            "must NOT dispatch to `cloud findings`"
        );
        assert!(argv.windows(2).any(|w| w == ["--app", "demo"]));
    }

    #[test]
    fn cloud_buckets_forwards_query_filter() {
        let argv = argv(
            "reproit_cloud_buckets",
            json!({ "app": "demo", "query": "checkout" }),
        );
        assert!(argv.windows(2).any(|w| w == ["cloud", "buckets"]));
        assert!(argv.windows(2).any(|w| w == ["--query", "checkout"]));
    }

    #[test]
    fn cloud_resolution_events_dispatches() {
        let argv = argv("reproit_cloud_resolution_events", json!({ "app": "demo" }));
        assert!(argv.windows(2).any(|w| w == ["cloud", "resolution-events"]));
        assert!(argv.windows(2).any(|w| w == ["--app", "demo"]));
    }

    #[test]
    fn cloud_timeline_dispatches_with_bucket() {
        let argv = argv(
            "reproit_cloud_timeline",
            json!({ "app": "demo", "bucket": "b00b" }),
        );
        assert!(argv.windows(2).any(|w| w == ["cloud", "timeline"]));
        assert!(argv.windows(2).any(|w| w == ["--app", "demo"]));
        assert!(argv.windows(2).any(|w| w == ["--bucket", "b00b"]));
    }

    #[test]
    fn unknown_tool_is_an_error() {
        let err = build_argv(None, "reproit_nonexistent", &json!({}))
            .expect_err("an unknown tool should error");
        assert!(err.1);
        assert!(err.0.contains("unknown tool"));
    }

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

    #[test]
    fn sweep_record_baseline_tools_are_present() {
        // The redesigned find/evidence surface is advertised.
        let names = tool_names();
        for want in ["reproit_sweep", "reproit_record", "reproit_baseline"] {
            assert!(names.contains(&want.to_string()), "missing tool {want}");
        }
    }

    #[test]
    fn sweep_dispatches_with_optional_target() {
        // Bare sweep -> just the verb (+ the --json global).
        let bare = argv("reproit_sweep", json!({}));
        assert_eq!(bare.last().unwrap(), "sweep");
        assert!(bare.contains(&"--json".to_string()));
        // A target (URL or alias) is forwarded positionally.
        let scoped = argv("reproit_sweep", json!({ "target": "https://app.com" }));
        assert!(scoped.windows(2).any(|w| w == ["sweep", "https://app.com"]));
    }

    #[test]
    fn check_no_longer_carries_record() {
        // record is its own verb now: a plain check never forwards --record.
        let a = argv("reproit_check", json!({ "repro": "cart-1" }));
        assert!(a.windows(2).any(|w| w == ["check", "cart-1"]));
        assert!(!a.iter().any(|x| x == "--record"));
    }

    #[test]
    fn record_dispatches_and_requires_repro() {
        let a = argv(
            "reproit_record",
            json!({ "repro": "cart-1", "flicker": true }),
        );
        assert!(a.windows(2).any(|w| w == ["record", "cart-1"]));
        assert!(a.contains(&"--flicker".to_string()));
        // repro is required.
        let err =
            build_argv(None, "reproit_record", &json!({})).expect_err("missing repro should error");
        assert!(err.1 && err.0.contains("repro"));
    }

    #[test]
    fn baseline_dispatches_with_update() {
        let a = argv("reproit_baseline", json!({ "update": true }));
        assert!(a.contains(&"baseline".to_string()));
        assert!(a.contains(&"--update".to_string()));
    }

    #[test]
    fn simplify_and_why_use_the_repro_group() {
        // The advanced repro ops live under the `repro` subcommand now.
        let s = argv(
            "reproit_simplify",
            json!({ "repro": "cart-1", "actions": ["tap:key:testid:add"] }),
        );
        assert!(s.windows(2).any(|w| w == ["repro", "simplify"]));
        let w = argv("reproit_why", json!({ "repro": "cart-1" }));
        assert!(w.windows(2).any(|x| x == ["repro", "why"]));
    }
}
