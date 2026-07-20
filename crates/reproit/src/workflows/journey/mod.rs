//! Framework-agnostic scripted journeys.
//!
//! A journey is a YAML path through the app: a list of steps that reproit
//! replays deterministically and classifies on the same pass/fail/flaky/stale
//! contract as a fuzz repro. A journey is, in effect, a hand-authored repro
//! with (later) assertions. It is framework-agnostic because the steps are the
//! same finder-based actions every backend already executes
//! (`tap:key:testid:add`, `back`, ...); the per-framework runner is the only
//! framework-specific part, and it is reproit's, not the user's.
//!
//! Steps: `do` (explicit action), `goto: <state>` (pathfind the state graph),
//! `expect` (assert `state`/`text`/`count` against the live screen), and `fill`
//! (type into fields,
//! with `secret:` values injected from the auth vault at run time). A journey
//! may also declare `setup: login(<acct>)` / `auth(<acct>)` to establish auth
//! first. Runs are classified pass / fail (a crash on the way) / stale (a step
//! could not be performed, so the app diverged from the map). `debug map
//! verify` reuses the same replay machinery to re-walk the map and report
//! drift.
//!
//! Addressing contract (every runner must uphold): a selector resolves against
//! VISIBLE / on-screen elements only. A `key:` is exact; `role:<role>#<idx>`
//! and a positional `#<n>` index ONLY the visible elements of that kind, never
//! one built-but-offstage (another PageView/IndexedStack/Tab page, a
//! `display:none` node, a collapsed panel). Visibility can't be resolved
//! host-side, only the runner sees the live UI, so each runner enforces it with
//! its native check (Flutter `hitTestable`, web `getBoundingClientRect`+style,
//! Appium `displayed`). Tapping already does this; filling must match. Today
//! only the web and Flutter runners implement `type`/`fill`; any runner that
//! adds it must resolve visible-only from the start.

//! Journey specifications, planning, replay, and multi-actor execution.

use crate::adapters::config;
use crate::adapters::orchestrator;
use crate::domain::appmap::AppMap;
use crate::domain::map::{action_str, entry_state};
use crate::domain::repro;
use crate::runtime::project_layout as layout;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

mod spec;
use spec::{parse_setup, resolve_fill_value, ActorAuth, SetupKind};
#[allow(unused_imports)]
pub use spec::{ActorList, Expect, IndependentActionPair, Journey, Step};
/// Where journeys live, relative to the project root.
mod persistence;
use persistence::*;
#[allow(unused_imports)]
pub use persistence::{
    discover_login_spec, exists, journey_path, journeys_dir, list, save, JourneySummary,
};
/// Load the committed app map, if one has been built.
mod planning;
use planning::*;
pub use planning::{is_multi_actor_target, prefix_actions};
mod execution;
mod schedule;
#[cfg(test)]
use execution::*;
#[allow(unused_imports)]
pub use execution::{fuzz_multi_checkpoint, run, MultiFuzzSummary};
mod replay;
use replay::*;
mod verification;
#[cfg(test)]
use verification::*;
#[allow(unused_imports)]
pub use verification::{verify_map, Drift, VerifyReport};
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_a_two_screen_phone_otp_login_from_semantics() {
        let map: AppMap = serde_json::from_value(serde_json::json!({
            "app":"chat", "version":1, "invariants":[], "interrupts":[],
            "states": {
                "phone": {
                    "description": "Sign in with phone",
                    "signature": {
                        "screenshot_phash": null,
                        "semantics_hash": "a",
                        "route": "/login"
                    },
                    "elements": [{
                        "sel": "key:testid:phone",
                        "role": "textbox",
                        "label": "Número de teléfono",
                        "inputPurpose": "phone"
                    }],
                    "texts": [],
                    "parameters": []
                },
                "verify": {
                    "description": "Verification code",
                    "signature": {
                        "screenshot_phash": null,
                        "semantics_hash": "b",
                        "route": "/verify"
                    },
                    "elements": [{
                        "sel": "key:testid:otp",
                        "role": "textbox",
                        "label": "Код подтверждения",
                        "inputPurpose": "otp"
                    }],
                    "texts": [],
                    "parameters": []
                },
                "home": {
                    "description": "Home",
                    "signature": {
                        "screenshot_phash": null,
                        "semantics_hash": "c",
                        "route": "/"
                    },
                    "elements": [],
                    "texts": [],
                    "parameters": []
                }
            },
            "transitions":[
                {
                    "from": "phone",
                    "to": "verify",
                    "action": {"kind": "tap", "finder": "key:testid:continue"},
                    "guards": [],
                    "reversibility": "verified_irreversible",
                    "expected": null
                },
                {
                    "from": "verify",
                    "to": "home",
                    "action": {"kind": "tap", "finder": "key:testid:verify"},
                    "guards": [],
                    "reversibility": "verified_irreversible",
                    "expected": null
                }
            ]
        }))
        .unwrap();
        let spec = discover_login_spec(
            &map,
            "bob",
            crate::adapters::config::AuthStrategy::PhoneOtp,
            Some("Inbox"),
        )
        .unwrap();
        assert!(spec.contains("secret:bob.phone"));
        assert!(spec.contains("secret:bob.otp"));
        assert!(spec.contains("tap:key:testid:continue"));
        assert!(spec.contains("tap:key:testid:verify"));
        assert!(spec.contains("Inbox"));
    }

    #[test]
    fn build_scenario_tags_actions_in_order() {
        let j = serde_yaml::from_str::<Journey>(
            "actors: [alice, bob]\nsteps:\n  - actor: alice\n    fill:\n      key:testid:msg: \
             hi\n  - actor: alice\n    do: tap:key:testid:send\n  - actor: bob\n    expect:\n      \
             text: hi\n",
        )
        .unwrap();
        let (actors, tagged) = build_scenario(Path::new("/nonexistent"), None, &j).unwrap();
        assert_eq!(actors, vec!["alice", "bob"]);
        assert_eq!(
            tagged,
            vec![
                ("alice".into(), "type:key:testid:msg=hi".into()),
                ("alice".into(), "tap:key:testid:send".into()),
                ("bob".into(), "assert:text=hi".into()),
            ]
        );
    }

    #[test]
    fn scenario_rejects_single_actor_only_steps() {
        let goto =
            serde_yaml::from_str::<Journey>("actors: [a]\nsteps:\n  - actor: a\n    goto: home\n")
                .unwrap();
        assert!(build_scenario(Path::new("/nonexistent"), None, &goto).is_err());
        let state = serde_yaml::from_str::<Journey>(
            "actors: [a]\nsteps:\n  - actor: a\n    expect: { state: home }\n",
        )
        .unwrap();
        assert!(build_scenario(Path::new("/nonexistent"), None, &state).is_err());
        let no_actor =
            serde_yaml::from_str::<Journey>("actors: [a]\nsteps:\n  - do: back\n").unwrap();
        assert!(build_scenario(Path::new("/nonexistent"), None, &no_actor).is_err());
    }

    #[test]
    fn scripted_journeys_default_to_sim_tier() {
        let dflt = serde_yaml::from_str::<Journey>("steps:\n  - do: back\n").unwrap();
        assert!(sim_tier(&dflt), "no tier -> sim (E2E by default)");
        let head =
            serde_yaml::from_str::<Journey>("tier: headless\nsteps:\n  - do: back\n").unwrap();
        assert!(!sim_tier(&head), "tier: headless opts out");
        let sim = serde_yaml::from_str::<Journey>("tier: sim\nsteps:\n  - do: back\n").unwrap();
        assert!(sim_tier(&sim));
    }

    #[test]
    fn per_actor_auth_prelude_and_secret_fills() {
        // Map-form actors bind each actor to a session-restore account; a
        // `secret:` fill in a step resolves against that actor's account.
        let j = serde_yaml::from_str::<Journey>(
            "actors:\n  alice: { auth: alice }\n  bob: { auth: bob }\nsteps:\n  - actor: \
                 alice\n    fill:\n      key:testid:msg: secret:password\n  - actor: bob\n    \
                 expect:\n      text: hi\n",
        )
        .unwrap();
        let (actors, tagged) = build_scenario(Path::new("/nonexistent"), None, &j).unwrap();
        assert_eq!(actors, vec!["alice", "bob"]);
        assert_eq!(
            tagged,
            vec![
                // preludes first, in actor order
                ("alice".into(), "auth:alice".into()),
                ("bob".into(), "auth:bob".into()),
                // then the steps; alice's secret fill binds to her account
                (
                    "alice".into(),
                    "type:key:testid:msg=${REPROIT_SECRET_ALICE_PASSWORD}".into()
                ),
                ("bob".into(), "assert:text=hi".into()),
            ]
        );
    }

    fn parse(yaml: &str) -> Journey {
        serde_yaml::from_str(yaml).unwrap()
    }

    /// A tiny map: entry `s_a` --tap add--> `s_b` --tap go--> `s_c`.
    fn chain_map() -> AppMap {
        let sig = serde_json::json!({
            "screenshot_phash": null, "semantics_hash": null, "route": null
        });
        serde_json::from_value(serde_json::json!({
            "app": "t", "version": 1,
            "states": {
                "s_a": {"description": "start", "signature": sig},
                "s_b": {"description": "mid",   "signature": sig},
                "s_c": {"description": "end",   "signature": sig},
            },
            "transitions": [
                {
                    "from": "s_a", "to": "s_b",
                    "action": {"kind": "tap", "finder": "key:testid:add"},
                    "reversibility": "proposed_reversible"
                },
                {
                    "from": "s_b", "to": "s_c",
                    "action": {"kind": "tap", "finder": "key:testid:go"},
                    "reversibility": "proposed_reversible"
                },
            ],
            "invariants": []
        }))
        .unwrap()
    }

    #[test]
    fn replay_trace_aligns_states_and_misses() {
        let log =
            "FUZZ:STATE a\nFUZZ:ACT tap:add\nFUZZ:STATE b\nFUZZ:ACT tap:go\nFUZZ:MISS tap:go\n";
        let (initial, steps) = replay_trace(log);
        assert_eq!(initial.as_deref(), Some("a"));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].state_after.as_deref(), Some("b"));
        assert!(!steps[0].missed);
        assert!(steps[1].missed);
        assert_eq!(steps[1].state_after, None);
    }

    #[test]
    fn edge_path_to_finds_chain() {
        let m = chain_map();
        assert_eq!(edge_path_to(&m, "s_a", "s_a"), Some(vec![]));
        assert_eq!(edge_path_to(&m, "s_a", "s_b"), Some(vec![0]));
        assert_eq!(edge_path_to(&m, "s_a", "s_c"), Some(vec![0, 1]));
        assert_eq!(edge_path_to(&m, "s_c", "s_a"), None);
    }

    #[test]
    fn cover_walk_visits_every_edge_once() {
        let plan = cover_walk(&chain_map()).unwrap();
        assert_eq!(
            plan.actions,
            vec!["tap:key:testid:add", "tap:key:testid:go"]
        );
        assert_eq!(plan.edge_at, vec![0, 1]);
        assert!(plan.unreachable.is_empty());
    }

    #[test]
    fn parses_do_steps_into_actions() {
        let j = parse("journey: smoke\nsteps:\n  - do: tap:key:testid:add\n  - do: back\n");
        let plan = resolve(None, &j, None).unwrap();
        assert_eq!(plan.actions, vec!["tap:key:testid:add", "back"]);
    }

    #[test]
    fn goto_and_expect_state_need_a_map() {
        assert!(resolve(None, &parse("steps:\n  - goto: home\n"), None).is_err());
        // `expect: state` needs a map to resolve the signature...
        assert!(resolve(None, &parse("steps:\n  - expect: { state: home }\n"), None).is_err());
    }

    #[test]
    fn expect_text_and_count_compile_to_asserts() {
        // ...but `text`/`count` assertions are evaluated live by the runner, so
        // they need no map.
        let j = parse(
            "steps:\n  - expect:\n      text: Welcome\n  - expect:\n      count:\n        \
             key:testid:item: 3\n",
        );
        let plan = resolve(None, &j, None).unwrap();
        assert_eq!(
            plan.actions,
            vec!["assert:text=Welcome", "assert:count:key:testid:item=3"]
        );
    }

    #[test]
    fn empty_expect_is_rejected() {
        assert!(resolve(None, &parse("steps:\n  - expect: {}\n"), None).is_err());
    }

    #[test]
    fn parse_setup_reads_kind_and_account() {
        assert_eq!(
            parse_setup("login(guest)").unwrap(),
            (SetupKind::Login, "guest".to_string())
        );
        assert_eq!(
            parse_setup(" auth(admin) ").unwrap(),
            (SetupKind::Auth, "admin".to_string())
        );
        assert!(parse_setup("login()").is_err());
        assert!(parse_setup("nope(x)").is_err());
        assert!(parse_setup("login(guest").is_err());
    }

    #[test]
    fn secret_fill_becomes_env_placeholder() {
        let j = parse("steps:\n  - fill:\n      key:testid:pass: secret:password\n");
        let plan = resolve(None, &j, Some("guest")).unwrap();
        assert_eq!(
            plan.actions,
            vec!["type:key:testid:pass=${REPROIT_SECRET_GUEST_PASSWORD}"]
        );
    }

    #[test]
    fn explicit_account_overrides_setup_for_secret() {
        let v = resolve_fill_value("secret:admin.password", Some("guest")).unwrap();
        assert_eq!(v, "${REPROIT_SECRET_ADMIN_PASSWORD}");
    }

    #[test]
    fn bare_secret_without_account_errors() {
        let j = parse("steps:\n  - fill:\n      f: secret:password\n");
        assert!(resolve(None, &j, None).is_err());
    }

    #[test]
    fn fill_expands_to_type_actions() {
        let j = parse(
            "steps:\n  - fill:\n      key:testid:email: guest@example.com\n      key:testid:pass: \
             \"123456\"\n",
        );
        let plan = resolve(None, &j, None).unwrap();
        // BTreeMap orders fields by finder, so the order is deterministic.
        assert_eq!(
            plan.actions,
            vec![
                "type:key:testid:email=guest@example.com",
                "type:key:testid:pass=123456",
            ]
        );
    }

    #[test]
    fn step_with_two_keys_is_rejected() {
        let j = parse("steps:\n  - do: back\n    goto: home\n");
        assert!(resolve(None, &j, None).is_err());
    }

    #[test]
    fn save_then_list_roundtrips() {
        let dir = std::env::temp_dir().join(format!("reproit-jsave-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spec = concat!(
            r#"{"setup":"login(guest)","steps":[{"do":"tap:key:testid:add"},"#,
            r#"{"expect":{"text":"Done"}}]}"#
        );
        let path = save(&dir, "smoke", spec).unwrap();
        assert!(path.exists());
        let listed = list(&dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "smoke");
        assert_eq!(listed[0].steps, 2);
        assert_eq!(listed[0].setup.as_deref(), Some("login(guest)"));
        // The written YAML parses back into a runnable journey.
        let j = load(&dir, "smoke").unwrap();
        assert_eq!(j.steps.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_rejects_bad_specs() {
        let dir = std::env::temp_dir().join(format!("reproit-jbad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(save(&dir, "x", r#"{"steps":[{"do":"back","goto":"home"}]}"#).is_err());
        assert!(save(&dir, "x", r#"{"steps":[{"expect":{}}]}"#).is_err());
        assert!(save(&dir, "x", r#"{"steps":[]}"#).is_err());
        assert!(save(&dir, "../evil", r#"{"steps":[{"do":"back"}]}"#).is_err());
        assert!(save(&dir, "x", "not json").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failed_assertion_is_stale() {
        let log = "FUZZ:ACT assert:text=Welcome\nFUZZ:ASSERT fail text=\"Welcome\"\n";
        assert_eq!(classify_run(log, true), repro::RunVerdict::CouldNotReplay);
        let ok = "FUZZ:ACT assert:text=Welcome\nFUZZ:ASSERT pass text=\"Welcome\"\n";
        assert_eq!(classify_run(ok, true), repro::RunVerdict::Green);
    }

    #[test]
    fn classify_clean_is_green() {
        let log = "FUZZ:ACT tap:add\nFUZZ:ACT back\nJOURNEY DONE\n";
        assert_eq!(classify_run(log, true), repro::RunVerdict::Green);
    }

    #[test]
    fn classify_miss_is_stale() {
        let log = "FUZZ:ACT tap:add\nFUZZ:MISS tap:gone\nJOURNEY DONE\n";
        assert_eq!(classify_run(log, true), repro::RunVerdict::CouldNotReplay);
    }

    #[test]
    fn classify_crash_is_broke() {
        let log = concat!(
            "FUZZ:ACT tap:x\n",
            "flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══\n",
            "boom\n"
        );
        assert_eq!(classify_run(log, true), repro::RunVerdict::Broke);
    }

    /// A `config::Loaded` rooted at `dir`, with a minimal valid config (no
    /// secrets), for exercising `prefix_actions` without a real project.
    fn loaded_at(dir: &Path) -> config::Loaded {
        let cfg: config::Config = serde_yaml::from_str(
            "app:\n  platform: web\ndevices:\n  namePrefix: t\njourneys:\n  driver: x\n  \
             doneMarkers: [DONE]\n",
        )
        .unwrap();
        config::Loaded {
            config: cfg,
            root: dir.to_path_buf(),
        }
    }

    #[test]
    fn from_journey_resolves_to_a_replay_prefix() {
        let dir = std::env::temp_dir().join(format!("reproit-jfrom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("journeys")).unwrap();
        // do + fill + expect:text need no map; they become the prefix the fuzzer
        // replays before branching outward.
        std::fs::write(
            dir.join("journeys").join("checkout.yaml"),
            concat!(
                "steps:\n  - do: tap:key:testid:buy\n",
                "  - fill:\n      key:qty: \"2\"\n",
                "  - expect:\n      text: Thanks\n"
            ),
        )
        .unwrap();
        // Resolves by NAME, like any journey target.
        let by_name = prefix_actions(&loaded_at(&dir), "checkout").unwrap();
        assert_eq!(
            by_name,
            vec!["tap:key:testid:buy", "type:key:qty=2", "assert:text=Thanks"]
        );
        // And by direct PATH (e.g. wherever `reproit import` wrote it).
        let by_path = prefix_actions(
            &loaded_at(&dir),
            dir.join("journeys").join("checkout.yaml").to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(by_path, by_name);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persisted_multi_finding_is_a_runnable_journey() {
        let dir = std::env::temp_dir().join(format!("reproit-multi-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let loaded = loaded_at(&dir);
        let actors = vec!["alice".to_string(), "bob".to_string()];
        let checkpoint = vec![("alice".to_string(), "tap:key:room".to_string())];
        let suffix = vec![("bob".to_string(), "tap:key:refresh".to_string())];
        let id = persist_multi_finding(
            &loaded,
            "chat-ready",
            7,
            &actors,
            &checkpoint,
            &suffix,
            "crash:no-exception:error:boom:frame:key",
            "cap_test",
            &[],
        )
        .unwrap();
        let saved = load(&dir, &id).unwrap();
        assert_eq!(saved.actors.entries().len(), 2);
        assert_eq!(saved.steps.len(), 2);
        assert_eq!(saved.steps[1].actor.as_deref(), Some("bob"));
        assert_eq!(saved.steps[1].do_action.as_deref(), Some("tap:key:refresh"));
        assert!(dir
            .join(".reproit/findings")
            .join(&id)
            .join("finding.json")
            .is_file());
        assert_eq!(
            std::fs::read_to_string(dir.join(".reproit/findings").join(&id).join("capsule-id"))
                .unwrap(),
            "cap_test"
        );
    }

    #[test]
    fn from_journey_rejects_multi_actor() {
        let dir = std::env::temp_dir().join(format!("reproit-jfrom-ma-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("journeys")).unwrap();
        std::fs::write(
            dir.join("journeys").join("chat.yaml"),
            "actors: [alice, bob]\nsteps:\n  - actor: alice\n    do: tap:key:testid:send\n",
        )
        .unwrap();
        let err = prefix_actions(&loaded_at(&dir), "chat")
            .unwrap_err()
            .to_string();
        assert!(err.contains("multi-actor"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
