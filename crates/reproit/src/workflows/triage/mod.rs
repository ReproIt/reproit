//! Production-bug triage: the "here's my issue, look at it and reproduce it"
//! flow, over the cloud's telemetry. Pairs with `reproit mcp` so a coding agent
//! (or a person) can ask, in plain words, what a bug might be and get a
//! deterministic reproduction.
//!
//! - `find`: list production error clusters + their context discriminator.
//! - `explain`: one bucket package in full (path, "which users" discriminator,
//!   suspected source from the stack, and the replay).
//! - `reproduce`: pull a bucket package, then run the saved local repro.
//! - `diagnose`: match a free-text report to a cluster, then explain (+repro).
//!
//! The cloud base URL/key come from --cloud/--key, then REPROIT_CLOUD_URL /
//! REPROIT_CLOUD_KEY, then the hosted cloud. Output is plain text so MCP can
//! relay it.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::domain::repro;

mod lifecycle;
mod presentation;
mod reproduction;
mod setup;
mod transport;

use lifecycle::first_line;
pub use lifecycle::{diagnose, resolution_events, timeline, triage};
pub use presentation::{buckets, explain, filter_buckets, filter_errors, find, top_bucket_id};
#[allow(unused_imports)]
// Preserve pure materialization helpers on `crate::workflows::triage`.
pub use reproduction::{
    build_replay_json, fetch_bucket_package, materialize_pull, pull, pull_global,
    report_tester_capture, reproduce_bucket, reproduce_bucket_global, verify_tester_capture,
    PulledRepro,
};
#[allow(unused_imports)] // Preserve the existing crate-level verdict façade.
pub(crate) use reproduction::{classify_repro, ReproVerdict};
pub(crate) use reproduction::{print_pull_next_step, PullContinuation};
pub use setup::{git_toplevel, setup};
use transport::Cloud;
#[allow(unused_imports)] // Preserve the device-login response type façade.
pub use transport::{
    bucket_app, device_login, pending_captures, raw, raw_buckets, validate_login, CloudProject,
    DeviceLogin,
};

#[cfg(test)]
use setup::{parse_git_remote_slug, REPRO_WORKFLOW};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_workflow_verifies_prs_and_reports_the_head_commit() {
        let workflow = REPRO_WORKFLOW.replace("__REPROIT_APP_ID__", "app_demo");
        assert!(workflow.contains("pull_request:"));
        assert!(workflow.contains("reproit check --strict --runs 3"));
        assert!(workflow.contains("github.event.pull_request.head.sha"));
        assert!(workflow.contains("replay-results"));
        assert!(workflow.contains("REPROIT_APP_ID: \"app_demo\""));
        assert!(!workflow.contains("__REPROIT_APP_ID__"));
    }
    use serde_json::json;

    #[test]
    fn parse_git_remote_slug_handles_every_form() {
        let owner_repo = Some("acme/web".to_string());
        // SSH scp-style, with and without .git.
        assert_eq!(
            parse_git_remote_slug("git@github.com:acme/web.git"),
            owner_repo
        );
        assert_eq!(parse_git_remote_slug("git@github.com:acme/web"), owner_repo);
        // https, with and without .git and trailing slash.
        assert_eq!(
            parse_git_remote_slug("https://github.com/acme/web.git"),
            owner_repo
        );
        assert_eq!(
            parse_git_remote_slug("https://github.com/acme/web"),
            owner_repo
        );
        assert_eq!(
            parse_git_remote_slug("https://github.com/acme/web/"),
            owner_repo
        );
        // ssh:// url and an https url carrying a user@ (token) prefix.
        assert_eq!(
            parse_git_remote_slug("ssh://git@github.com/acme/web.git"),
            owner_repo
        );
        assert_eq!(
            parse_git_remote_slug("https://x-token@github.com/acme/web.git"),
            owner_repo
        );
        // Host-agnostic (GitHub Enterprise): still owner/repo.
        assert_eq!(
            parse_git_remote_slug("git@ghe.corp.internal:acme/web.git"),
            owner_repo
        );
        // Trailing whitespace/newline from `git remote get-url`.
        assert_eq!(
            parse_git_remote_slug("git@github.com:acme/web.git\n"),
            owner_repo
        );
        // Not a recognizable remote.
        assert_eq!(parse_git_remote_slug("not-a-url"), None);
        assert_eq!(parse_git_remote_slug(""), None);
    }

    #[test]
    fn classify_repro_distinguishes_clean_from_stale() {
        // The JSON verdict wins.
        assert_eq!(
            classify_repro(Some("fail"), Some(0)),
            ReproVerdict::Reproduced
        );
        assert_eq!(
            classify_repro(Some("pass"), Some(0)),
            ReproVerdict::NotReproduced
        );
        assert_eq!(classify_repro(Some("stale"), Some(0)), ReproVerdict::Stale);
        assert_eq!(classify_repro(Some("flaky"), Some(0)), ReproVerdict::Flaky);
        // No JSON: fall back to the exit-code contract (1/2/3/0).
        assert_eq!(classify_repro(None, Some(1)), ReproVerdict::Reproduced);
        assert_eq!(classify_repro(None, Some(2)), ReproVerdict::Flaky);
        assert_eq!(classify_repro(None, Some(3)), ReproVerdict::Stale);
        assert_eq!(classify_repro(None, Some(0)), ReproVerdict::NotReproduced);
        assert_eq!(classify_repro(None, None), ReproVerdict::CouldNotReplay);
        // The old bug: a stale run (exit 3 / outcome stale) must NOT read as
        // reproduced just because the process did not exit 0.
        assert_ne!(
            classify_repro(Some("stale"), Some(3)),
            ReproVerdict::Reproduced
        );
    }

    #[test]
    fn materialize_pull_writes_a_checkable_repro_shape() {
        // A bucket replay package (the content-addressed endpoint's shape) ->
        // Meta + actions identical in SHAPE to what `keep` writes, so `check`
        // reads it unchanged. This is the pure materialization core (no network,
        // no fs): given the package JSON, materialize the local repro.
        let pkg = json!({
            "bucketId": "b00b",
            "expectedError": "Uncaught TypeError: state.reset is not a function",
            "crashSig": "crash:TypeError:state.reset",
            "startSig": "home",
            "replay": ["tap:key:id:reset", "key:Enter"],
            "fixtureSpec": {},
        });
        let pulled = materialize_pull(&pkg, "login-crash", "2026-06-21T00:00:00+00:00").unwrap();

        // The action sequence is the package's PII-safe replay, in order.
        assert_eq!(pulled.actions, vec!["tap:key:id:reset", "key:Enter"]);

        let m = &pulled.meta;
        // Identity: the SAME content hash `keep`/`check` use (seed 0, normalized
        // actions). 12 hex chars, deterministic.
        assert_eq!(m.id, repro::repro_id(0, &pulled.actions));
        assert_eq!(m.id.len(), 12);
        // Alias = --as; status quarantined (a fresh save); seed defaulted to 0.
        assert_eq!(m.alias.as_deref(), Some("login-crash"));
        assert_eq!(m.status, repro::Status::Quarantined);
        assert_eq!(m.seed, 0);
        // Trigger context: index = replay length, sig = crashSig, oracle = crash.
        assert_eq!(m.trigger_index, Some(2));
        assert_eq!(
            m.trigger_sig.as_deref(),
            Some("crash:TypeError:state.reset")
        );
        assert_eq!(m.oracle.as_deref(), Some("crash"));
        assert_eq!(m.created, "2026-06-21T00:00:00+00:00");
        // An empty fixtureSpec -> empty fixture: replay.json is the bare
        // {seed, replay} shape, no inputs/locale (a path-only repro).
        assert!(pulled.fixture.is_empty());
        let replay = build_replay_json(m.seed, &pulled.actions, &pulled.fixture);
        assert_eq!(replay["seed"], json!(0));
        assert_eq!(replay["replay"], json!(["tap:key:id:reset", "key:Enter"]));
        assert!(replay.get("inputs").is_none());
        assert!(replay.get("locale").is_none());
    }

    #[test]
    fn tester_capture_materializes_without_actions_and_keeps_its_oracle() {
        let pkg = json!({
            "bucketId": "bkt_manual",
            "crashSig": "state-checkout-broken",
            "replay": [],
            "findingIdentity": {
                "oracle": "tester-capture",
                "invariant": "tester-observed-failure",
                "kind": "structural-state",
                "message": "",
                "frame": "",
                "trigger": "load",
                "boundary": "state-checkout-broken"
            },
            "fixtureSpec": {}
        });
        let pulled = materialize_pull(&pkg, "manual", "2026-07-16T00:00:00Z").unwrap();
        assert!(pulled.actions.is_empty());
        assert_eq!(pulled.meta.oracle.as_deref(), Some("tester-capture"));
        assert_eq!(
            pulled.meta.trigger_sig.as_deref(),
            Some("state-checkout-broken")
        );
    }

    #[test]
    fn pull_preserves_fixture_in_replay_json() {
        // TASK 1: a data-dependent prod bug (locale + a long-name field) must pull
        // with its property-matched fixture FOLDED INTO replay.json, in the shape
        // `check_repro` forwards verbatim to the runner (top-level `inputs`, and a
        // top-level `locale` it lifts to REPROIT_LOCALE). Without this the repro
        // pulls path-only and replays clean (the bug never fires).
        let pkg = json!({
            "expectedError": "RangeError: index out of range",
            "crashSig": "crash:RangeError:render",
            "replay": ["tap:key:id:name", "type:key:id:name=longname"],
            "fixtureSpec": {
                "locale": "tr",
                "inputs": [{
                    "field": "name",
                    "generate": { "charset": "unicode", "minLen": 312 },
                }],
            },
        });
        let pulled = materialize_pull(&pkg, "name-crash", "t").unwrap();
        assert!(
            !pulled.fixture.is_empty(),
            "the fixtureSpec carries locale + a field, so the fixture is non-empty"
        );

        let replay = build_replay_json(pulled.meta.seed, &pulled.actions, &pulled.fixture);
        // The action sequence is preserved as before.
        assert_eq!(
            replay["replay"],
            json!(["tap:key:id:name", "type:key:id:name=longname"])
        );
        // Locale is lifted to a top-level key (check_repro forwards it to
        // REPROIT_LOCALE when no explicit --locale is given).
        assert_eq!(replay["locale"], json!("tr"));
        // The per-field synthesized value lands in a top-level `inputs` array,
        // exactly where the runner's loadInputs() reads it off each seed config.
        let inputs = replay["inputs"].as_array().expect("inputs array present");
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["field"], json!("name"));
        // A concrete, non-empty synthesized value (deterministic; no RNG).
        let v = inputs[0]["value"].as_str().expect("a string value");
        assert!(
            !v.is_empty(),
            "the long-name field synthesized to a concrete value"
        );
    }

    #[test]
    fn materialize_pull_honors_seed_and_startsig_fallback() {
        // A package carrying an explicit seed and NO crashSig: seed flows into the
        // id, and the trigger sig falls back to startSig.
        let pkg = json!({
            "seed": 7,
            "startSig": "checkout",
            "replay": ["tap:key:id:pay"],
        });
        let pulled = materialize_pull(&pkg, "pay", "t").unwrap();
        assert_eq!(pulled.meta.seed, 7);
        assert_eq!(pulled.meta.id, repro::repro_id(7, &["tap:key:id:pay"]));
        assert_eq!(pulled.meta.trigger_sig.as_deref(), Some("checkout"));
    }

    #[test]
    fn materialize_pull_rejects_empty_replay() {
        // A package with no executable actions cannot become a check-able repro.
        let pkg = json!({ "replay": [], "crashSig": "x" });
        assert!(materialize_pull(&pkg, "x", "t").is_err());
    }

    #[test]
    fn production_pull_accepts_only_complete_redacted_replayable_capsules() {
        let capsule = json!({
            "version": 2, "id": "cloud-id", "app": "chat", "builds": {}, "environment": {},
            "capabilities": {
                "ui_actions": {"status":"captured"}, "http": {"status":"captured"},
                "http_replay": {"status":"captured"}
            },
            "actions": [{"index":1,"actor":"a","action":"tap:key:send"}],
            "exchanges": [{
                "id":"a-1-0","actor":"a","action_index":1,"ordinal":0,"protocol":"https",
                "method": "POST",
                "url": "https://api.test/send",
                "request_headers": {"authorization": "raw"},
                "request_body":{"token":"raw","message":{"kind":"text"}},"status":200,
                "response_headers": {"content-type": "application/json"},
                "response_body": {"ok": true},
                "required": true
            }],
            "causalGraph": {"version":1,"nodes":[],"edges":[]},
            "environmentEnvelope": {
                "version":1,"complete":false,"replayAttempts":0,
                "relaxedDimensions":[],"trials":[]
            },
            "finding": {
                "oracle": "crash",
                "invariant": "no-exception",
                "kind": "TypeError",
                "frame": "send:1",
                "trigger": "key:send"
            },
            "redactions":[]
        });
        let pulled = materialize_pull(&json!({"capsule": capsule}), "chat-crash", "t").unwrap();
        assert_eq!(pulled.actions, vec!["tap:key:send"]);
        let capsule = pulled.capsule.unwrap();
        assert_ne!(capsule.id, "cloud-id");
        assert_eq!(
            capsule.exchanges[0].request_headers["authorization"],
            "<reproit:secret>"
        );
        assert_ne!(
            capsule.exchanges[0].request_body.as_ref().unwrap()["token"],
            "raw"
        );

        let mut incomplete = serde_json::to_value(capsule).unwrap();
        incomplete["capabilities"]
            .as_object_mut()
            .unwrap()
            .remove("http_replay");
        let error = match materialize_pull(&json!({"capsule": incomplete}), "x", "t") {
            Ok(_) => panic!("incomplete capsule unexpectedly accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("hermetically replayable"));
    }

    #[test]
    fn filter_errors_keeps_matching_messages() {
        let v = json!({ "errors": [
            { "message": "RangeError in feed" },
            { "message": "Null check operator on login" },
            { "message": "RangeError again" },
        ]});
        let out = filter_errors(v, Some("rangeerror"));
        let arr = out["errors"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().all(|e| e["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("rangeerror")));
    }

    #[test]
    fn filter_errors_none_query_is_identity() {
        let v = json!({ "errors": [ { "message": "a" }, { "message": "b" } ] });
        let out = filter_errors(v.clone(), None);
        assert_eq!(out, v);
    }

    #[test]
    fn filter_errors_tolerates_missing_array() {
        let v = json!({ "unexpected": true });
        let out = filter_errors(v.clone(), Some("x"));
        assert_eq!(out, v);
    }

    #[test]
    fn filter_buckets_matches_bucket_identity_fields() {
        let v = json!({ "items": [
            { "bucketId": "bkt_feed", "crashSig": "sig_a", "message": "RangeError in feed" },
            { "bucketId": "bkt_login", "crashSig": "sig_b", "message": "Null check" },
            { "bucketId": "bkt_cart", "crashSig": "checkout_sig", "message": "Payment failed" },
        ]});
        let out = filter_buckets(v, Some("checkout"));
        let arr = out["items"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["bucketId"], "bkt_cart");
    }
}
