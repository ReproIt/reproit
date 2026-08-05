//! Pure MCP tool-to-CLI argument dispatch.

use super::{missing, missing_app};
use serde_json::Value;

pub(super) fn build_argv(
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
            // Debug is an implementation surface; the command refreshes the
            // internal model before rendering it for the authoring agent.
            argv.extend(["debug".into()]);
            argv.push("map".into());
            argv.push("show".into());
        }
        "reproit_accessibility" => {
            // The UI-vs-a11y diff, read from the map's operability gaps.
            argv.extend(["debug".into()]);
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
            argv.extend(["debug".into()]);
            argv.push("map".into());
            argv.push("semantic".into());
        }
        "reproit_fuzz" => {
            argv.extend(["fuzz".into()]);
            // The agent wants the whole deduped work-list: collect findings
            // across the seed budget and group them into unique bugs (same bug
            // reached by different paths counts once).
            argv.push("--all".into());
            if let Some(t) = s("target") {
                argv.extend(["--journey".into(), t]);
            }
            if let Some(p) = s("platform") {
                argv.extend(["--target".into(), p]);
            }
        }
        "reproit_scan" => {
            argv.extend(["scan".into()]);
            if let Some(t) = s("target") {
                argv.push(t);
            }
            if let Some(only) = s("only") {
                if only != "route-access" {
                    return Err((format!("unsupported scan contract family {only:?}"), true));
                }
                argv.extend(["--only".into(), only]);
            }
            if b("record_video") {
                argv.push("--record-video".into());
            }
        }
        "reproit_check" => {
            if let Some(r) = s("repro") {
                if args.get("changed").is_some() {
                    return Err((
                        "`changed` orders the full suite and cannot be combined with `repro`"
                            .into(),
                        true,
                    ));
                }
                let direct = if r.starts_with('@')
                    || r.starts_with("fnd_")
                    || r.starts_with("rep_")
                    || r.starts_with("bkt_")
                {
                    r
                } else {
                    format!("@{r}")
                };
                argv.push(direct);
            } else {
                argv.push("check".into());
                if let Some(base) = s("changed") {
                    argv.extend(["--changed".into(), base]);
                }
            }
            if b("record_video") {
                argv.push("--record-video".into());
            }
            if b("flicker") {
                argv.push("--flicker".into());
            }
        }
        "reproit_reproduce" => {
            // Hermetic reproduce: verdict from re-executed code. A capture
            // file with an exec override routes through check --exec; every
            // other reference (occ_, guard id) resolves its recipe from
            // repo-local config or the guard's stored hermetic.json. Headless
            // is automatic (the dispatcher is never a TTY), not a flag.
            let reference = args
                .get("reference")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(exec) = args.get("exec").and_then(Value::as_str) {
                argv.push("check".into());
                argv.push(reference.to_string());
                argv.push("--exec".into());
                argv.push(exec.to_string());
            } else {
                argv.push(reference.to_string());
            }
        }
        "reproit_verify" => {
            // Batch proof-of-fix: replay every persisted backend finding (or the
            // named ids) and assert none still reproduces. A green result is
            // machine-checkable proof the agent's fix holds.
            argv.extend(["verify".into()]);
            if let Some(ids) = args.get("ids").and_then(Value::as_array) {
                for id in ids {
                    if let Some(id) = id.as_str() {
                        argv.push(id.to_string());
                    }
                }
            }
        }
        "reproit_accept" => {
            argv.extend(["accept".into()]);
            if let Some(ids) = args.get("ids").and_then(Value::as_array) {
                for id in ids {
                    if let Some(id) = id.as_str() {
                        argv.push(id.to_string());
                    }
                }
            }
            if let Some(reason) = args.get("reason").and_then(Value::as_str) {
                argv.push("--reason".into());
                argv.push(reason.to_string());
            }
            if let Some(until) = args.get("until").and_then(Value::as_str) {
                argv.push("--until".into());
                argv.push(until.to_string());
            }
            if b("remove") {
                argv.push("--remove".into());
            }
        }
        "reproit_baseline" => {
            argv.extend(["baseline".into()]);
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
        "reproit_repros" => argv.extend(["list".into()]),
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
            argv.extend([
                "__cloud-internal".into(),
                "buckets".into(),
                "--app".into(),
                app,
            ]);
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
            argv.extend([
                "__cloud-internal".into(),
                "blast-radius".into(),
                "--app".into(),
                app,
            ]);
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
            let Some(as_name) = s("as") else {
                return Err((missing("as"), true));
            };
            // Bucket-first: pass the content-addressed `bkt_...` id to the
            // bucket-based reproduce (pull -> check). The old `--idx <bucket>`
            // dispatch broke with real bucket ids (they are not integers).
            argv.extend([
                "__cloud-internal".into(),
                "__replay-dispatch".into(),
                "--app".into(),
                app,
                "--bucket".into(),
                bucket,
                "--as".into(),
                as_name,
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
                "__cloud-internal".into(),
                "__pull".into(),
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
                "__cloud-internal".into(),
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
                "__cloud-internal".into(),
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
                "__cloud-internal".into(),
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
