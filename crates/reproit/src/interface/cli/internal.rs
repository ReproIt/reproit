//! The implementation surface behind `reproit internal <sub>`.
//!
//! These are not vocabulary: engines, cloud plumbing, runner hosts, and the
//! agent bridge, invoked by reproit itself, by our own scripts, and by MCP
//! dispatch. A word appears in `reproit --help` only when a HUMAN needs it;
//! everything else lives here, one hidden multiplex instead of a hidden verb
//! per feature.

use super::args::{
    AuthStrategyArg, CloudAction, DebugAction, FuzzArgs, JourneyAction, ListState, ReproAction,
    ScanArgs, SkillsAction,
};
use clap::Subcommand;
use std::ffi::OsString;
use std::path::PathBuf;

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub(crate) enum InternalCmd {
    /// List local guards, blocked candidates, or confirmed production bugs.
    List {
        #[arg(long, value_enum, default_value = "guards")]
        state: ListState,
        /// Filter production bugs by message, identity, or bucket id.
        #[arg(long)]
        query: Option<String>,
    },
    /// Record a program's reads of the outside world into a process capsule
    /// (the general-program sibling of a backend capture). Requires
    /// REPROIT_PROCESS_SHIM to name a built runners/process-shim library.
    ProcessCapture {
        /// Where to write the capsule.
        #[arg(long = "out", value_name = "CAPSULE")]
        out: PathBuf,
        /// The command to run, after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Checkpoint a replaying process so a later replay restores the anchor
    /// and re-executes only the tail. Linux and criu only; the anchor
    /// accelerates investigating a failure and is never used to verify a fix,
    /// because a criu image carries the old binary's memory.
    ProcessAnchor {
        /// The process capsule to anchor. The anchor is written into it.
        #[arg(long = "capsule", value_name = "CAPSULE")]
        capsule: PathBuf,
        /// The command that re-executes the program, as `check --exec` takes.
        #[arg(long = "exec", value_name = "COMMAND")]
        exec: String,
        /// Directory to hold the checkpoint image.
        #[arg(long = "image", value_name = "DIR")]
        image: PathBuf,
        /// Checkpoint once the replaying program has produced this many lines
        /// of output, an observable stand-in for how far it has got.
        #[arg(long = "after-lines", value_name = "N")]
        after_lines: usize,
    },
    /// Restore a capsule's anchor and judge the tail, without replaying the
    /// head. Refuses, loudly, any anchor it cannot restore faithfully.
    ProcessRestore {
        /// The anchored process capsule.
        #[arg(long = "capsule", value_name = "CAPSULE")]
        capsule: PathBuf,
    },
    /// Print the HTTP surface a backend serves, read from its source, and
    /// write nothing. Works with no schema, no running service and no
    /// credentials, and reports each service of a monorepo separately. Where a
    /// schema is declared it also says where schema and source disagree.
    Surface,
    /// Reset Reproit state for this project. The default removes only
    /// regenerable state; --all also removes saved evidence and configuration.
    Reset {
        /// Remove all project-local Reproit state and reproit.yaml. This
        /// requires confirmation and never removes application source files.
        #[arg(long)]
        all: bool,
        /// Initialize the project again after --all completes.
        #[arg(long, requires = "all")]
        init: bool,
        /// Platform override for the initialization after reset.
        #[arg(long, requires = "init")]
        platform: Option<String>,
    },
    /// Check for or install the latest ReproIt CLI release.
    Update {
        /// Report whether an update is available without installing it.
        #[arg(long)]
        check: bool,
    },
    /// Advanced diagnostics. Normal scan/fuzz/check workflows maintain their
    /// internal app model automatically.
    Debug {
        #[command(subcommand)]
        action: DebugAction,
    },
    /// Collect a signed, encrypted offline support bundle from bounded files.
    Collect {
        /// Destination `.rpb` file. The command refuses to overwrite it.
        #[arg(long, short)]
        output: PathBuf,
        /// Product identity recorded in the immutable occurrence envelope.
        #[arg(long)]
        product: String,
        /// Component that observed the failure.
        #[arg(long)]
        component: String,
        /// Optional operating system or runtime platform.
        #[arg(long)]
        platform: Option<String>,
        /// Human failure observation. This is a source claim, not an oracle.
        #[arg(long)]
        summary: String,
        /// Evidence file to include. Repeat for logs, dumps, traces, or reports.
        #[arg(long = "artifact", value_name = "FILE")]
        artifacts: Vec<PathBuf>,
        /// Assert that every included artifact was redacted at source and may
        /// cross the collection boundary.
        #[arg(long)]
        exportable: bool,
        /// Retention policy label carried with the occurrence.
        #[arg(long, default_value = "support-30d")]
        retention_class: String,
    },
    /// Capture a known failure from a configured app, a command, or a signed
    /// offline support bundle.
    #[command(name = "capture", trailing_var_arg = true)]
    CaptureCommand {
        /// Signed offline support bundle to verify and import as an immutable
        /// occurrence.
        #[arg(long, value_name = "FILE")]
        bundle: Option<PathBuf>,
        /// Project identity used by Cloud grouping. Defaults to the checkout
        /// directory name.
        #[arg(long)]
        project: Option<String>,
        /// Component identity. Defaults to the executable name.
        #[arg(long)]
        component: Option<String>,
        /// Exact semantic identity asserted by a trusted command verifier.
        /// The command's exit status remains the replay matcher.
        #[arg(long)]
        identity: Option<String>,
        /// Stop the command after this many milliseconds.
        #[arg(long, default_value_t = 300_000)]
        timeout_ms: u64,
        /// Retain bounded stdout and stderr as local-only restricted artifacts.
        #[arg(long)]
        include_output: bool,
        /// Keep the capture on this machine even when Cloud credentials exist.
        #[arg(long)]
        local_only: bool,
        /// Capture an already-running configured application.
        #[arg(long)]
        attach: bool,
        /// Short description for an application demonstration.
        #[arg(long)]
        title: Option<String>,
        /// SDK action/state export for an application demonstration.
        #[arg(long)]
        actions_file: Option<PathBuf>,
        /// Record screen video with an application demonstration.
        #[arg(long)]
        record_video: bool,
        /// Review and push the application demonstration to Cloud.
        #[arg(long)]
        push: bool,
        /// Print the Cloud review URL instead of opening it.
        #[arg(long, requires = "push")]
        no_open: bool,
        /// Optional configured application sub-variant.
        #[arg(long)]
        kind: Option<String>,
        /// Command and arguments. Use `--` before command flags.
        #[arg(allow_hyphen_values = true, num_args = 0..)]
        command: Vec<OsString>,
    },
    /// Internal direct occurrence route used by `reproit occ_...`.
    #[command(name = "__occurrence")]
    Occurrence {
        reference: String,
        /// Download and validate the occurrence without executing it.
        #[arg(long)]
        no_run: bool,
    },
    /// Create a bug report by demonstrating the problem in the configured app.
    /// Repro It preserves the immutable original without claiming an unverified
    /// detector result.
    Create {
        /// Wait for a marked SDK capture, clean-run it, and derive a minimized
        /// repro. Unlike the default human capture, this requires verification.
        #[arg(
            long,
            conflicts_with_all = [
                "attach",
                "title",
                "actions_file",
                "record_video",
                "push",
                "no_open"
            ]
        )]
        cloud_tester: bool,
        /// Capture an app that is already running instead of launching the
        /// configured target. Screen capture is currently supported on macOS;
        /// structural actions require an SDK export via --actions-file.
        #[arg(long)]
        attach: bool,
        /// Short description stored with the original capture.
        #[arg(long)]
        title: Option<String>,
        /// Optional SDK export containing an action array, or an object with
        /// `actions` and `states`. It is copied into the immutable original.
        #[arg(long)]
        actions_file: Option<PathBuf>,
        /// Also record screen video as supporting evidence. Video is captured
        /// automatically when no structural action export is supplied.
        #[arg(long)]
        record_video: bool,
        /// Review and push the immutable original to Repro It Cloud after the
        /// demonstration stops.
        #[arg(long)]
        push: bool,
        /// Print the Cloud review link instead of opening a browser.
        #[arg(long, requires = "push")]
        no_open: bool,
        /// Cloud project for --cloud-tester. Defaults to the selected project.
        #[arg(long)]
        app: Option<String>,
        /// Stop waiting for a --cloud-tester SDK capture after this many seconds.
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
        /// Optional sub-variant, passed as --dart-define=PROMPT_KIND=<kind>
        #[arg(long)]
        kind: Option<String>,
    },
    /// Push a local human-created bug report to Repro It Cloud.
    Push {
        /// Immutable local capture id (cap_...).
        capture: String,
        /// Print the Cloud review link instead of opening a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Visual-regression the current capture against the committed baseline:
    /// per-pixel tolerance, ignore regions, and `--update` to accept the
    /// current capture. What is compared is driven by the `visual` section
    /// in reproit.yaml.
    Baseline {
        /// Accept the current capture as the new baseline.
        #[arg(long)]
        update: bool,
    },
    /// Advanced operations on an existing repro: `simplify` (verify + adopt a
    /// shorter action sequence) and `why` (rank suspect code for the failure).
    Repro {
        #[command(subcommand)]
        action: ReproAction,
    },
    /// Explain the immutable authority, evaluation, replay, minimization, and
    /// promotion decision for a finding or saved repro.
    Proof {
        /// Finding id, repro id, or saved repro alias.
        reference: String,
    },
    /// Replay every persisted backend finding against the live target and assert
    /// none still reproduces: a durable regression suite and batch proof-of-fix.
    /// Exits non-zero if any finding reproduces. Pass ids to verify only those.
    Verify {
        /// Finding ids to verify (default: all persisted findings).
        ids: Vec<String>,
        /// Write a JUnit report of held vs reproducing findings.
        #[arg(long)]
        junit: Option<PathBuf>,
        /// Delete findings whose contract the schema no longer asserts.
        #[arg(long)]
        prune_retracted: bool,
    },
    /// Accept one backend finding so the CI gate stops blocking on it, with a
    /// stated reason and an optional expiry. Unlike `check --update-baseline`,
    /// this accepts ONLY the findings you name; everything else keeps blocking.
    Accept {
        /// Finding ids to accept.
        ids: Vec<String>,
        /// Why this finding is being lived with. Required.
        #[arg(long, default_value = "")]
        reason: String,
        /// Date the acceptance lapses (YYYY-MM-DD). After it, the finding
        /// blocks again rather than staying silent forever.
        #[arg(long, value_name = "YYYY-MM-DD")]
        until: Option<String>,
        /// Drop the acceptance instead of adding it.
        #[arg(long)]
        remove: bool,
        /// Show what is currently accepted. Acceptances outlive a baseline, so
        /// a cleared history can still be carrying one.
        #[arg(long)]
        list: bool,
    },
    /// Internal route for the direct `reproit bkt_...` form.
    #[command(name = "__replay-bucket")]
    ReplayBucket {
        /// Production bucket/finding id (bkt_...).
        issue: String,
        /// Local alias (default: the production issue id).
        #[arg(long = "as", name = "name")]
        as_name: Option<String>,
        /// Download without running the local confirmation replay.
        #[arg(long)]
        no_run: bool,
        /// Save screen video as supporting evidence for the executed repro.
        #[arg(long, conflicts_with = "no_run")]
        record_video: bool,
        /// Scan recorded video for transient render glitches.
        #[arg(long, requires = "record_video")]
        flicker: bool,
        /// Cloud base URL (default: persisted login / $REPROIT_CLOUD_URL).
        #[arg(long)]
        cloud: Option<String>,
        /// Project key (default: persisted login / $REPROIT_CLOUD_KEY).
        #[arg(long)]
        key: Option<String>,
    },
    /// Internal route for the direct `reproit cap_...` form.
    #[command(name = "__capture")]
    OriginalCapture {
        /// Immutable original capture id (cap_...).
        capture: String,
        /// Open the original local video.
        #[arg(long, conflicts_with = "open")]
        watch: bool,
        /// Open the uploaded capture page in a browser.
        #[arg(long)]
        open: bool,
    },
    /// Update a production bug's lifecycle state. Example:
    /// `reproit triage bkt_... fixed --fixed-in-build 1.2.3`.
    Triage {
        issue: String,
        status: String,
        #[arg(long = "fixed-in-build")]
        fixed_in_build: Option<String>,
        #[arg(long)]
        assignee: Option<i64>,
    },
    /// Show a production bug's occurrence history and resolution state.
    Timeline { issue: String },
    /// List recent production confirmation and regression transitions.
    ResolutionEvents,
    /// Scan each reachable screen once for state-present oracle findings.
    /// Results retain an authoritative or specialist classification, but both
    /// are reported when their oracle predicate holds.
    /// `--record-video` saves quick audit clips; use
    /// `reproit <id> --record-video` for a fuzz repro.
    Scan(ScanArgs),
    /// Find confirmed, replayable bugs through deeper interaction exploration.
    /// ReproIt learns and refreshes its internal app model automatically.
    /// Stable, objective detectors are on by default. Specialist detectors are
    /// opt-in with `--only`; `--soak` runs the leak cycle.
    Fuzz(FuzzArgs),
    /// Serve reproit as an MCP server (stdio) for coding agents
    Mcp,
    /// Show the platform support matrix: which UI frameworks map to which
    /// introspection backend and capability source
    Platforms,
    /// Install the bundled coding-agent skills (the reproit playbook) into
    /// .claude/skills, so an agent drives reproit like an expert
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Configure and verify one test login. `auth <account>` replays the
    /// contract directly; `--discover` regenerates it first.
    Auth {
        account: String,
        #[arg(long, value_enum)]
        strategy: Option<AuthStrategyArg>,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        phone: Option<String>,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        otp: Option<String>,
        #[arg(long)]
        totp_secret: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        user_id: Option<String>,
        #[arg(long)]
        validate_text: Option<String>,
        #[arg(long)]
        no_discover: bool,
        /// Rebuild the login contract from exploration before verifying it.
        #[arg(long, conflicts_with = "no_discover")]
        discover: bool,
    },
    /// Run and manage scripted journeys (declarative YAML paths).
    #[command(
        after_help = "Run:     reproit journey <name>\nCreate:  reproit journey create \
                      <name>\nList:    reproit journey list"
    )]
    Journey {
        #[command(subcommand)]
        action: JourneyAction,
    },
    /// Capture store/marketing screenshots: drive a tour (a journey) across
    /// locales and devices into a journey-led layout (or your own
    /// --path-template). Reuses the SHOOT capture machinery; one
    /// locale-invariant tour covers every locale.
    Screenshots {
        /// Tour to drive (a journey file stem). Defaults to screenshots.tour.
        tour: Option<String>,
        /// Output root (default: screenshots.out, else `screenshots/`).
        #[arg(long)]
        out: Option<String>,
        /// Comma-separated locales (e.g. de,ar,ja). Overrides config when set.
        #[arg(long)]
        locale: Option<String>,
        /// Comma-separated platforms/engines to fan out (e.g. ios,android).
        #[arg(long)]
        target: Option<String>,
        /// Comma-separated device names/ids. Overrides config when set.
        #[arg(long)]
        device: Option<String>,
        /// Skip the cross-screen verification gate (it is on by default).
        #[arg(long)]
        no_verify: bool,
        /// Per-shot directory template, overriding the auto layout.
        /// Placeholders: {journey} {platform} {locale} {device}.
        /// Example: "{locale}/{device}".
        #[arg(long)]
        path_template: Option<String>,
    },
    /// Import an offline `.rpb` support bundle or a flow from another tool.
    Import {
        /// Bundle path, or source tool (`maestro`) when a second path follows.
        source: String,
        /// Source flow file for tool imports.
        path: Option<PathBuf>,
        /// Journey name (default: the source file stem).
        #[arg(long)]
        name: Option<String>,
        /// Write the journey here (default: stdout).
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// Internal Cloud and CI plumbing. Human Cloud workflows are top-level.
    #[command(name = "__cloud-internal")]
    Cloud {
        #[command(subcommand)]
        action: CloudAction,
    },
    /// (internal) PTY-driven terminal-UI runner; spawned by the tui backend
    #[command(name = "__tui")]
    TuiRun,
    /// (internal) Windows UI Automation runner; spawned by the desktop-uia
    /// backend
    #[command(name = "__uia")]
    UiaRun,
    /// (internal) Linux AT-SPI runner; spawned by the desktop-atspi backend
    #[command(name = "__atspi")]
    AtspiRun,
    /// (internal) Replay one explicit Vitest assertion as an authored contract.
    #[command(name = "__vitest-contract")]
    VitestContract {
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        test_path: String,
        #[arg(long)]
        test_name: String,
        #[arg(long)]
        pnpm_version: String,
    },
    /// Refresh the release cache without delaying the calling command.
    #[command(name = "__update-check")]
    UpdateCheck,
    /// Verify a signed offline support bundle (manifest, payload digest, and
    /// signature) without decrypting artifacts.
    Inspect {
        /// The `.rpb` support bundle to verify.
        #[arg(value_name = "BUNDLE")]
        reference: String,
    },
}
