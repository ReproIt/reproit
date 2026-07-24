//! Clap schema for the Reproit command-line interface.

use super::context::Ctx;
use super::rewrite;
use crate::VERSION;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod actions;

pub(crate) use actions::*;

#[derive(Parser)]
#[command(
    name = "reproit",
    version = VERSION,
    about = "Find UI failures and keep every confirmed bug reproducible",
    after_help = "Run one repro:\n  reproit fnd_<id>\n  reproit rep_<id>\n  reproit @saved-name\n\nAdd video evidence:\n  reproit @saved-name --record-video"
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

/// Packaging format for the embedded agent playbook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum SkillFormat {
    /// AGENTS.md, the broad cross-agent format and default.
    Agents,
    /// Agent Skills, installed as a SKILL.md tree.
    Skill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ScanOnly {
    /// Evaluate only the declared browser route-access matrix.
    RouteAccess,
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

#[derive(Args)]
pub(crate) struct ScanArgs {
    /// What to scan. An OpenAPI, GraphQL introspection, or protobuf schema
    /// checks read-only service operations; use `--service` when the schema
    /// has no local server URL. An A2UI JSON/JSONL stream runs against the
    /// official React and Lit renderers. A URL (https://app.com) runs
    /// zero-config against that deployed app; a terminal EXECUTABLE (e.g.
    /// `lazygit`, `htop`, or a path) runs zero-config in a PTY; any other
    /// value scopes the crawl to that alias/node in a reproit.yaml.
    #[arg(value_name = "TARGET")]
    pub(crate) target: Option<String>,
    /// Disposable backend service URL for an OpenAPI, GraphQL, or protobuf
    /// target. Overrides the schema server URL.
    #[arg(long, value_name = "URL")]
    pub(crate) service: Option<String>,
    /// Backend service base URL. Precedence: --target > REPROIT_BACKEND_URL >
    /// backend.target in reproit.yaml > the schema servers entry.
    #[arg(long = "target", value_name = "URL")]
    pub(crate) target_url: Option<String>,
    /// Workflow override for a URL target: `web` forces the zero-config
    /// browser scan even inside a backend project; `backend` requires the
    /// backend configuration.
    #[arg(long, value_name = "PLATFORM")]
    pub(crate) platform: Option<String>,
    /// Coverage budget: how many actions the crawl may take to reach screens.
    #[arg(long, default_value_t = 60)]
    pub(crate) budget: u32,
    /// Force the simulator tier (default: headless / web).
    #[arg(long)]
    pub(crate) sim: bool,
    /// After the crawl, record a video for every distinct reported finding.
    /// Visually localizable findings are boxed; the rest are diagnostic clips.
    #[arg(long)]
    pub(crate) record_video: bool,
    /// Where the `--record-video` clips land (default:
    /// .reproit/recordings/scan/<scan-run>/).
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
    /// Extra HTTP header injected into the browser context, `"Name: value"`.
    /// Repeatable. Use it to pass a WAF clearance cookie, an auth bearer, or a
    /// preview token so a challenge-fronted or authenticated target is reachable.
    #[arg(long = "header", value_name = "NAME: VALUE")]
    pub(crate) headers: Vec<String>,
    /// Restrict the scan to one declarative contract family.
    #[arg(long, value_enum)]
    pub(crate) only: Option<ScanOnly>,
}

#[derive(Args)]
pub(crate) struct FuzzArgs {
    /// What to fuzz (optional). Schemas drive valid service calls, A2UI streams
    /// are checked across renderers, tests become a replay prefix, URLs and
    /// terminal executables run zero-config, and other values name a journey.
    #[arg(value_name = "TARGET")]
    pub(crate) target_arg: Option<String>,
    /// Disposable backend service URL for schema-driven targets.
    #[arg(long, value_name = "URL")]
    pub(crate) service: Option<String>,
    /// Same-origin reset endpoint for exact stateful replay and minimization.
    #[arg(long, value_name = "URL")]
    pub(crate) reset: Option<String>,
    /// Explorer journey to drive.
    #[arg(long, default_value = "explore")]
    pub(crate) journey: String,
    /// First seed; runs use seed, seed+1, ...
    #[arg(long, default_value_t = 1)]
    pub(crate) seed: u64,
    /// Number of seeds to try.
    #[arg(long, default_value_t = 3)]
    pub(crate) runs: u32,
    /// Actions per walk.
    #[arg(long, default_value_t = 40)]
    pub(crate) budget: u32,
    /// Skip clean-session confirmation and minimization.
    #[arg(long)]
    pub(crate) no_confirm: bool,
    /// Keep hunting and collect unique findings across the whole seed budget.
    #[arg(long)]
    pub(crate) all: bool,
    /// Start each walk from the least-visited reachable state.
    #[arg(long)]
    pub(crate) frontier: bool,
    /// Replay a journey, then fuzz outward from its end state.
    #[arg(long)]
    pub(crate) from: Option<String>,
    /// Use uniform-random choices and a fixed budget.
    #[arg(long)]
    pub(crate) uniform: bool,
    /// JSON array of real user action paths to branch from.
    #[arg(long)]
    pub(crate) seeds: Option<String>,
    /// Seeds per drive session. Zero runs all seeds in one session.
    #[arg(long, default_value_t = 0)]
    pub(crate) batch: u32,
    /// Print a per-phase timing breakdown for each drive session.
    #[arg(long)]
    pub(crate) profile_timing: bool,
    /// Force the simulator tier.
    #[arg(long)]
    pub(crate) sim: bool,
    /// Confirm a headless finding once on a simulator.
    #[arg(long)]
    pub(crate) confirm_on_sim: bool,
    /// Cloud base URL for the optional delivery pipeline.
    #[arg(long)]
    pub(crate) cloud: Option<String>,
    /// Cloud app id for delivered evidence.
    #[arg(long)]
    pub(crate) app: Option<String>,
    /// Cloud bucket id for delivered evidence.
    #[arg(long)]
    pub(crate) bucket: Option<String>,
    /// Post the generated pull request comment instead of emitting a dry run.
    #[arg(long)]
    pub(crate) post_comment: bool,
    /// Run the leak detector over a reversible cycle.
    #[arg(long)]
    pub(crate) soak: bool,
    /// Semicolon-separated actions for a soak cycle.
    #[arg(long)]
    pub(crate) cycle: Option<String>,
    /// Number of soak cycle repetitions.
    #[arg(long, default_value_t = 15)]
    pub(crate) repeats: u32,
    /// Reuse the previous build for a soak run.
    #[arg(long)]
    pub(crate) warm: bool,
    /// Comma-separated engines or platforms; on a backend project a URL value
    /// is the backend service base URL (precedence: --target >
    /// REPROIT_BACKEND_URL > backend.target > the schema servers entry).
    #[arg(long)]
    pub(crate) target: Option<String>,
    /// Workflow override for a URL target: `web` forces the zero-config
    /// browser fuzz even inside a backend project; `backend` requires the
    /// backend configuration.
    #[arg(long, value_name = "PLATFORM")]
    pub(crate) platform: Option<String>,
    /// URL for a web-engine target, defaulting to app.url.
    #[arg(long)]
    pub(crate) url: Option<String>,
    /// Run web-engine targets headlessly.
    #[arg(long)]
    pub(crate) headless: bool,
    /// Comma-separated locales.
    #[arg(long)]
    pub(crate) locale: Option<String>,
    /// Restrict execution to these detector categories.
    #[arg(long)]
    pub(crate) only: Option<String>,
    /// Exclude these detector categories after applying --only.
    #[arg(long = "no")]
    pub(crate) no_oracles: Option<String>,
    /// Specific device name or id.
    #[arg(long)]
    pub(crate) device: Option<String>,
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
        #[arg(value_name = "URL", conflicts_with = "learn")]
        target: Option<String>,
        /// Platform override: flutter | web | rn | android | backend.
        #[arg(long)]
        platform: Option<String>,
        /// Derive a DRAFT schema from the backend source when the project has
        /// none (implies --platform backend).
        #[arg(long)]
        learn: bool,
        /// Running service base URL: --learn sends one bounded GET per derived
        /// parameterless GET route and records the observed response.
        #[arg(long = "target", value_name = "SERVICE_URL", requires = "learn")]
        learn_target: Option<String>,
        /// Replace existing generated scaffold files.
        #[arg(long)]
        force: bool,
    },
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
    /// Run the saved regression suite and classify each: pass (0) / fail (1) /
    /// flaky (2) / stale (3). To reproduce one bug, run `reproit <id>` or
    /// `reproit @saved-name`.
    /// Add `--record-video` to save video evidence; the visual oracle is
    /// `baseline`.
    Check {
        /// Internal direct-reference route. Users run `reproit <id>` or
        /// `reproit @saved-name`.
        #[arg(long = "repro-id", hide = true)]
        repro: Option<String>,
        /// A captured-production backend payload file (the
        /// `reproit-backend-capture` JSON that `debug replay-capture` takes)
        /// to re-evaluate under check's verdict contract. A saved repro or
        /// finding with the same name still resolves as the saved artifact.
        #[arg(value_name = "CAPTURE", conflicts_with = "repro")]
        reference: Option<String>,
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
        /// Save screen video as supporting evidence for each executed repro.
        #[arg(long)]
        record_video: bool,
        /// Scan recorded video for transient render glitches.
        #[arg(long, requires = "record_video")]
        flicker: bool,
        /// Run repros connected to files changed since BASE first, then run the
        /// rest of the full suite. With no value, BASE defaults to HEAD^. This
        /// changes feedback order only and never skips an unmapped repro.
        #[arg(
            long,
            value_name = "BASE",
            num_args = 0..=1,
            default_missing_value = "HEAD^",
            conflicts_with = "repro"
        )]
        changed: Option<String>,
    },
    /// Open one repro on its configured platform, step through its actions, and
    /// write a structured fix packet. Inspection is diagnostic and never
    /// promotes or updates the saved guard.
    Inspect {
        /// Saved repro id or alias, or a production bucket id to pull first.
        /// Backend projects also accept a finding id or a captured-production
        /// payload file (the `debug replay-capture` artifact).
        #[arg(value_name = "REPRO")]
        reference: String,
        /// Backend only: step through the recorded event trail without
        /// re-sending any request to a live target.
        #[arg(long)]
        offline: bool,
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
    /// Explain the immutable authority, evaluation, replay, minimization, and
    /// promotion decision for a finding or saved repro.
    Proof {
        /// Finding id, repro id, or saved repro alias.
        reference: String,
    },
    /// List discovered candidates that are still blocked from promotion, with
    /// the exact missing proof stages.
    Candidates,
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
    #[command(name = "__capture", hide = true)]
    Capture {
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
    /// Match a bug report to a confirmed production bug.
    Diagnose {
        report: String,
        #[arg(long)]
        run: bool,
    },
    /// List recent production confirmation and regression transitions.
    ResolutionEvents,
    /// Open a repro's recorded video in your default player. Recordings live
    /// under .reproit/recordings/repro/ (gitignored); make one with
    /// `reproit @name --record-video`.
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
    /// Diagnose local setup: config, runner deps, app URL, and cloud
    /// credentials.
    Doctor,
    /// Configure and verify one test login. `auth verify <account>` replays the
    /// contract directly; `auth discover <account>` regenerates it first.
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

impl Cli {
    /// Parse a complete process argument sequence after expanding direct bug
    /// IDs and named references.
    pub(crate) fn parse_args<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString>,
    {
        let args = args.into_iter().map(Into::into).collect();
        Self::parse_from(rewrite::expand_direct_reference_arg(args))
    }
}

#[cfg(test)]
mod tests;
