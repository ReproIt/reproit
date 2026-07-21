//! MCP server (stdio, newline-delimited JSON-RPC 2.0): exposes reproit to
//! coding agents so they can loop edit -> verify -> fix against the
//! deterministic runner ("the acceptance oracle for agents").
//!
//! The agent-facing surface mirrors the new CLI (see docs/cli.md, "MCP"): the
//! deterministic core (map / scan / fuzz / check / keep / repro /
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

mod dispatch;
use dispatch::build_argv;

const MCP_INSTRUCTIONS: &str = concat!(
    "Deterministic E2E bug oracle. scan -> fuzz -> check -> keep. Call ",
    "reproit_context(target) to get the scoped graph + screens + selectors, then author or fix ",
    "yourself (no bundled LLM here). reproit_scan is the default finder (state-present bugs ",
    "visible on each screen); reproit_fuzz is the deep search for sequence bugs ",
    "(crash/jank/hang). reproit_check classifies each pass/fail/flaky/stale (deterministic, so a ",
    "green check means you really fixed it); reproit_keep saves a repro; reproit_check with ",
    "record_video clips it; reproit_why ranks suspect code. The cloud tools close the FULL ",
    "production loop, so an ",
    "agent can MANAGE + MONITOR bugs, not just fix them: reproit_cloud_buckets lists ",
    "impact-ranked bugs -> reproit_cloud_pull the top one -> reproit_check (reproduce) -> fix ",
    "-> reproit_check (verify) -> reproit_keep -> reproit_cloud_triage status=fixed ",
    "--fixed-in-build <ver> to record the fix intent -> then watch ",
    "reproit_cloud_resolution_events for a regression (prod contradicting the claim)."
);

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
                        "instructions": MCP_INSTRUCTIONS
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
const CONTEXT_DESCRIPTION: &str = concat!(
    "The authoring/fixing entry point. Returns the current scoped state graph plus the ",
    "screen list and addressable structural selectors for a target. Reproit refreshes its ",
    "internal app model automatically when source, configuration, build inputs, or the CLI ",
    "version change. `target` is an alias or node id to scope to (e.g. \"login\"). FULL CLOUD ",
    "LOOP (manage + monitor bugs, not just fix them): reproit_cloud_buckets (impact-ranked) -> ",
    "reproit_cloud_pull the top -> reproit_check (reproduce) -> fix -> reproit_check (verify) ",
    "-> reproit_keep -> reproit_cloud_triage status=fixed --fixed-in-build <ver> (record the ",
    "fix intent) -> reproit_cloud_resolution_events to monitor for a regression (prod ",
    "contradicting the fix claim)."
);

const ACCESSIBILITY_DESCRIPTION: &str = concat!(
    "The accessibility audit. Returns reproit's UI-graph-vs-accessibility-graph diff per ",
    "screen: the ground-truth-operable controls that the accessibility/keyboard graph is ",
    "missing. The internal model is refreshed automatically. Each gap carries a stable ",
    "selector, WCAG dimension, source file, and line. `state` scopes to one screen; `kind` ",
    "filters to one dimension."
);

const COVERAGE_DESCRIPTION: &str = concat!(
    "Derive the CANDIDATE (hypothesized) map of every screen the app SHOULD have by reading ",
    "its source with the LLM (no simulator), reconcile it against the verified map, and ",
    "return the coverage ledger plus the pending WORKLIST: which screens aren't reached yet ",
    "and why (needs_data | needs_peer | needs_login | frontier). Use it to know exactly which ",
    "journeys to author next to close coverage, and which need seeding or a dual-user ",
    "scenario. The candidate map is a worklist, never ground truth: only a driven run ",
    "(reproit_check on a journey) promotes a screen to verified. Slow: an LLM pass over ",
    "source."
);

const SCAN_DESCRIPTION: &str = concat!(
    "One coverage crawl that visits each reachable screen once. Default confirmed results ",
    "require an application-owned structural contract. Built-in content, layout, and routing ",
    "observations are specialist oracles available explicitly through reproit_fuzz --only ",
    "<oracle>; repeatability alone does not prove application intent. Pass a URL (zero-config, ",
    "deployed app) or an alias/node to scope. Set `only=route-access` to evaluate the exact ",
    "authored browser route matrix instead of crawling. `record_video=true` saves quick clips ",
    "for confirmed findings into .reproit/recordings/scan/. Use reproit_fuzz for deeper ",
    "sequence-dependent bugs, then reproit_check / reproit_keep on the fnd_... id. Slow: a real ",
    "run."
);

const FUZZ_DESCRIPTION: &str = concat!(
    "The DEEP, sequence-dependent bug search: combinatorially permutes action sequences to ",
    "provoke bugs that only appear after the right actions in the right order (crash / jank / ",
    "hang / leak). Returns a deduped work-list with a shortest fnd_... repro id per bug. ",
    "Reproit maintains its internal app model automatically. For bugs simply visible on a ",
    "screen prefer reproit_scan. Pass an id to reproit_check, then reproit_keep. Set ",
    "record_video=true on reproit_check for video evidence. `target` concentrates the hunt on ",
    "an alias/node; `platform` selects ",
    "ios|android|web|all. Slow: real runs."
);

const CHECK_DESCRIPTION: &str = concat!(
    "Run a repro and classify it: pass / fail (regression, exit 1) / flaky (app race, exit 2) / ",
    "stale (UI changed, couldn't replay, exit 3). `repro` is a saved repro (id/alias) OR a ",
    "pending fuzz finding id from reproit_fuzz, so you can confirm a finding reproduces BEFORE ",
    "reproit_keep. With no `repro`, runs the whole committed suite and reports the worst. ",
    "Set `changed` to a git base (for example HEAD^) to run mapped affected repros first, then ",
    "the rest of the full suite. It changes ordering only and never skips coverage. ",
    "Deterministic, so a green check means the bug is really fixed. `record_video=true` adds ",
    "annotated video evidence; `flicker=true` also checks that video for transient glitches. ",
    "For a baseline pixel diff use reproit_baseline."
);

const BASELINE_DESCRIPTION: &str = concat!(
    "The visual-regression oracle: diff the current capture against the committed baseline ",
    "(per-pixel tolerance + ignore regions), driven by the `visual` section in reproit.yaml. ",
    "`update=true` accepts the current capture as the new baseline after an intended UI ",
    "change."
);

const KEEP_DESCRIPTION: &str = concat!(
    "Save a repro from the latest fuzz run into the committed suite ",
    "(.reproit/repros/<content-hash>/), stable across machines and self-deduping. `id` is the ",
    "finding id from reproit_fuzz (uses the sole finding if omitted). `as` assigns a human ",
    "alias used by reproit_check. A kept repro lands quarantined and auto-promotes to required ",
    "on its first green."
);

const REPROS_DESCRIPTION: &str = concat!(
    "List the saved repros under .reproit/repros/ with each one's last status, plus each ",
    "repro's action sequence (so you can see what to simplify with reproit_simplify)."
);

const SIMPLIFY_DESCRIPTION: &str = concat!(
    "Replace a repro with a SHORTER, cleaner action sequence YOU propose, but only if reproit ",
    "can deterministically verify it still reproduces the same finding. Use it to clean up a ",
    "tangled fuzz-found repro (e.g. one that ends on a positional `role:button#4` selector or ",
    "post-crash UI). Read the repro's actions (reproit_repros), propose a minimal equivalent ",
    "using KEYED selectors (`tap:key:...`), and reproit checks equivalence before adopting it, \
     or rejects it if ",
    "it doesn't reproduce or isn't shorter. The engine verifies, so your simplification can ",
    "never be wrong (you propose, reproit disposes). Slow: it replays the candidate."
);

const WHY_DESCRIPTION: &str = concat!(
    "Rank suspect code for a failure: spectrum-based fault localization (Ochiai) contrasting ",
    "coverage of passing vs failing runs. Needs both (reproit_fuzz produces them) and coverage ",
    "snapshots. Feeds the agent evidence for where to fix. `repro` scopes to one repro's ",
    "coverage when given."
);

const JOURNEYS_DESCRIPTION: &str = concat!(
    "List the saved scripted journeys (declarative YAML paths through the app) with each one's ",
    "step count and auth setup. The authoring complement to reproit_repros: journeys are ",
    "hand/agent-authored repros with assertions. Run one with reproit_check <name>."
);

const JOURNEY_SAVE_DESCRIPTION: &str = concat!(
    "Author a scripted journey: write journeys/<name>.yaml from a structured spec, then ",
    "confirm it with reproit_check <name> (pass/fail/flaky/stale, deterministic). GROUND IT ",
    "FIRST with reproit_context. Address actions by structural selectors from the map/context, ",
    "such as tap:key:testid:add or tap:role:button#0. A journey is a list of `steps`, each ",
    "EXACTLY ONE of: {\"do\":\"tap:key:testid:add\"} explicit action (or \"back\"); ",
    "{\"goto\":\"<screen>\"} pathfind the map to a screen; {\"expect\":{...}} assert one ",
    "of state/text/count: {\"state\":\"<screen>\"} | {\"text\":\"<visible substring>\"} ",
    "| {\"count\":{\"<finder>\":N}}; {\"fill\":{\"<finder>\":\"<value>\"}} type ",
    "into fields, where a value of \"secret:password\"/\"secret:username\" is injected from ",
    "the auth vault (never hardcode credentials). Optional top-level ",
    "\"setup\":\"login(<account>)\" (drive the login UI first) or ",
    "\"auth(<account>)\" (restore a saved session, skip the UI). MULTI-USER: to ",
    "test two+ logged-in users interacting (e.g. one posts, another sees it), add \"actors\" ",
    "and tag every step with the actor that performs it. \"actors\" is either a bare list ",
    "[\"alice\",\"bob\"] or a map binding each actor to its login ",
    "{\"alice\":{\"login\":\"alice\"},\"bob\":{\"auth\":\"bob\"}} (login = drive ",
    "the UI with that account's vault creds; auth = restore its session). reproit launches one ",
    "device per actor, runs steps in the listed order across them (so alice's effect is ",
    "observable to bob), and a `secret:` fill in a step binds to that step's actor account. ",
    "Multi-actor steps support do/expect(text|count)/fill only (no goto/expect:state). A failed ",
    "assertion or unreachable step reports STALE, not pass."
);

const JOURNEY_SPEC_DESCRIPTION: &str = concat!(
    "The spec. Single-user: {\"setup\"?: \"login(guest)\", \"steps\": [ {\"do\":...} | ",
    "{\"goto\":...} | {\"expect\":...} | {\"fill\":...} ]}. Multi-user: {\"actors\": ",
    "{\"alice\":{\"login\":\"alice\"}, \"bob\":{\"auth\":\"bob\"}}, \"steps\": ",
    "[ {\"actor\":\"alice\", \"do\":...}, {\"actor\":\"bob\", ",
    "\"expect\":{\"text\":...}} ]}."
);

const CLOUD_BUCKETS_DESCRIPTION: &str = concat!(
    "The IMPACT-RANKED bug list and the loop's STARTING point: returns each bucket's ",
    "content-addressed `bucketId`, impact score + severity, resolution status, count, and ",
    "message, already sorted by impact (highest first). This is the ONLY tool that surfaces ",
    "the `bucketId` -- the id reproit_cloud_pull / reproit_cloud_triage / ",
    "reproit_cloud_timeline take via `bucket`. Distinct from reproit_cloud_blast_radius (the ",
    "cohort who's-affected lens, which has no bucket id). `app` is the cloud app id (defaults ",
    "to $REPROIT_CLOUD_APP); `query` filters buckets by message substring."
);

const BLAST_RADIUS_DESCRIPTION: &str = concat!(
    "Who's affected by a bucket: cohorts, percentages, versions. `bucket` is the bucket index ",
    "(or signature). `app` is the cloud app id (defaults to $REPROIT_CLOUD_APP)."
);

const CLOUD_REPRODUCE_DESCRIPTION: &str = concat!(
    "Pull a real user session for a bucket and replay it locally in one step (pull -> check), ",
    "reporting whether it reproduced (an exception fired) or replayed clean (likely ",
    "data-dependent). Equivalent to reproit_cloud_pull then reproit_check, but saves the repro ",
    "AND runs it. `bucket` is the content-addressed bucket id from reproit_cloud_buckets (a ",
    "`bkt_...` id, NOT an integer index). `as` is a short local name for the saved repro (used ",
    "as reproit_check <name>). `app` defaults to $REPROIT_CLOUD_APP. Slow: a real run."
);

const CLOUD_PULL_DESCRIPTION: &str = concat!(
    "Pull a production bug from the cloud as a FIRST-CLASS LOCAL repro you can then verify + ",
    "fix offline. The autonomous fix loop: reproit_cloud_buckets (impact-ranked) -> ",
    "reproit_cloud_pull the top bucket -> reproit_check <as> (reproduces it locally, ",
    "NETWORK-FREE) -> fix the code -> reproit_check <as> again (proves the fix) -> ",
    "reproit_keep. `bucket` is the content-addressed bucket id from reproit_cloud_buckets; `as` ",
    "is a short local name used in reproit_check; `app` defaults to $REPROIT_CLOUD_APP."
);

const CLOUD_TRIAGE_DESCRIPTION: &str = concat!(
    "READ or SET a bucket's triage status: the MANAGEMENT state an agent owns (where in the ",
    "lifecycle, who's on it), distinct from the prod-truth resolution the system computes. ",
    "With only `bucket`, READS + returns the current state. With `status`, SETS it: new | ",
    "triaged | assigned | fixed | wontfix. This is how an agent RECORDS its intent in the ",
    "loop: after reproit_check proves a fix holds locally, call this with status=fixed and ",
    "`fixed_in_build`=<the build you shipped the fix in> to anchor the claim; production then ",
    "confirms (resolved) or contradicts (regressed) it, which you read back via ",
    "reproit_cloud_resolution_events. `assignee` (an org member id) is only valid with ",
    "status=assigned; `fixed_in_build` only with status=fixed (and defaults server-side to the ",
    "newest build seen). `bucket` is the content-addressed bucket id from ",
    "reproit_cloud_buckets; `app` defaults to $REPROIT_CLOUD_APP."
);

const RESOLUTION_EVENTS_DESCRIPTION: &str = concat!(
    "MONITOR the loop: list recent PROD-TRUTH transitions (resolved->regressed, ",
    "resolving->resolved, ...), newest first, computed from production telemetry against each ",
    "bucket's fix anchor. This is what an autonomous monitor reads to catch a REGRESSION: a ",
    "bucket you marked fixed (reproit_cloud_triage status=fixed) that started firing again in ",
    "production. A `regressed` event is the signal to reproit_cloud_pull it again and re-open ",
    "the fix loop. `app` defaults to $REPROIT_CLOUD_APP."
);

const CLOUD_TIMELINE_DESCRIPTION: &str = concat!(
    "The per-bucket OCCURRENCE time-series (segmented by build) plus the computed prod-truth ",
    "resolution status. Shows whether occurrences dropped after a fix anchor ",
    "(resolving/resolved) or returned (regressed). Use it to confirm a specific bucket's fix is ",
    "actually holding in production, or to see the shape of a regression. `bucket` is the ",
    "content-addressed bucket id; `app` defaults to $REPROIT_CLOUD_APP."
);

fn tool_defs() -> Value {
    json!([
        {
            "name": "reproit_context",
            "description": CONTEXT_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "target": {
                    "type": "string",
                    "description": concat!(
                        "Alias or node id to scope the graph + selectors to (e.g. ",
                        "\"login\"). Omit for the whole graph."
                    )
                }
            } }
        },
        {
            "name": "reproit_accessibility",
            "description": ACCESSIBILITY_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "state": {
                    "type": "string",
                    "description": concat!(
                        "Scope to one screen, by signature id or human name. ",
                        "Omit for all screens."
                    )
                },
                "kind": {
                    "type": "string",
                    "description": concat!(
                        "Filter to one dimension: pointer_only | keyboard_unreachable | ",
                        "no_role | focus_trap."
                    )
                }
            } }
        },
        {
            "name": "reproit_coverage",
            "description": COVERAGE_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "reproit_scan",
            "description": SCAN_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "target": {
                    "type": "string",
                    "description": concat!(
                        "A URL (https://app.com, zero-config) or an alias/node to scope the ",
                        "crawl to."
                    )
                },
                "record_video": {
                    "type": "boolean",
                    "description": concat!(
                        "Record every distinct reported finding into ",
                        ".reproit/recordings/scan/. Visually localizable findings are boxed; ",
                        "the rest are diagnostic clips."
                    )
                },
                "only": {
                    "type": "string",
                    "enum": ["route-access"],
                    "description": "Evaluate only the declared browser route-access matrix."
                }
            } }
        },
        {
            "name": "reproit_fuzz",
            "description": FUZZ_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "target": {
                    "type": "string",
                    "description": "Alias/node to concentrate the hunt on (e.g. \"login\")."
                },
                "platform": {
                    "type": "string",
                    "description": concat!(
                        "ios|android|web|all (comma list -> run all + ",
                        "divergence diff)."
                    )
                }
            } }
        },
        {
            "name": "reproit_check",
            "description": CHECK_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "repro": {
                    "type": "string",
                    "description": concat!(
                        "Saved repro id/alias, or a pending finding id from reproit_fuzz. ",
                        "Omit to run the whole saved suite."
                    )
                },
                "changed": {
                    "type": "string",
                    "description": concat!(
                        "Git base used to prioritize repros mapped to changed source files. ",
                        "The complete saved suite still runs."
                    )
                },
                "record_video": {
                    "type": "boolean",
                    "description": "Save annotated screen video as supporting evidence."
                },
                "flicker": {
                    "type": "boolean",
                    "description": "With record_video, also scan for intra-run flicker."
                }
            } }
        },
        {
            "name": "reproit_baseline",
            "description": BASELINE_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "update": {
                    "type": "boolean",
                    "description": "Accept the current capture as the new baseline."
                }
            } }
        },
        {
            "name": "reproit_keep",
            "description": KEEP_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "id": {
                    "type": "string",
                    "description": concat!(
                        "Finding id (dirname) from the latest fuzz run. ",
                        "Omit to use the sole finding."
                    )
                },
                "as": {
                    "type": "string",
                    "description": "Human alias for the kept repro (used in reproit_check)."
                }
            } }
        },
        {
            "name": "reproit_repros",
            "description": REPROS_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "reproit_simplify",
            "description": SIMPLIFY_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "repro": {
                    "type": "string",
                    "description": "Repro id/alias (or a pending finding id) to simplify."
                },
                "actions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": concat!(
                        "Candidate action sequence, e.g. [\"tap:key:testid:add\",",
                        "\"tap:key:testid:open-cart\",\"tap:key:testid:remove\"]."
                    )
                }
            }, "required": ["repro", "actions"] }
        },
        {
            "name": "reproit_why",
            "description": WHY_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "repro": {
                    "type": "string",
                    "description": concat!(
                        "Repro/alias to scope fault localization to. ",
                        "Omit for all runs under .reproit/runs."
                    )
                }
            } }
        },
        {
            "name": "reproit_journeys",
            "description": JOURNEYS_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "reproit_journey_save",
            "description": JOURNEY_SAVE_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "name": {
                    "type": "string",
                    "description": "Journey name (the file stem under journeys/)."
                },
                "journey": { "type": "object", "description": JOURNEY_SPEC_DESCRIPTION }
            }, "required": ["name", "journey"] }
        },
        {
            "name": "reproit_cloud_buckets",
            "description": CLOUD_BUCKETS_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "app": {
                    "type": "string",
                    "description": "Cloud app id (default: $REPROIT_CLOUD_APP)."
                },
                "query": { "type": "string", "description": "Filter buckets by message substring." }
            } }
        },
        {
            "name": "reproit_cloud_blast_radius",
            "description": BLAST_RADIUS_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "bucket": {
                    "type": "string",
                    "description": "Bucket index (integer) or signature."
                },
                "app": {
                    "type": "string",
                    "description": "Cloud app id (default: $REPROIT_CLOUD_APP)."
                }
            }, "required": ["bucket"] }
        },
        {
            "name": "reproit_cloud_reproduce",
            "description": CLOUD_REPRODUCE_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "bucket": {
                    "type": "string",
                    "description": "Content-addressed bucket id (from reproit_cloud_buckets)."
                },
                "as": {
                    "type": "string",
                    "description": concat!(
                        "A short local name for the pulled repro ",
                        "(used as reproit_check <name>)."
                    )
                },
                "app": {
                    "type": "string",
                    "description": "Cloud app id (default: $REPROIT_CLOUD_APP)."
                }
            }, "required": ["bucket", "as"] }
        },
        {
            "name": "reproit_cloud_pull",
            "description": CLOUD_PULL_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "bucket": {
                    "type": "string",
                    "description": "Content-addressed bucket id (from reproit_cloud_buckets)."
                },
                "as": {
                    "type": "string",
                    "description": concat!(
                        "A short local name for the pulled repro ",
                        "(used as reproit_check <name>)."
                    )
                },
                "app": {
                    "type": "string",
                    "description": "Cloud app id (default: $REPROIT_CLOUD_APP)."
                }
            }, "required": ["bucket", "as"] }
        },
        {
            "name": "reproit_cloud_triage",
            "description": CLOUD_TRIAGE_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "bucket": {
                    "type": "string",
                    "description": "Content-addressed bucket id (from reproit_cloud_buckets)."
                },
                "status": {
                    "type": "string",
                    "description": concat!(
                        "Set the status: new | triaged | assigned | fixed | wontfix. ",
                        "Omit to READ the current state."
                    )
                },
                "fixed_in_build": {
                    "type": "string",
                    "description": concat!(
                        "The build the fix shipped in (the prod-resolution anchor). ",
                        "Only meaningful with status=fixed; defaults to the newest build ",
                        "seen for the bucket."
                    )
                },
                "assignee": {
                    "type": "integer",
                    "description": concat!(
                        "Org member id to assign. Required by, and only valid for, ",
                        "status=assigned."
                    )
                },
                "app": {
                    "type": "string",
                    "description": "Cloud app id (default: $REPROIT_CLOUD_APP)."
                }
            }, "required": ["bucket"] }
        },
        {
            "name": "reproit_cloud_resolution_events",
            "description": RESOLUTION_EVENTS_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "app": {
                    "type": "string",
                    "description": "Cloud app id (default: $REPROIT_CLOUD_APP)."
                }
            } }
        },
        {
            "name": "reproit_cloud_timeline",
            "description": CLOUD_TIMELINE_DESCRIPTION,
            "inputSchema": { "type": "object", "properties": {
                "bucket": {
                    "type": "string",
                    "description": "Content-addressed bucket id (from reproit_cloud_buckets)."
                },
                "app": {
                    "type": "string",
                    "description": "Cloud app id (default: $REPROIT_CLOUD_APP)."
                }
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
/// A one-line, actionable gloss for a `check` verdict, derived from its --json
/// `outcome`. Keeps the four outcomes legible and DISTINCT for the agent: a STALE
/// is "could not run, re-record", never a soft pass; a FAIL is a confirmed
/// reproduction. Returns None when the output carries no outcome (a real error
/// the caller already surfaces) or an unknown outcome string.
fn check_gloss(stdout: &[u8]) -> Option<String> {
    let v = serde_json::from_slice::<serde_json::Value>(stdout).ok()?;
    let outcome = v.get("outcome").and_then(Value::as_str)?;
    let msg = match outcome {
        "pass" => concat!(
            "PASS: replayed clean and reached the trigger context -- the bug is fixed ",
            "(deterministic, so this is a real green)."
        ),
        "fail" => concat!(
            "FAIL: the finding REPRODUCED (a confirmed regression). If you were confirming a ",
            "fuzz finding, this is the expected signal to keep it; if you were verifying a ",
            "fix, the fix did not hold."
        ),
        "flaky" => concat!(
            "FLAKY: the finding reproduced on SOME replays but not all (a non-deterministic ",
            "app race) -- not a clean reproduction and not a clean fix."
        ),
        "stale" => concat!(
            "STALE: could not run the case to its trigger -- the UI path moved or the runner ",
            "could not replay it. This is NOT a pass or a verdict on the bug: retry so reproit ",
            "refreshes its internal model, then re-record if the authored path itself changed; ",
            "the change may also have fixed it."
        ),
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
mod tests;
