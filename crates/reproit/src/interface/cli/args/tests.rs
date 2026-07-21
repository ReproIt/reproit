use super::*;
use clap::CommandFactory;

#[test]
fn clap_schema_is_internally_consistent() {
    Cli::command().debug_assert();
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
        Cmd::Capture {
            ref capture,
            watch: true,
            ..
        } if capture == "cap_deadbeef00000000"
    ));
}

#[test]
fn removed_compatibility_commands_are_not_parseable() {
    for args in [
        vec!["reproit", "run"],
        vec!["reproit", "guard"],
        vec!["reproit", "save"],
        vec!["reproit", "pull", "bkt_deadbeef0001"],
        vec!["reproit", "check", "fnd_deadbeef0001"],
        vec!["reproit", "check", "checkout"],
        vec!["reproit", "verify", "fnd_deadbeef0001"],
        vec!["reproit", "replay", "fnd_deadbeef0001"],
        vec!["reproit", "record"],
        vec!["reproit", "scan", "--record"],
        vec!["reproit", "fuzz", "--shrink"],
        vec!["reproit", "cloud"],
        vec!["reproit", "cloud", "login"],
        vec!["reproit", "cloud", "pull"],
        vec!["reproit", "cloud", "reproduce"],
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
