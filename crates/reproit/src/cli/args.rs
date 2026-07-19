//! Clap schema for the Reproit command-line interface.

use super::context::Ctx;
use super::rewrite;
use crate::{config, skills, VERSION};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "reproit",
    version = VERSION,
    about = "Find UI failures and keep every confirmed bug reproducible"
)]
pub(crate) struct Cli {
    /// Path to reproit.yaml (default: search cwd and ancestors)
    #[arg(long, global = true)]
    pub(crate) config: Option<PathBuf>,
    /// Machine-readable output (CI, scripts, the MCP bridge)
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Minimal output (CI logs)
    #[arg(long, global = true)]
    pub(crate) quiet: bool,
    /// Never prompt (non-interactive / CI)
    #[arg(long, global = true)]
    pub(crate) yes: bool,
    #[command(subcommand)]
    pub(crate) command: Cmd,
}

impl Cli {
    pub(crate) fn ctx(&self) -> Ctx {
        Ctx {
            json: self.json,
            quiet: self.quiet,
            yes: self.yes,
        }
    }
}

// A clap subcommand enum: variants carry their flags by value and are
// instantiated once at startup, so the size spread between variants is
// irrelevant (and unavoidable for a rich CLI).
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Detect the current app and create the smallest working reproit setup.
    /// After initialization, use `reproit scan` or `reproit fuzz`.
    Init {
        /// Running web app to initialize. A URL always selects the web UI
        /// workflow.
        #[arg(value_name = "URL")]
        target: Option<String>,
        /// Platform override: flutter | web | rn | android | backend.
        #[arg(long)]
        platform: Option<String>,
        /// Replace existing generated scaffold files.
        #[arg(long)]
        force: bool,
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
    /// Run the saved regression suite and classify each: pass (0) / fail (1) /
    /// flaky (2) / stale (3). To reproduce one bug, run `reproit <id>`.
    /// (Annotated video is `record`; the visual oracle is `baseline`.)
    Check {
        /// Internal direct-id route. Users run `reproit <id>`.
        #[arg(long = "repro-id", hide = true)]
        repro: Option<String>,
        /// Number of concurrent devices (multi-actor)
        #[arg(long, default_value_t = 1)]
        devices: usize,
        /// Optional sub-variant, passed as --dart-define=PROMPT_KIND=<kind>
        #[arg(long)]
        kind: Option<String>,
        /// Override gate.runs from config (gate-style repeats)
        #[arg(long)]
        runs: Option<u32>,
        /// Write JUnit XML results to this path (for CI)
        #[arg(long)]
        junit: Option<PathBuf>,
        /// Treat a quarantined (reported, non-blocking) repro's failure as
        /// blocking too, so it gates the exit code like a required repro.
        #[arg(long)]
        strict: bool,
        /// Comma-separated locale list to check across (e.g. de,ar,ja). Each
        /// locale replays with REPROIT_LOCALE set; results are reported per
        /// locale and a locale-specific failure (fails in one locale, passes in
        /// another) is noted. Unset = the app default.
        #[arg(long)]
        locale: Option<String>,
        /// Device target: ios|android|web|all. Interactive picker when omitted
        /// on a TTY and not --yes.
        #[arg(long)]
        target: Option<String>,
        /// Specific device name/id to route to (else the interactive picker).
        #[arg(long)]
        device: Option<String>,
    },
    /// Capture a bug exactly as a person experiences it. With no id Repro It
    /// launches the configured app, records until you stop, and preserves the
    /// immutable original without requiring an oracle. Pass a repro id to film
    /// an existing deterministic repro instead.
    Record {
        /// Existing repro to film: pending fnd_..., saved rep_..., or alias.
        /// Omit it to create a new human-authored original capture.
        repro: Option<String>,
        /// Preserve the legacy SDK/Cloud tester workflow: wait for a marked SDK
        /// capture, clean-replay it, and derive a minimized repro. Unlike the
        /// default human capture, this requires verification.
        #[arg(
            long,
            conflicts_with_all = [
                "attach",
                "title",
                "actions_file",
                "no_video",
                "upload",
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
        /// Do not record screen video. Useful when the SDK export contains all
        /// evidence or screen-recording permission is unavailable.
        #[arg(long)]
        no_video: bool,
        /// Review and upload the immutable original to Repro It Cloud after
        /// recording stops.
        #[arg(long)]
        upload: bool,
        /// Print the Cloud review link instead of opening a browser.
        #[arg(long, requires = "upload")]
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
        /// Number of concurrent devices (multi-actor)
        #[arg(long, default_value_t = 1)]
        devices: usize,
        /// Reuse the previous build (--no-build). Only valid when the last
        /// build was this same journey.
        #[arg(long)]
        warm: bool,
        /// Capture SHOOT screenshots into this directory
        #[arg(long)]
        shots_dir: Option<PathBuf>,
        /// Drive in profile mode (AOT) for representative perf
        #[arg(long)]
        profile: bool,
        /// After recording, scan the video for transient render glitches
        /// (intra-run flicker: a frame that diverges then snaps back).
        /// No baseline needed.
        #[arg(long)]
        flicker: bool,
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
    /// Keep a repro from the latest fuzz run in the committed suite. The
    /// store dir is the repro's CONTENT HASH (.reproit/repros/<id>/), stable
    /// across machines and self-deduping. `--as` assigns a human alias.
    Keep {
        /// Finding id (dirname) from the latest fuzz run. Uses the sole finding
        /// if omitted, else lists choices.
        id: Option<String>,
        /// Optional human label for the kept repro.
        #[arg(long = "as", name = "name")]
        as_name: Option<String>,
        /// Land the repro `required` (blocking) immediately instead of
        /// quarantined-until-first-green.
        #[arg(long)]
        strict: bool,
    },
    /// Advanced operations on an existing repro: `simplify` (verify + adopt a
    /// shorter action sequence) and `why` (rank suspect code for the failure).
    Repro {
        #[command(subcommand)]
        action: ReproAction,
    },
    /// List saved local repros under .reproit/repros/.
    Repros,
    /// List confirmed production bugs, impact-ranked, for the project selected
    /// during `reproit login`.
    Bugs {
        /// Filter by message, signature, or bucket id.
        query: Option<String>,
    },
    /// Internal route for the direct `reproit bkt_...` form.
    #[command(name = "__replay-bucket", hide = true)]
    ReplayBucket {
        /// Production bucket/finding id (bkt_...).
        issue: String,
        /// Local alias (default: the production issue id).
        #[arg(long = "as", name = "name")]
        as_name: Option<String>,
        /// Download without running the local confirmation replay.
        #[arg(long)]
        no_run: bool,
        /// Cloud base URL (default: persisted login / $REPROIT_CLOUD_URL).
        #[arg(long)]
        cloud: Option<String>,
        /// Project key (default: persisted login / $REPROIT_CLOUD_KEY).
        #[arg(long)]
        key: Option<String>,
    },
    /// Internal route for the direct `reproit cap_...` form.
    #[command(name = "__capture", hide = true)]
    Capture {
        /// Immutable original capture id (cap_...).
        capture: String,
        /// Open the original local video.
        #[arg(long, visible_alias = "record", conflicts_with_all = ["upload", "open"])]
        watch: bool,
        /// Review and upload the immutable original to Repro It Cloud.
        #[arg(long, conflicts_with = "open")]
        upload: bool,
        /// Open the uploaded capture page in a browser.
        #[arg(long)]
        open: bool,
        /// Print a browser URL instead of opening it.
        #[arg(long, requires = "upload")]
        no_open: bool,
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
    /// Match a bug report to a confirmed production bug.
    Diagnose {
        report: String,
        #[arg(long)]
        run: bool,
    },
    /// List recent production confirmation and regression transitions.
    ResolutionEvents,
    /// Open a repro's recorded video in your default player. Recordings live
    /// under .reproit/recordings/repro/ (gitignored); make one with `record
    /// <id>`.
    Watch {
        /// The repro to watch (id or alias).
        repro: String,
    },
    /// Generate app-source fix patches from a run's findings (working-tree
    /// diff for review; requires a write-capable CLI provider). Agent-only:
    /// reached over MCP / a BYO-key path, not part of the public surface.
    #[command(hide = true)]
    Fix {
        /// Run directory name under evidence.outDir (default: latest run)
        run: Option<String>,
    },
    /// Triage a run's evidence bundle via the configured LLM provider.
    /// Agent-only: reached over MCP, not part of the public surface.
    #[command(hide = true)]
    Analyze {
        /// Run directory name under evidence.outDir (default: latest run)
        run: Option<String>,
    },
    /// List the simulators reproit manages (by configured name prefix)
    Devices,
    /// Scan each reachable screen once for state-present oracle findings.
    /// Results retain an authoritative or specialist classification, but both
    /// are reported when their oracle predicate holds.
    /// `--record` saves quick audit clips; use `record <id>` for a fuzz repro.
    Scan {
        /// What to scan. An OpenAPI, GraphQL introspection, or protobuf schema
        /// checks read-only service operations; use `--service` when the schema
        /// has no local server URL. An A2UI JSON/JSONL stream runs against the
        /// official React and Lit renderers. A URL (https://app.com) runs
        /// zero-config against that deployed app; a terminal EXECUTABLE (e.g.
        /// `lazygit`, `htop`, or a path) runs zero-config in a PTY; any other
        /// value scopes the crawl to that alias/node in a reproit.yaml.
        #[arg(value_name = "TARGET")]
        target_arg: Option<String>,
        /// Disposable backend service URL for an OpenAPI, GraphQL, or protobuf
        /// target. Overrides the schema server URL.
        #[arg(long, value_name = "URL")]
        service: Option<String>,
        /// Coverage budget: how many actions the crawl may take to reach
        /// screens.
        #[arg(long, default_value_t = 60)]
        budget: u32,
        /// Force the simulator tier (default: headless / web).
        #[arg(long)]
        sim: bool,
        /// After the crawl, record every distinct reported finding. Visually
        /// localizable findings are boxed; the rest are diagnostic clips.
        #[arg(long)]
        record: bool,
        /// Where the `--record` clips land (default:
        /// .reproit/recordings/scan/<scan-run>/).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Extra HTTP header injected into the browser context, `"Name:
        /// value"`. Repeatable. Use it to pass a WAF clearance cookie
        /// (`--header "Cookie: cf_clearance=..."`), an auth bearer, or a
        /// preview token so a challenge-fronted or authed target is
        /// reachable.
        #[arg(long = "header", value_name = "NAME: VALUE")]
        header: Vec<String>,
    },
    /// Find confirmed, replayable bugs through deeper interaction exploration.
    /// ReproIt learns and refreshes its internal app model automatically.
    /// Stable, objective detectors are on by default. Specialist detectors are
    /// opt-in with `--only`; `--soak` runs the leak cycle.
    Fuzz {
        /// What to fuzz (optional). An OpenAPI, GraphQL introspection, or
        /// protobuf schema drives schema-valid service calls; use `--service`
        /// for the disposable target and `--reset` for exact stateful replay.
        /// An A2UI JSON/JSONL stream is checked across the official React and
        /// Lit renderers with schema-valid mutations. A PLAYWRIGHT TEST file
        /// (`reproit fuzz your-test.spec.ts`) is run under trace; reproit
        /// replays its actions to reach its deep state, then fuzzes
        /// onward for the bugs the test never covered (you wrote the
        /// test; reproit finds the bugs you didn't). A URL (https://app.com) is
        /// auto-detected and runs zero-config
        /// against that deployed app; a terminal EXECUTABLE (e.g. `lazygit`, or
        /// a path) runs zero-config in a PTY; no reproit.yaml needed
        /// for any of these. Any other value scopes the hunt to that
        /// alias/node.
        #[arg(value_name = "TARGET")]
        target_arg: Option<String>,
        /// Disposable backend service URL for an OpenAPI, GraphQL, or protobuf
        /// target. Remote mutating fuzz still requires `--yes`.
        #[arg(long, value_name = "URL")]
        service: Option<String>,
        /// Same-origin reset endpoint used before confirmation, shrinking, and
        /// replay of stateful backend findings.
        #[arg(long, value_name = "URL")]
        reset: Option<String>,
        /// Explorer journey to drive (resolves like any journey target)
        #[arg(long, default_value = "explore")]
        journey: String,
        /// First seed; runs use seed, seed+1, ...
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Number of seeds to try
        #[arg(long, default_value_t = 3)]
        runs: u32,
        /// Actions per walk
        #[arg(long, default_value_t = 40)]
        budget: u32,
        /// Deprecated compatibility flag: confirmation now minimizes by
        /// default.
        #[arg(long, hide = true)]
        shrink: bool,
        /// Skip the clean-session confirmation/minimization replay. Unconfirmed
        /// observations are candidates and should not be alerted or saved.
        #[arg(long)]
        no_confirm: bool,
        /// Collect findings across the whole seed budget instead of stopping at
        /// the first, and group them by crash signature into UNIQUE bugs (the
        /// deduped "fuzz and fix" work-list). Slower (it keeps hunting).
        #[arg(long)]
        all: bool,
        /// Coverage-guided: deterministic path to the least-visited state,
        /// then the seeded walk explores from that frontier
        #[arg(long)]
        frontier: bool,
        /// Fuzz FROM a journey: replay this journey (a name like any journey
        /// target, or a path to a .yaml, e.g. one just written by
        /// `reproit import`) to its end state, then branch the seeded walk
        /// outward from there. Turns a recorded/imported flow into a launchpad
        /// for the bugs it never covered. Takes precedence over --frontier.
        #[arg(long)]
        from: Option<String>,
        /// A/B control: uniform-random pick + fixed budget (disables the
        /// inverse-visit-count scoring and power schedule)
        #[arg(long)]
        uniform: bool,
        /// Production-seeded fuzzing: path to a JSON array of real user action
        /// paths (e.g. from SDK telemetry). The fuzzer branches outward from
        /// them instead of always launching cold.
        #[arg(long)]
        seeds: Option<String>,
        /// Seeds per drive session (batch-seeds-per-session). One install +
        /// launch + connect is amortized across this many seeds, resetting app
        /// state between them. Default 0 = all `runs` in ONE session. Use
        /// `--batch 1` for a one-drive-per-seed A/B control.
        #[arg(long, default_value_t = 0)]
        batch: u32,
        /// Print a per-phase wall-clock breakdown (sim ensure, reset, build,
        /// launch->ready, walk, teardown) for each drive session.
        #[arg(long)]
        profile_timing: bool,
        /// Force the SIMULATOR tier (flutter drive on an iOS sim). Default is
        /// the HEADLESS tier (flutter test, no sim, runs in seconds on Linux).
        /// Use --sim for jank/runtime/keyboard/plugin oracles or repro video.
        #[arg(long)]
        sim: bool,
        /// On a headless finding, replay the minimized repro ONCE on the
        /// simulator to confirm on the real runtime (default off).
        #[arg(long)]
        confirm_on_sim: bool,
        /// Cloud base URL. When set (with --app), a finding triggers the
        /// delivery pipeline: annotate + upload the minimized-repro clip, then
        /// emit the PR-comment markdown.
        #[arg(long)]
        cloud: Option<String>,
        /// Cloud app id the finding's evidence attaches to (with --cloud).
        #[arg(long)]
        app: Option<String>,
        /// Cloud bucket id the finding's evidence attaches to (bkt_...).
        #[arg(long)]
        bucket: Option<String>,
        /// Actually POST the PR comment (needs GITHUB_TOKEN + repo + PR);
        /// otherwise the pipeline emits the comment markdown as a dry-run.
        #[arg(long)]
        post_comment: bool,
        /// Leak oracle: repeat a reversible cycle and watch heap growth per
        /// cycle. Use with --cycle / --repeats.
        #[arg(long)]
        soak: bool,
        /// (--soak) semicolon-separated actions, e.g.
        /// "tap:Compose;tap:New post;tap:Publish"
        #[arg(long)]
        cycle: Option<String>,
        /// (--soak) how many times to repeat the cycle
        #[arg(long, default_value_t = 15)]
        repeats: u32,
        /// Reuse the previous build (--no-build). Applies to --soak.
        #[arg(long)]
        warm: bool,
        /// Target engines/platforms. When set to web engines (e.g.
        /// "chromium,firefox,webkit"), runs the cross-engine differential
        /// (divergence oracle). The first engine is reference.
        #[arg(long)]
        target: Option<String>,
        /// (--target web engines) URL under test (defaults to app.url)
        #[arg(long)]
        url: Option<String>,
        /// (--target web engines) run headless (default headed, so the real
        /// GPU compositor runs)
        #[arg(long)]
        headless: bool,
        /// Comma-separated locale list to fuzz across (e.g. de,ar,ja). Each
        /// locale runs the flow once with REPROIT_LOCALE set; findings are
        /// tagged with their locale and locale-specific i18n findings are
        /// noted. Unset = the app default (behavior unchanged).
        #[arg(long)]
        locale: Option<String>,
        /// Restrict to oracle categories. This also opts into preview or
        /// experimental detectors that are not part of the stable default.
        #[arg(long)]
        only: Option<String>,
        /// Exclude these oracle categories. Applied after --only.
        #[arg(long = "no")]
        no_oracles: Option<String>,
        /// Specific device name/id to route to (else the interactive picker
        /// when a TTY is present and --target is a platform).
        #[arg(long)]
        device: Option<String>,
    },
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
    /// Diagnose local setup: config, runner deps, app URL, and cloud
    /// credentials.
    Doctor,
    /// Configure, discover, and verify one test login. Existing accounts need
    /// only `reproit auth <account>`; flags create/update an account for CI.
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
    /// Import a flow from another tool into a reproit journey (switching cost
    /// ~0). Currently supports Maestro: `reproit import maestro flow.yaml`.
    Import {
        /// Source tool. Currently: maestro.
        tool: String,
        /// Path to the source flow file.
        path: PathBuf,
        /// Journey name (default: the source file stem).
        #[arg(long)]
        name: Option<String>,
        /// Write the journey here (default: stdout).
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
    /// Internal Cloud and CI plumbing. Human Cloud workflows are top-level.
    #[command(name = "__cloud-internal", hide = true)]
    Cloud {
        #[command(subcommand)]
        action: CloudAction,
    },
    /// Sign in to ReproIt Cloud in your browser, then discover and select a
    /// project. Hosted Cloud is assumed; --cloud is only for a self-hosted
    /// deployment.
    Login {
        /// Cloud base URL (default: https://cloud.reproit.com).
        #[arg(long)]
        cloud: Option<String>,
        /// Account/project key for noninteractive CI (default:
        /// $REPROIT_CLOUD_KEY).
        #[arg(long)]
        key: Option<String>,
    },
    /// (internal) PTY-driven terminal-UI runner; spawned by the tui backend
    #[command(name = "__tui", hide = true)]
    TuiRun,
    /// (internal) Windows UI Automation runner; spawned by the desktop-uia
    /// backend
    #[command(name = "__uia", hide = true)]
    UiaRun,
    /// (internal) Linux AT-SPI runner; spawned by the desktop-atspi backend
    #[command(name = "__atspi", hide = true)]
    AtspiRun,
    /// (internal) Replay one explicit Vitest assertion as an authored contract.
    #[command(name = "__vitest-contract", hide = true)]
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
    #[command(name = "__update-check", hide = true)]
    UpdateCheck,
}

#[derive(Subcommand)]
pub(crate) enum DebugAction {
    /// Inspect or force maintenance of reproit's internal app model.
    Map {
        #[command(subcommand)]
        action: Option<MapAction>,
    },
}

/// `repro` subcommands: advanced operations that act on an existing repro.
#[derive(Subcommand)]
pub(crate) enum ReproAction {
    /// Verify an alternate action sequence still reproduces a repro's finding,
    /// and adopt it if it does and is no longer than the current one. The
    /// engine VERIFIES the candidate deterministically, so a simplification
    /// can never be wrong: your agent proposes a shorter/cleaner sequence,
    /// reproit disposes.
    Simplify {
        /// The repro (id or alias) to simplify, or a pending fuzz finding id.
        repro: String,
        /// Candidate action sequence as a JSON array of action strings, e.g.
        /// '["tap:key:testid:add","tap:key:testid:open-cart","tap:key:testid:
        /// remove"]'.
        #[arg(long)]
        to: String,
    },
    /// Rank suspect code for a failure (spectrum-based fault localization,
    /// Ochiai) over per-run coverage snapshots (*.cov.json from instrumented
    /// runs). Contrasts passing vs failing coverage.
    Why {
        /// Directory scanned recursively for *.cov.json coverage snapshots
        #[arg(long, default_value = ".reproit/runs")]
        dir: String,
        /// How many suspicious elements to print
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
}

/// `skills` subcommands.
#[derive(Subcommand)]
pub(crate) enum SkillsAction {
    /// Write the reproit playbook for your coding agent. `--format agents` (the
    /// default) writes AGENTS.md, the cross-agent standard read by Codex,
    /// opencode, Cursor, and most others; `--format skill` writes a SKILL.md
    /// tree (Claude Code / opencode).
    Install {
        /// Output format: agents (AGENTS.md) or skill (SKILL.md tree)
        #[arg(long, value_enum, default_value = "agents")]
        format: skills::Format,
        /// Write to the user-global location instead of the project
        #[arg(long)]
        global: bool,
        /// Explicit output directory (overrides the default location)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

/// `map` subcommands: two ways to build the map (structural = crawl the running
/// app, semantic = LLM read the code) plus views over them.
#[derive(Subcommand)]
pub(crate) enum MapAction {
    /// Crawl the running app into the verified (structural) map. Scaffolds the
    /// repo on first run if needed.
    Structural {
        /// Explorer journey name (resolves like any journey target)
        #[arg(long, default_value = "explore")]
        journey: String,
        /// Optional safety cap: how many actions the crawl may take
        #[arg(long)]
        budget: Option<u32>,
        /// Ask the LLM to give states human names (login, meet_feed, ...)
        #[arg(long)]
        label: bool,
        /// Rebuild from an existing exploration run dir instead of re-running
        #[arg(long)]
        from: Option<PathBuf>,
        /// Scaffolding platform on first-run setup: flutter | web
        #[arg(long)]
        platform: Option<String>,
        /// Overwrite existing scaffold files on first-run setup
        #[arg(long)]
        force: bool,
    },
    /// LLM-read the source into the candidate (semantic) map, reconcile,
    /// report.
    Semantic,
    /// Coverage diff: screens the code declares vs screens verified by the
    /// crawl.
    Coverage,
    /// Validate semantic candidates against the structural map, prune the wrong
    /// ones, repeat until nothing new resolves.
    Converge,
    /// Render the map graph as text (mermaid | dot) for an external viewer.
    Show {
        #[arg(long, default_value = "mermaid")]
        format: String,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, name = "map-path")]
        map_path: Option<PathBuf>,
    },
    /// The accessibility diff: ground-truth-operable elements that the
    /// accessibility/keyboard graph is missing (WCAG 2.1.1 / 4.1.2 + focus
    /// traps), per screen, read from the map's operability gaps.
    Accessibility {
        /// Only report this screen (signature or alias). Omit for all screens.
        #[arg(long)]
        state: Option<String>,
        /// Only report this gap kind: pointer_only | keyboard_unreachable |
        /// no_role | focus_trap.
        #[arg(long)]
        kind: Option<String>,
        /// Output format: text (default) | md (an exportable WCAG-cited report;
        /// redirect to a file). --json also works for the structured form.
        #[arg(long, default_value = "text")]
        format: String,
        /// Compare against a baseline appmap.json and report only the NEW gaps
        /// (regression gate; exits 1 if any new gap was introduced).
        #[arg(long)]
        baseline: Option<PathBuf>,
        #[arg(long, name = "map-path")]
        map_path: Option<PathBuf>,
    },
    /// Re-walk the committed map and report drift (exit 3 on drift).
    Verify,
}

/// Cloud subcommands. Each maps to an existing triage::*/deliver::* handler.
#[derive(Subcommand)]
pub(crate) enum CloudAction {
    /// Authenticate with a cloud/project key (sk_live_..., distinct from the
    /// app auth vault) and VALIDATE it against the cloud. Reads
    /// $REPROIT_CLOUD_KEY / $REPROIT_CLOUD_URL when not passed.
    #[command(name = "__login", hide = true)]
    Login {
        /// Cloud base URL (default: $REPROIT_CLOUD_URL)
        #[arg(long)]
        cloud: Option<String>,
        /// Cloud/project key, sk_live_... (default: $REPROIT_CLOUD_KEY)
        #[arg(long)]
        key: Option<String>,
        /// App id to validate the key against (validates via GET
        /// /v1/apps/:app/buckets). Without it, login validates via GET /v1/me.
        #[arg(long)]
        app: Option<String>,
    },
    /// One-step onboarding: wire an EXISTING cloud project into THIS repo.
    /// Validates and persists the project key, binds this GitHub repo for
    /// hosted reproduction (repository_dispatch) via PUT
    /// /v1/apps/:app/integrations, writes .github/workflows/reproit-repro.
    /// yml, and prints the remaining manual steps (the repo secret + the
    /// SDK start call). Create the project in the dashboard first, then
    /// pass its appId with --app.
    Setup {
        /// The existing project's app id (from the dashboard).
        #[arg(long)]
        app: String,
        /// Project key, sk_live_... (default: $REPROIT_CLOUD_KEY / persisted
        /// login)
        #[arg(long)]
        key: Option<String>,
        /// Cloud base URL (default: $REPROIT_CLOUD_URL)
        #[arg(long)]
        cloud: Option<String>,
        /// GitHub fine-grained PAT (Contents read/write on the app repo) the
        /// cloud uses to dispatch reproduction (default:
        /// $REPROIT_DISPATCH_TOKEN).
        #[arg(long)]
        dispatch_token: Option<String>,
        /// Override the auto-detected GitHub repo (owner/name).
        #[arg(long)]
        repo: Option<String>,
        /// Where to write the workflow (default:
        /// .github/workflows/reproit-repro.yml).
        #[arg(long)]
        workflow_path: Option<String>,
        /// Do not write the reproduction workflow file.
        #[arg(long)]
        no_workflow: bool,
    },
    /// Run fuzz locally and store the confirmed result in Cloud. Optionally
    /// links the delivered result to a pull request.
    Fuzz {
        /// Cloud app id the finding's evidence attaches to
        #[arg(long)]
        app: String,
        /// Explorer journey to drive
        #[arg(long, default_value = "explore")]
        journey: String,
        /// PR number to link the job to (enables PR comment posting)
        #[arg(long)]
        pr: Option<u64>,
        /// Cloud base URL (default: $REPROIT_CLOUD_URL)
        #[arg(long)]
        cloud: Option<String>,
        /// Existing cloud bucket to attach evidence to. Omit when discovering a
        /// new bug; the cloud will bucket uploaded findings by identity.
        #[arg(long)]
        bucket: Option<String>,
    },
    /// The IMPACT-RANKED bug list: each bucket's content-addressed id, impact
    /// score + severity, resolution status, count, and message, already sorted
    /// by impact. This is the loop's STARTING point: the command that surfaces
    /// the `bucketId` that direct reproduction, `triage`, and `timeline` use
    /// via `--bucket`. Hits GET /v1/apps/:app/buckets. Distinct from
    /// `findings` (the cohort "who's affected" lens, which has no bucket
    /// id).
    Buckets {
        #[arg(long)]
        app: String,
        /// Filter buckets by message substring.
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// The cohort "who's affected" lens: grouped clusters + counts + the user
    /// discriminators (versions, %), NOT the bucket id. Hits
    /// GET /v1/errors/:app/cohorts.
    Findings {
        #[arg(long)]
        app: String,
        #[arg(long)]
        query: Option<String>,
        /// Write the raw JSON response to stdout instead of a rendered view.
        #[arg(long)]
        export: bool,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Explain a bucket package, or resolve a crash signature to its bucket.
    BlastRadius {
        #[arg(long)]
        app: String,
        /// Content-addressed bucket id. Omit with --sig to resolve by
        /// signature.
        #[arg(long)]
        bucket: Option<String>,
        #[arg(long)]
        sig: Option<String>,
        /// Write the raw cohorts JSON to stdout instead of a rendered view.
        #[arg(long)]
        export: bool,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Private CI callback used by the generated dispatch workflow.
    #[command(name = "__replay-dispatch", hide = true)]
    ReplayDispatch {
        #[arg(long)]
        app: String,
        /// Content-addressed bucket id to reproduce. With `--as`, does pull ->
        /// check.
        #[arg(long)]
        bucket: String,
        /// Local name (alias) for the saved repro, used in `check <name>`.
        #[arg(long = "as", name = "replay_as")]
        as_name: String,
        /// Actually execute the replay (otherwise just write/save the repro).
        #[arg(long)]
        run: bool,
        /// Hosted-dispatch ledger id to complete (CI workflows pass the run id
        /// from the reproduce dispatch payload; forwarded on replay-results).
        #[arg(long)]
        run_id: Option<i64>,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Internal materialization route used by integrations. Direct bucket ids
    /// are the public interface.
    #[command(name = "__pull", hide = true)]
    /// Download a cloud bug as a first-class LOCAL repro. The ONE
    /// cloud boundary in the check loop: fetches the bucket's replay package
    /// and writes it as a saved repro under `.reproit/repros/` named `--as
    /// <name>`, the SAME on-disk shape `keep` produces. Afterwards `reproit
    /// check <name>` runs the standard local, network-free verification and
    /// `reproit repros` lists it -- indistinguishable from a locally found
    /// repro. Fetches the content-addressed `GET
    /// /v1/apps/:app/buckets/:bucket`.
    Pull {
        #[arg(long)]
        app: String,
        /// Content-addressed bucket id to pull.
        #[arg(long)]
        bucket: Option<String>,
        /// Pull the highest-impact unresolved bucket from `cloud buckets`.
        #[arg(long, conflicts_with = "bucket")]
        top: bool,
        /// Local name (alias) for the saved repro, used in `check <name>`.
        #[arg(long = "as", name = "name")]
        as_name: String,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// READ or SET a bucket's triage status (the management state: where in the
    /// lifecycle, who owns it). GET/POST /v1/apps/:app/buckets/:bucket/triage.
    /// With no --status, reads + renders the current state; with --status, sets
    /// it. Agent use: after a fix proves out locally (`check`), record intent
    /// with `--status fixed --fixed-in-build <ver>`; prod then confirms or
    /// regresses it.
    Triage {
        #[arg(long)]
        app: String,
        /// Content-addressed bucket id.
        #[arg(long)]
        bucket: String,
        /// New status: new | triaged | assigned | fixed | wontfix. Omit to
        /// READ.
        #[arg(long)]
        status: Option<String>,
        /// The build the fix shipped in (the prod-resolution anchor). Only
        /// meaningful with `--status fixed`; defaults server-side to the newest
        /// build seen for the bucket if omitted.
        #[arg(long = "fixed-in-build")]
        fixed_in_build: Option<String>,
        /// Org member id to assign (required by, and only valid for,
        /// `assigned`).
        #[arg(long)]
        assignee: Option<i64>,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// List recent prod-truth resolution TRANSITIONS (resolved->regressed,
    /// resolving->resolved, ...), newest first. GET
    /// /v1/apps/:app/resolution-events. Agent use: an autonomous monitor
    /// reads this to see what REGRESSED after a bucket was marked fixed.
    ResolutionEvents {
        #[arg(long)]
        app: String,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// The per-bucket occurrence time-series (segmented by build) + the
    /// computed prod-truth resolution. GET
    /// /v1/apps/:app/buckets/:bucket/timeline.
    Timeline {
        #[arg(long)]
        app: String,
        #[arg(long)]
        bucket: String,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Match a free-text bug report to a bucket, then explain (+ optional
    /// repro). Powers the MCP diagnose entry point.
    Diagnose {
        #[arg(long)]
        app: String,
        #[arg(long)]
        report: String,
        #[arg(long)]
        run: bool,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Bucket data out for your own analysis. Applies the same query filter as
    /// `buckets`.
    Query {
        #[arg(long)]
        app: String,
        #[arg(long)]
        query: Option<String>,
        /// Export raw data instead of a rendered view
        #[arg(long)]
        export: bool,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum JourneyAction {
    /// Run a journey by name (`reproit journey <name>`).
    #[command(external_subcommand)]
    Run(Vec<String>),
    /// List saved journeys with a one-line summary of each.
    List,
    /// Create or overwrite a journey from a JSON spec, e.g.
    /// {"setup":"login(guest)","steps":[{"do":"tap:key:testid:add"}]}.
    /// Validates the structure (and against the map if one exists) before
    /// writing journeys/<name>.yaml. Reads the spec from stdin if --spec
    /// omitted.
    Create {
        /// Journey name (the file stem under journeys/).
        name: String,
        /// The journey as a JSON object: {"setup"?, "steps":[...]}.
        #[arg(long)]
        spec: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum AuthAction {
    /// Create/update a named test account and its local vault refs.
    Add {
        /// Account handle, e.g. alice, admin, buyer.
        account: String,
        /// How this account logs in.
        #[arg(long, value_enum)]
        strategy: AuthStrategyArg,
        /// Store an email/username in the vault as <account>.email.
        #[arg(long)]
        email: Option<String>,
        /// Store a phone number in the vault as <account>.phone.
        #[arg(long)]
        phone: Option<String>,
        /// Store a username in the vault as <account>.username.
        #[arg(long)]
        username: Option<String>,
        /// Store a password in the vault as <account>.password.
        #[arg(long)]
        password: Option<String>,
        /// Store a fixed/manual OTP in the vault as <account>.otp.
        #[arg(long)]
        otp: Option<String>,
        /// Store a base32 TOTP secret in the vault as <account>.totp.
        #[arg(long)]
        totp_secret: Option<String>,
        /// Store a session blob in the vault as <account>.session.
        #[arg(long)]
        session: Option<String>,
        /// Non-secret backend user id for reset templates.
        #[arg(long)]
        user_id: Option<String>,
        /// Text that should be visible after login succeeds.
        #[arg(long)]
        validate_text: Option<String>,
        /// Store credentials only; skip automatic login-flow discovery.
        #[arg(long)]
        no_discover: bool,
    },
    /// Discover, generate, and clean-run a multi-screen login flow for an
    /// account.
    Discover { account: String },
    /// Validate a configured account: refs, vault keys, TOTP, and login
    /// journey.
    Doctor { account: String },
    /// Store a secret under a key (reads the value from stdin if --value
    /// omitted)
    Set {
        /// Vault key, e.g. alice.password
        key: String,
        #[arg(long)]
        value: Option<String>,
    },
    /// Store a base32 TOTP secret under a key (for 2FA / one-time codes)
    SetTotp {
        key: String,
        /// The base32 TOTP seed from the provider's QR/setup screen
        secret: String,
    },
    /// List stored secret keys (names only, never values)
    List,
    /// Remove a stored secret
    Remove { key: String },
    /// Show what an account resolves to: env keys and a live TOTP code. Never
    /// prints the password.
    Test { account: String },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum AuthStrategyArg {
    Password,
    PasswordOtp,
    PhoneOtp,
    EmailLink,
    OauthTest,
    Session,
    Api,
}

impl AuthStrategyArg {
    pub(crate) fn config(self) -> config::AuthStrategy {
        match self {
            AuthStrategyArg::Password => config::AuthStrategy::Password,
            AuthStrategyArg::PasswordOtp => config::AuthStrategy::PasswordOtp,
            AuthStrategyArg::PhoneOtp => config::AuthStrategy::PhoneOtp,
            AuthStrategyArg::EmailLink => config::AuthStrategy::EmailLink,
            AuthStrategyArg::OauthTest => config::AuthStrategy::OauthTest,
            AuthStrategyArg::Session => config::AuthStrategy::Session,
            AuthStrategyArg::Api => config::AuthStrategy::Api,
        }
    }
}

impl Cli {
    /// Parse a complete process argument sequence after expanding direct bug
    /// IDs.
    pub(crate) fn parse_args<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString>,
    {
        let args = args.into_iter().map(Into::into).collect();
        Self::parse_from(rewrite::expand_direct_bug_arg(args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_schema_is_internally_consistent() {
        Cli::command().debug_assert();
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

        let cli = Cli::parse_args(["reproit", "bkt_deadbeef0001"]);
        assert!(matches!(
            cli.command,
            Cmd::ReplayBucket { ref issue, .. } if issue == "bkt_deadbeef0001"
        ));

        let cli = Cli::parse_args(["reproit", "cap_deadbeef00000000", "--record"]);
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
            vec!["reproit", "cloud"],
            vec!["reproit", "cloud", "login"],
            vec!["reproit", "cloud", "pull"],
            vec!["reproit", "cloud", "reproduce"],
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
    fn record_defaults_to_original_capture_and_cloud_tester_is_explicit() {
        let cli =
            Cli::try_parse_from(["reproit", "record", "--attach", "--title", "menu bug"]).unwrap();
        assert!(matches!(
            cli.command,
            Cmd::Record {
                repro: None,
                cloud_tester: false,
                attach: true,
                title: Some(ref title),
                ..
            } if title == "menu bug"
        ));

        let cli = Cli::try_parse_from(["reproit", "record", "--cloud-tester"]).unwrap();
        assert!(matches!(
            cli.command,
            Cmd::Record {
                cloud_tester: true,
                attach: false,
                ..
            }
        ));
        assert!(Cli::try_parse_from(["reproit", "record", "--cloud-tester", "--attach"]).is_err());
        assert!(Cli::try_parse_from(["reproit", "record", "--cloud-tester", "--upload"]).is_err());

        let cli = Cli::try_parse_from(["reproit", "record", "--upload", "--no-open"]).unwrap();
        assert!(matches!(
            cli.command,
            Cmd::Record {
                upload: true,
                no_open: true,
                ..
            }
        ));
    }
}
