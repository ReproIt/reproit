use super::*;
use clap::CommandFactory;

#[test]
fn clap_schema_is_internally_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn quiet_help_states_that_human_output_is_suppressed() {
    let help = Cli::command().render_long_help().to_string();
    assert!(
        help.contains("Suppress human-readable output"),
        "quiet is silent, so its help must not promise minimal logs:\n{help}"
    );
}

#[test]
fn every_documented_ci_invocation_matches_the_current_parser() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let contracts = std::fs::read_to_string(root.join("validation/release/ci-invocations.txt"))
        .expect("read documented CI invocations");
    let mut parsed = 0usize;
    for line in contracts.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let args = line.split_ascii_whitespace().collect::<Vec<_>>();
        Cli::try_parse_from(&args)
            .unwrap_or_else(|error| panic!("documented CI invocation failed: {line}\n{error}"));
        parsed += 1;
    }
    assert_eq!(parsed, 3, "update the expected bounded invocation count");

    let command = Cli::command();
    let fuzz = command.find_subcommand("fuzz").expect("fuzz command");
    let known_flags = fuzz
        .get_arguments()
        .filter_map(clap::Arg::get_long)
        .collect::<std::collections::BTreeSet<_>>();
    for relative in ["action.yml", ".github/workflows/reproit-pr.yml"] {
        let document = std::fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        let invocation = documented_fuzz_invocation(&document)
            .unwrap_or_else(|| panic!("no fuzz invocation in {relative}"));
        for token in invocation.split_ascii_whitespace() {
            let Some(flag) = token.trim_end_matches(['\\', ')']).strip_prefix("--") else {
                continue;
            };
            assert!(
                known_flags.contains(flag),
                "{relative} uses unknown fuzz flag --{flag}"
            );
        }
    }
}

fn documented_fuzz_invocation(document: &str) -> Option<&str> {
    let start = document
        .find("args=( fuzz")
        .or_else(|| document.find("./target/release/reproit fuzz"))?;
    let tail = &document[start..];
    let end = tail.find("\n        if ").or_else(|| tail.find("\n\n"));
    Some(&tail[..end.unwrap_or(tail.len())])
}

#[test]
fn changed_check_defaults_the_base_and_stays_suite_only() {
    let cli = Cli::parse_args(["reproit", "check", "--changed"]);
    assert!(matches!(
        cli.command,
        Cmd::Check {
            repro: None,
            changed: Some(ref base),
            ..
        } if base == "HEAD^"
    ));

    let cli = Cli::parse_args(["reproit", "check", "--changed", "origin/main"]);
    assert!(matches!(
        cli.command,
        Cmd::Check {
            repro: None,
            changed: Some(ref base),
            ..
        } if base == "origin/main"
    ));
}

#[test]
fn inspect_accepts_a_saved_alias_or_production_bucket() {
    let alias = Cli::parse_args(["reproit", "inspect", "@checkout-crash"]);
    assert!(matches!(
        alias.command,
        Cmd::Inspect { ref reference, offline: false } if reference == "@checkout-crash"
    ));

    let bucket = Cli::parse_args(["reproit", "inspect", "bkt_deadbeef0001"]);
    assert!(matches!(
        bucket.command,
        Cmd::Inspect { ref reference, offline: false } if reference == "bkt_deadbeef0001"
    ));

    let capture = Cli::parse_args(["reproit", "inspect", "capture.json", "--offline"]);
    assert!(matches!(
        capture.command,
        Cmd::Inspect { ref reference, offline: true } if reference == "capture.json"
    ));
}

#[test]
fn reset_modes_parse_with_explicit_destructive_dependencies() {
    let cli = Cli::parse_args(["reproit", "reset"]);
    assert!(matches!(
        cli.command,
        Cmd::Reset {
            all: false,
            init: false,
            platform: None,
        }
    ));

    let cli = Cli::parse_args(["reproit", "reset", "--all", "--init", "--platform", "web"]);
    assert!(matches!(
        cli.command,
        Cmd::Reset {
            all: true,
            init: true,
            platform: Some(ref platform),
        } if platform == "web"
    ));

    assert!(Cli::try_parse_from(["reproit", "reset", "--init"]).is_err());
    assert!(Cli::try_parse_from(["reproit", "reset", "--platform", "web"]).is_err());
}

#[test]
fn parser_boundary_applies_direct_bug_id_rewriting() {
    let cli = Cli::parse_args(["reproit", "--json", "fnd_deadbeef0001"]);
    assert!(cli.json);
    assert!(matches!(
        cli.command,
        Cmd::Check {
            repro: Some(ref id),
            ..
        } if id == "fnd_deadbeef0001"
    ));

    let cli = Cli::parse_args(["reproit", "@checkout-crash", "--record-video"]);
    assert!(matches!(
        cli.command,
        Cmd::Check {
            repro: Some(ref alias),
            record_video: true,
            changed: None,
            ..
        } if alias == "checkout-crash"
    ));

    let cli = Cli::parse_args(["reproit", "bkt_deadbeef0001", "--record-video"]);
    assert!(matches!(
        cli.command,
        Cmd::ReplayBucket {
            ref issue,
            record_video: true,
            ..
        } if issue == "bkt_deadbeef0001"
    ));

    let cli = Cli::parse_args(["reproit", "cap_deadbeef00000000", "--watch"]);
    assert!(matches!(
        cli.command,
        Cmd::OriginalCapture {
            ref capture,
            watch: true,
            ..
        } if capture == "cap_deadbeef00000000"
    ));
}

/// `check <reference>` takes a positional so a captured-production payload
/// file routes through the same resolution as `--repro-id`; the two forms are
/// mutually exclusive.
#[test]
fn check_accepts_a_positional_capture_reference() {
    let cli = Cli::try_parse_from(["reproit", "check", "capture.json"]).unwrap();
    assert!(matches!(
        cli.command,
        Cmd::Check {
            ref reference,
            repro: None,
            ..
        } if reference.as_deref() == Some("capture.json")
    ));
    assert!(Cli::try_parse_from([
        "reproit",
        "check",
        "--repro-id",
        "fnd_deadbeef0001",
        "capture.json"
    ])
    .is_err());
}

#[test]
fn removed_compatibility_commands_are_not_parseable() {
    for args in [
        vec!["reproit", "run"],
        vec!["reproit", "guard"],
        vec!["reproit", "save"],
        vec!["reproit", "repros"],
        vec!["reproit", "repro", "list"],
        vec!["reproit", "candidates"],
        vec!["reproit", "bugs"],
        vec!["reproit", "pull", "bkt_deadbeef0001"],
        vec!["reproit", "replay", "fnd_deadbeef0001"],
        vec!["reproit", "record"],
        vec!["reproit", "scan", "--record"],
        vec!["reproit", "fuzz", "--shrink"],
        vec!["reproit", "cloud"],
        vec!["reproit", "cloud", "login"],
        vec!["reproit", "cloud", "pull"],
        vec!["reproit", "cloud", "reproduce"],
        vec!["reproit", "init", "--learn"],
        vec!["reproit", "auth", "verify", "alice"],
        vec!["reproit", "auth", "discover", "alice"],
        vec![
            "reproit",
            "check",
            "--repro-id",
            "fnd_deadbeef0001",
            "--flicker",
        ],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }

    let cli = Cli::try_parse_from(["reproit", "verify", "fnd_deadbeef0001"]).unwrap();
    assert!(matches!(
        cli.command,
        Cmd::Verify {
            ids,
            junit: None,
            prune_retracted: false,
        } if ids == ["fnd_deadbeef0001"]
    ));

    let cli = Cli::try_parse_from(["reproit", "verify", "--prune-retracted"]).unwrap();
    assert!(matches!(
        cli.command,
        Cmd::Verify {
            prune_retracted: true,
            ..
        }
    ));

    let cli = Cli::try_parse_from(["reproit", "journey", "checkout"]).unwrap();
    assert!(matches!(
        cli.command,
        Cmd::Journey {
            action: JourneyAction::Run(args)
        } if args == ["checkout"]
    ));

    let cli = Cli::try_parse_from([
        "reproit",
        "__cloud-internal",
        "__replay-dispatch",
        "--app",
        "acme-store",
        "--bucket",
        "bkt_deadbeef0001",
        "--as",
        "bkt_deadbeef0001",
        "--run",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Cmd::Cloud {
            action: CloudAction::ReplayDispatch { .. }
        }
    ));
}

#[test]
fn hosted_login_needs_no_key_or_project_argument() {
    let cli = Cli::try_parse_from(["reproit", "login"]).unwrap();
    assert!(matches!(
        cli.command,
        Cmd::Login {
            cloud: None,
            key: None,
        }
    ));
    assert!(Cli::try_parse_from(["reproit", "login", "--app", "acme-store"]).is_err());
}

#[test]
fn create_is_distinct_from_video_and_push_is_explicit() {
    let cli = Cli::try_parse_from([
        "reproit",
        "create",
        "--attach",
        "--title",
        "menu bug",
        "--record-video",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Cmd::Create {
            cloud_tester: false,
            attach: true,
            title: Some(ref title),
            record_video: true,
            ..
        } if title == "menu bug"
    ));

    let cli = Cli::try_parse_from(["reproit", "create", "--cloud-tester"]).unwrap();
    assert!(matches!(
        cli.command,
        Cmd::Create {
            cloud_tester: true,
            attach: false,
            ..
        }
    ));
    assert!(Cli::try_parse_from(["reproit", "create", "--cloud-tester", "--attach"]).is_err());
    assert!(Cli::try_parse_from(["reproit", "create", "--cloud-tester", "--push"]).is_err());

    let cli = Cli::try_parse_from(["reproit", "create", "--push", "--no-open"]).unwrap();
    assert!(matches!(
        cli.command,
        Cmd::Create {
            push: true,
            no_open: true,
            ..
        }
    ));

    let cli = Cli::try_parse_from(["reproit", "push", "cap_deadbeef00000000"]).unwrap();
    assert!(matches!(cli.command, Cmd::Push { .. }));
}

#[test]
fn primary_help_contains_only_the_outcome_oriented_surface() {
    let visible = Cli::command()
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(|command| command.get_name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        visible,
        ["init", "find", "list", "check", "capture", "keep", "doctor", "login"]
    );
}

#[test]
fn capture_routes_ui_and_command_sources_through_one_public_verb() {
    let ui = Cli::try_parse_from([
        "reproit",
        "capture",
        "--attach",
        "--title",
        "menu bug",
        "--record-video",
    ])
    .unwrap();
    assert!(matches!(
        ui.command,
        Cmd::CaptureCommand {
            attach: true,
            title: Some(ref title),
            record_video: true,
            ref command,
            ..
        } if title == "menu bug" && command.is_empty()
    ));

    let command = Cli::try_parse_from([
        "reproit",
        "capture",
        "--include-output",
        "--identity",
        "doctor:blank-backend-project-root",
        "--",
        "sh",
        "-c",
        "exit 7",
    ])
    .unwrap();
    assert!(matches!(
        command.command,
        Cmd::CaptureCommand {
            include_output: true,
            ref identity,
            command,
            ..
        } if command == ["sh", "-c", "exit 7"]
            && identity.as_deref() == Some("doctor:blank-backend-project-root")
    ));

    let bundle =
        Cli::try_parse_from(["reproit", "capture", "--bundle", "customer-case.rpb"]).unwrap();
    assert!(matches!(
        bundle.command,
        Cmd::CaptureCommand {
            bundle: Some(ref path),
            ref command,
            ..
        } if path == std::path::Path::new("customer-case.rpb") && command.is_empty()
    ));
}

#[test]
fn find_and_list_parse_as_the_primary_discovery_and_inventory_commands() {
    let find = Cli::try_parse_from([
        "reproit",
        "find",
        "https://example.test",
        "--exhaustive",
        "--runs",
        "4",
    ])
    .unwrap();
    assert!(matches!(
        find.command,
        Cmd::Find(FindArgs {
            target: Some(ref target),
            exhaustive: true,
            runs: Some(4),
            ..
        }) if target == "https://example.test"
    ));

    let list = Cli::try_parse_from(["reproit", "list", "--state", "candidates"]).unwrap();
    assert!(matches!(
        list.command,
        Cmd::List {
            state: ListState::Candidates,
            query: None,
        }
    ));
}
