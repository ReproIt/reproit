//! reproit: reproducible AI QA. Deterministic multi-device test orchestration
//! with evidence capture. See docs/cli.md.

// These two doc-format lints (new in clippy 1.93) fire on intentionally aligned
// hanging-indent doc tables (e.g. model/repro.rs) whose alignment aids reading.
// Keep the alignment rather than reflow it to satisfy a purely stylistic lint.
#![allow(clippy::doc_overindented_list_items, clippy::doc_lazy_continuation)]

// Top-level: CLI entry, config, and cross-cutting infra.
mod auth;
mod config;
mod crashreporter;
mod crosscut;
mod exec;
mod init;
mod junit;
mod mcp;
mod skills;
// backends/, the execution layer: device/runtime drivers + run orchestration.
// (#[path] keeps the module path flat, `crate::tui`, `crate::drive`, ..., so
// the folder grouping is purely organizational and changes no call sites.)
#[path = "backends/drive.rs"]
mod drive;
#[path = "backends/frames.rs"]
mod frames;
#[path = "backends/orchestrator.rs"]
mod orchestrator;
#[path = "backends/platform.rs"]
mod platform;
#[path = "backends/reset.rs"]
mod reset;
#[path = "backends/simctl.rs"]
mod simctl;
#[path = "backends/tui.rs"]
mod tui;
#[path = "backends/vmservice.rs"]
mod vmservice;
// modes/, the user-facing commands.
#[path = "modes/analyze.rs"]
mod analyze;
#[path = "modes/barrier.rs"]
mod barrier;
#[path = "modes/deliver.rs"]
mod deliver;
#[path = "modes/fix.rs"]
mod fix;
#[path = "modes/flicker.rs"]
mod flicker;
#[path = "modes/fuzz.rs"]
mod fuzz;
#[path = "modes/graph.rs"]
mod graph;
#[path = "modes/import.rs"]
mod import;
#[path = "modes/journey.rs"]
mod journey;
#[path = "modes/mapplan.rs"]
mod mapplan;
#[path = "modes/screenshots.rs"]
mod screenshots;
#[path = "modes/soak.rs"]
mod soak;
#[path = "modes/triage.rs"]
mod triage;
#[path = "modes/visual.rs"]
mod visual;
// model/, the app model + analysis.
#[path = "model/accessibility.rs"]
mod accessibility;
#[path = "model/appmap.rs"]
mod appmap;
#[path = "model/attribute.rs"]
mod attribute;
#[path = "model/candidate.rs"]
mod candidate;
#[path = "model/fault.rs"]
mod fault;
#[path = "model/fixture.rs"]
mod fixture;
#[path = "model/invariants.rs"]
mod invariants;
#[path = "model/map.rs"]
mod map;
#[path = "model/repro.rs"]
mod repro;
#[path = "model/signature.rs"]
mod signature;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Central exit-code contract (the CI contract; see docs/cli.md).
///
/// 0 clean, 1 regression, 2 flaky, 3 stale. The four-outcome `check`
/// classification produces these via `repro::Outcome::exit_code`; this enum is
/// the named, command-agnostic surface (e.g. `--visual`, `--soak` map their
/// pass/fail onto Clean/Regression).
#[derive(Clone, Copy)]
#[allow(dead_code)] // Clean is the implicit Ok path; kept for the explicit contract.
enum Exit {
    /// clean / all pass
    Clean = 0,
    /// real regression (replayed, still broken)
    Regression = 1,
    /// flaky (same actions, inconsistent result -> app race)
    Flaky = 2,
    /// stale (UI changed, couldn't replay -> re-record)
    Stale = 3,
}

impl Exit {
    fn code(self) -> ExitCode {
        ExitCode::from(self as u8)
    }
}

impl From<repro::Outcome> for Exit {
    fn from(o: repro::Outcome) -> Self {
        match o {
            repro::Outcome::Pass => Exit::Clean,
            repro::Outcome::Fail => Exit::Regression,
            repro::Outcome::Flaky => Exit::Flaky,
            repro::Outcome::Stale => Exit::Stale,
        }
    }
}

/// The single place a non-clean run leaves the process. Returned from `main`
/// as an `ExitCode` so the clean path stays a normal `Ok` return.
fn exit_with(e: Exit) -> ExitCode {
    e.code()
}

/// Cross-cutting flags carried on every command (clap `global = true`).
/// Threaded as a small context. `--json` and `--quiet` are honored by the
/// structured commands (fuzz, check, keep, repros, map); `--yes` suppresses
/// prompts.
#[derive(Clone, Copy, Default)]
pub(crate) struct Ctx {
    json: bool,
    quiet: bool,
    #[allow(dead_code)] // reserved for interactive prompts (keep picker)
    yes: bool,
}

impl Ctx {
    /// Print a human line unless `--quiet` or `--json` is set (JSON output is
    /// the machine surface; human chatter would corrupt it).
    pub(crate) fn say(&self, line: impl std::fmt::Display) {
        if !self.quiet && !self.json {
            println!("{line}");
        }
    }

    /// Emit a JSON object to stdout (pretty), when `--json` is set.
    pub(crate) fn emit(&self, value: &serde_json::Value) {
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into())
            );
        }
    }
}

/// Version string stamped by build.rs: a clean `0.1.<commit-count>` for an
/// install / clean build, plus a `(<rev>-dirty <date>)` suffix ONLY for local
/// working builds with uncommitted edits. So `cargo install` shows a plain
/// `0.1.64` while a dev build is obviously identifiable.
const VERSION: &str = env!("REPROIT_VERSION");

#[derive(Parser)]
#[command(
    name = "reproit",
    version = VERSION,
    about = "Reproducible AI QA: map -> fuzz -> check"
)]
struct Cli {
    /// Path to reproit.yaml (default: search cwd and ancestors)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Machine-readable output (CI, scripts, the MCP bridge)
    #[arg(long, global = true)]
    json: bool,
    /// Minimal output (CI logs)
    #[arg(long, global = true)]
    quiet: bool,
    /// Never prompt (non-interactive / CI)
    #[arg(long, global = true)]
    yes: bool,
    #[command(subcommand)]
    command: Cmd,
}

impl Cli {
    fn ctx(&self) -> Ctx {
        Ctx {
            json: self.json,
            quiet: self.quiet,
            yes: self.yes,
        }
    }
}

/// Classify the positional fuzz target: a URL (point at a deployed app) vs an
/// alias/node to scope the hunt to. Returns the full URL (scheme prepended if
/// missing) when it looks like one, else None (an alias like "login").
///
/// We don't sniff http-vs-https: a bare host defaults to https (http redirects
/// to it anyway), and localhost/loopback defaults to http (dev servers). A token
/// is a URL if it has a scheme, a dotted host, is loopback, or has a host:port.
fn target_as_url(t: &str) -> Option<String> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        return Some(t.to_string());
    }
    // The authority is everything before the first '/'; the host is before any ':'.
    let authority = t.split('/').next().unwrap_or(t);
    let host = authority.split(':').next().unwrap_or(authority);
    let is_loopback = host == "localhost" || host == "127.0.0.1" || host == "0.0.0.0";
    let has_port = authority
        .rsplit_once(':')
        .map(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false);
    if host.contains('.') || is_loopback || has_port {
        let scheme = if is_loopback { "http" } else { "https" };
        return Some(format!("{scheme}://{t}"));
    }
    None
}

// A clap subcommand enum: variants carry their flags by value and are
// instantiated once at startup, so the size spread between variants is
// irrelevant (and unavoidable for a rich CLI).
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Cmd {
    /// The app map. `map structural` crawls the running app; `map semantic` reads
    /// the code; `map coverage` diffs them. Bare `map` builds the structural map.
    Map {
        #[command(subcommand)]
        action: Option<MapAction>,
    },
    /// Run saved repros and classify each: pass (0) / fail (1) / flaky (2) /
    /// stale (3). With no name, runs the whole suite and exits with the worst.
    /// `--record` runs a repro once with annotated video; `--visual` runs the
    /// visual regression oracle.
    Check {
        /// Repro/journey name. Empty = run the whole saved suite.
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
        /// Run ONCE with full evidence capture + annotated video (was `run`)
        #[arg(long)]
        record: bool,
        /// Reuse the previous build (--no-build). Only valid when the last
        /// build was this same journey. Applies to `--record`.
        #[arg(long)]
        warm: bool,
        /// (--record) capture SHOOT screenshots into this directory
        #[arg(long)]
        shots_dir: Option<PathBuf>,
        /// (--record) drive in profile mode (AOT) for representative perf
        #[arg(long)]
        profile: bool,
        /// Visual regression against the committed baseline (was `visual`)
        #[arg(long)]
        visual: bool,
        /// (--visual) accept the current capture as the new baseline
        #[arg(long)]
        update: bool,
        /// Intra-run flicker detection: record once, scan the video for transient
        /// render glitches (a frame that diverges then snaps back). No baseline.
        #[arg(long)]
        flicker: bool,
        /// Treat a quarantined repro's failure as blocking too (no effect on
        /// the exit code today: every outcome already maps to its CI code).
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
    /// Save a repro from the latest fuzz run into the committed suite. The
    /// store dir is the repro's CONTENT HASH (.reproit/repros/<id>/), stable
    /// across machines and self-deduping. `--as` assigns a human alias.
    Keep {
        /// Finding id (dirname) from the latest fuzz run. Uses the sole finding
        /// if omitted, else lists choices.
        id: Option<String>,
        /// Human alias for the kept repro (used in `check <alias>`)
        #[arg(long = "as", name = "name")]
        as_name: Option<String>,
        /// Land the repro `required` (blocking) immediately instead of
        /// quarantined-until-first-green.
        #[arg(long)]
        strict: bool,
    },
    /// Verify an alternate action sequence still reproduces a repro's finding,
    /// and adopt it if it does and is no longer than the current one. The engine
    /// VERIFIES the candidate deterministically, so a simplification can never be
    /// wrong: your agent proposes a shorter/cleaner sequence, reproit disposes.
    Simplify {
        /// The repro (id or alias) to simplify, or a pending fuzz finding id.
        repro: String,
        /// Candidate action sequence as a JSON array of action strings, e.g.
        /// '["tap:key:testid:add","tap:key:testid:open-cart","tap:key:testid:remove"]'.
        #[arg(long)]
        to: String,
    },
    /// List saved repros under .reproit/repros/
    Repros,
    /// Open a repro's recorded video in your default player. Recordings live
    /// under .reproit/media/ (gitignored); make one with `check <id> --record`.
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
        /// Fix the a11y class instead: unlabeled tappables from the live map
        #[arg(long)]
        a11y: bool,
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
    /// Find repros using the map (pure; emits a fuzz artifact). All oracles on
    /// by default. `--soak` runs the leak cycle; `--target` selects engines.
    Fuzz {
        /// What to fuzz (optional). A URL (https://app.com) is auto-detected and
        /// runs zero-config against that deployed app, no reproit.yaml needed.
        /// Any other value scopes the hunt to that alias/node.
        #[arg(value_name = "TARGET")]
        target_arg: Option<String>,
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
        /// Greedy-shrink the first finding (extra replays, slow)
        #[arg(long)]
        shrink: bool,
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
        /// state between them. Default 0 = all `runs` in ONE session. `--batch
        /// 1` = the legacy one-drive-per-seed behavior (use for the A/B).
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
        /// Finding index in the cloud error list (default 0).
        #[arg(long, default_value_t = 0)]
        app_idx: usize,
        /// Actually POST the PR comment (needs GITHUB_TOKEN + repo + PR);
        /// otherwise the pipeline emits the comment markdown as a dry-run.
        #[arg(long)]
        post_comment: bool,
        /// Leak oracle: repeat a reversible cycle and watch heap growth per
        /// cycle (was `soak`). Use with --cycle / --repeats.
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
        /// (divergence oracle; was `web-diff`). The first engine is reference.
        #[arg(long)]
        target: Option<String>,
        /// (--target web engines) URL under test (defaults to app.url)
        #[arg(long)]
        url: Option<String>,
        /// (--target web engines) run headless (default headed, so the real
        /// GPU compositor runs)
        #[arg(long)]
        headless: bool,
        /// (--target web engines) output dir for frames, diffs, report.html
        #[arg(long, default_value = "/tmp/reproit-diff")]
        out: String,
        /// (--target web engines) optional replay path JSON
        #[arg(long)]
        replay: Option<String>,
        /// Comma-separated locale list to fuzz across (e.g. de,ar,ja). Each
        /// locale runs the flow once with REPROIT_LOCALE set; findings are
        /// tagged with their locale and locale-specific i18n findings are
        /// noted. Unset = the app default (behavior unchanged).
        #[arg(long)]
        locale: Option<String>,
        /// Restrict to these oracle categories (crash,jank,leak,visual,
        /// divergence,a11y,i18n,graph). Default: all on.
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
    /// introspection backend, and what's executable today
    Platforms,
    /// Install the bundled coding-agent skills (the reproit playbook) into
    /// .claude/skills, so an agent drives reproit like an expert
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Check that required tools are available (folded into `map`; kept for
    /// MCP and CI use)
    #[command(hide = true)]
    Doctor,
    /// Test-login creds for the app under test (encrypted credential vault)
    Secrets {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Author and list scripted journeys (declarative YAML paths). Running a
    /// journey is `check <name>`; this manages the files, for MCP/agent authoring.
    Journey {
        #[command(subcommand)]
        action: JourneyAction,
    },
    /// Capture store/marketing screenshots: drive a tour (a journey) across
    /// locales and devices into a journey-led layout (or your own --path-template).
    /// Reuses the SHOOT capture machinery; one locale-invariant tour covers every
    /// locale.
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
        /// Per-shot directory template, overriding the auto layout. Placeholders:
        /// {journey} {platform} {locale} {device}. Example: "{locale}/{device}".
        #[arg(long)]
        path_template: Option<String>,
    },
    /// Import a flow from another tool into a reproit journey (switching cost ~0).
    /// Currently supports Maestro: `reproit import maestro flow.yaml`.
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
    /// Cloud loop: a fleet + production telemetry. Submit jobs, browse findings,
    /// see blast radius, and reproduce real user sessions deterministically.
    Cloud {
        #[command(subcommand)]
        action: CloudAction,
    },
    /// (internal) PTY-driven terminal-UI runner; spawned by the tui backend
    #[command(name = "__tui", hide = true)]
    TuiRun,
}

/// `skills` subcommands.
#[derive(Subcommand)]
enum SkillsAction {
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
enum MapAction {
    /// Crawl the running app into the verified (structural) map. Scaffolds the
    /// repo on first run if needed.
    Structural {
        /// Explorer journey name (resolves like any journey target)
        #[arg(long, default_value = "explore")]
        journey: String,
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
    /// LLM-read the source into the candidate (semantic) map, reconcile, report.
    Semantic,
    /// Coverage diff: screens the code declares vs screens verified by the crawl.
    Coverage,
    /// Validate semantic candidates against the structural map, prune the wrong
    /// ones, repeat until nothing new resolves.
    Converge,
    /// Render the map (mermaid | dot | html).
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
        /// Only report this gap kind: pointer_only | keyboard_unreachable | no_role | focus_trap.
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
enum CloudAction {
    /// Authenticate with a cloud service token (distinct from `secrets`).
    /// Reads $REPROIT_API_KEY / $REPROIT_CLOUD_URL when not passed.
    Login {
        /// Cloud base URL (default: $REPROIT_CLOUD_URL)
        #[arg(long)]
        cloud: Option<String>,
        /// Cloud service token (default: $REPROIT_API_KEY)
        #[arg(long)]
        key: Option<String>,
    },
    /// Fan-out fuzz job -> stored artifact (auto-links to a PR). Submits via the
    /// existing fuzz cloud-delivery path.
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
        /// Finding index in the cloud error list (default 0)
        #[arg(long, default_value_t = 0)]
        idx: usize,
    },
    /// The IMPACT-RANKED bug list: each bucket's content-addressed id, impact
    /// score + severity, resolution status, count, and message, already sorted
    /// by impact. This is the loop's STARTING point: the ONLY command that
    /// surfaces the `bucketId` that `pull`/`triage`/`timeline` take via
    /// `--bucket`. Hits GET /v1/apps/:app/buckets. Distinct from `findings` (the
    /// cohort "who's affected" lens, which has no bucket id).
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
    /// discriminators (versions, %), NOT the bucket id. Was `triage find`.
    /// Hits GET /v1/errors/:app/cohorts.
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
    /// Who's affected by a bucket: cohorts, %, versions. Was `triage explain`.
    /// Hits GET /v1/errors/:app/cohorts.
    BlastRadius {
        #[arg(long)]
        app: String,
        #[arg(long)]
        sig: Option<String>,
        #[arg(long)]
        idx: Option<usize>,
        /// Write the raw cohorts JSON to stdout instead of a rendered view.
        #[arg(long)]
        export: bool,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Pull a real user session and replay it locally. Was `triage reproduce`.
    /// Hits GET /v1/errors/:app/:idx/repro.
    Reproduce {
        #[arg(long)]
        app: String,
        #[arg(long)]
        idx: usize,
        #[arg(long, default_value = "explore")]
        journey: String,
        /// Actually execute the replay (otherwise just write the config)
        #[arg(long)]
        run: bool,
        /// Write the raw repro JSON to stdout instead of a rendered view.
        #[arg(long)]
        export: bool,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// EXPLICITLY download a cloud bug as a first-class LOCAL repro. The ONE
    /// cloud boundary in the check loop: fetches the bucket's replay package and
    /// writes it as a saved repro under `.reproit/repros/` named `--as <name>`,
    /// the SAME on-disk shape `keep` produces. Afterwards `reproit check <name>`
    /// runs the standard local, network-free verification and `reproit repros`
    /// lists it -- indistinguishable from a locally found repro.
    /// Prefers the content-addressed `GET /v1/apps/:app/buckets/:bucket`; pass
    /// `--idx` to use the legacy `GET /v1/errors/:app/:idx/repro` instead.
    Pull {
        #[arg(long)]
        app: String,
        /// Content-addressed bucket id to pull (preferred). Provide this OR --idx.
        #[arg(long)]
        bucket: Option<String>,
        /// Legacy error index to pull instead of a bucket. Provide this OR --bucket.
        #[arg(long)]
        idx: Option<usize>,
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
    /// it. Agent use: after a fix proves out locally (`check`), record intent with
    /// `--status fixed --fixed-in-build <ver>`; prod then confirms or regresses it.
    Triage {
        #[arg(long)]
        app: String,
        /// Content-addressed bucket id.
        #[arg(long)]
        bucket: String,
        /// New status: new | triaged | assigned | fixed | wontfix. Omit to READ.
        #[arg(long)]
        status: Option<String>,
        /// The build the fix shipped in (the prod-resolution anchor). Only
        /// meaningful with `--status fixed`; defaults server-side to the newest
        /// build seen for the bucket if omitted.
        #[arg(long = "fixed-in-build")]
        fixed_in_build: Option<String>,
        /// Org member id to assign (required by, and only valid for, `assigned`).
        #[arg(long)]
        assignee: Option<i64>,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// List recent prod-truth resolution TRANSITIONS (resolved->regressed,
    /// resolving->resolved, ...), newest first. GET /v1/apps/:app/resolution-events.
    /// Agent use: an autonomous monitor reads this to see what REGRESSED after a
    /// bucket was marked fixed.
    ResolutionEvents {
        #[arg(long)]
        app: String,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// The per-bucket occurrence time-series (segmented by build) + the computed
    /// prod-truth resolution. GET /v1/apps/:app/buckets/:bucket/timeline.
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
    /// Match a free-text bug report to a bucket, then explain (+ optional repro).
    /// Was `triage diagnose`; powers the MCP diagnose entry point.
    Diagnose {
        #[arg(long)]
        app: String,
        #[arg(long)]
        report: String,
        #[arg(long)]
        run: bool,
        #[arg(long, default_value = "explore")]
        journey: String,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Raw data out for your own analysis. TODO(phase-b): a real query surface;
    /// for now routes to the findings list with `--export` semantics.
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
enum JourneyAction {
    /// List saved journeys with a one-line summary of each.
    List,
    /// Create or overwrite a journey from a JSON spec, e.g.
    /// {"setup":"login(guest)","steps":[{"do":"tap:key:testid:add"}]}.
    /// Validates the structure (and against the map if one exists) before
    /// writing journeys/<name>.yaml. Reads the spec from stdin if --spec omitted.
    Save {
        /// Journey name (the file stem under journeys/).
        name: String,
        /// The journey as a JSON object: {"setup"?, "steps":[...]}.
        #[arg(long)]
        spec: Option<String>,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    /// Store a secret under a key (reads the value from stdin if --value omitted)
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

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let ctx = cli.ctx();
    match cli.command {
        Cmd::Doctor => {
            doctor(cli.config.as_deref()).await?;
            Ok(ExitCode::SUCCESS)
        }
        // `map`: build/refresh the graph, or render it with --show. Folds in the
        // old `init` (first-run scaffold), `map`, and `graph`.
        Cmd::Map { action } => {
            // Bare `map` builds the structural map (the common case).
            let action = action.unwrap_or(MapAction::Structural {
                journey: "explore".to_string(),
                label: false,
                from: None,
                platform: None,
                force: false,
            });
            match action {
                MapAction::Structural {
                    journey,
                    label,
                    from,
                    platform,
                    force,
                } => {
                    // Scaffold the repo first if there's no config yet (folds in
                    // the old `init`); then crawl + assemble the graph.
                    if config::load(cli.config.as_deref()).is_err() {
                        init::init(&std::env::current_dir()?, platform.as_deref(), force)?;
                    }
                    let loaded = config::load(cli.config.as_deref())?;
                    map::build_map(
                        &loaded.config,
                        &loaded.root,
                        &journey,
                        label,
                        from.as_deref(),
                    )
                    .await?;
                    if ctx.json {
                        let m = map::load_map(&loaded.root, &loaded.config);
                        ctx.emit(&serde_json::json!({
                            "command": "map structural",
                            "states": m.states.len(),
                            "transitions": m.transitions.len(),
                            "map_path": loaded.root.join(".reproit/appmap.json").to_string_lossy(),
                        }));
                    }
                    Ok(ExitCode::SUCCESS)
                }
                MapAction::Show {
                    format,
                    out,
                    map_path,
                } => {
                    let path = match map_path {
                        Some(p) => p,
                        None => {
                            let loaded = config::load(cli.config.as_deref())?;
                            loaded.root.join(".reproit/appmap.json")
                        }
                    };
                    graph::render(&path, &format, out.as_deref())?;
                    Ok(ExitCode::SUCCESS)
                }
                MapAction::Accessibility {
                    state,
                    kind,
                    format,
                    baseline,
                    map_path,
                } => {
                    // `root` is the project to attribute selectors into (file:
                    // line). With an explicit --map-path we have no project tree
                    // to scan, so attribution is skipped (None).
                    let (m, root) = match map_path {
                        Some(p) => {
                            let txt = std::fs::read_to_string(&p)?;
                            (serde_json::from_str::<appmap::AppMap>(&txt)?, None)
                        }
                        None => {
                            let loaded = config::load(cli.config.as_deref())?;
                            let m = map::load_map(&loaded.root, &loaded.config);
                            (m, Some(loaded.root))
                        }
                    };
                    // --baseline: regression gate. Diff the current map's gaps
                    // against the baseline's and exit 1 if any new gap appeared.
                    if let Some(bpath) = baseline {
                        let btxt = std::fs::read_to_string(&bpath)?;
                        let bmap = serde_json::from_str::<appmap::AppMap>(&btxt)?;
                        let regressed = accessibility::regression(&bmap, &m, &ctx);
                        return Ok(if regressed {
                            ExitCode::from(1)
                        } else {
                            ExitCode::SUCCESS
                        });
                    }
                    accessibility::report(
                        &m,
                        root.as_deref(),
                        state.as_deref(),
                        kind.as_deref(),
                        format == "md",
                        &ctx,
                    );
                    Ok(ExitCode::SUCCESS)
                }
                MapAction::Verify => {
                    let loaded = config::load(cli.config.as_deref())?;
                    let report = journey::verify_map(&loaded, ctx.json || ctx.quiet).await?;
                    let code = if report.is_clean() { 0u8 } else { 3u8 };
                    Ok(ExitCode::from(code))
                }
                MapAction::Semantic => {
                    let loaded = config::load(cli.config.as_deref())?;
                    let cm = mapplan::plan(&loaded, ctx.quiet).await?;
                    if ctx.json {
                        let mut v = mapplan::coverage_json(&cm);
                        v["command"] = "map semantic".into();
                        ctx.emit(&v);
                    }
                    Ok(ExitCode::SUCCESS)
                }
                MapAction::Coverage => {
                    let loaded = config::load(cli.config.as_deref())?;
                    mapplan::cover(&loaded, ctx.json)?;
                    Ok(ExitCode::SUCCESS)
                }
                MapAction::Converge => {
                    let loaded = config::load(cli.config.as_deref())?;
                    mapplan::converge_cmd(&loaded, ctx.json)?;
                    Ok(ExitCode::SUCCESS)
                }
            }
        }
        // `check`: run saved repros and classify each pass/fail/flaky/stale
        // (the four-outcome CI contract), or one journey with video (--record),
        // or the visual oracle (--visual). With no name, runs the whole suite
        // and aggregates the worst outcome.
        Cmd::Check {
            repro,
            devices,
            kind,
            runs,
            junit,
            record,
            warm,
            shots_dir,
            profile,
            visual,
            update,
            flicker,
            strict,
            locale,
            target,
            device,
        } => {
            let _ = strict; // every outcome already maps to its CI code.
            let loaded = config::load(cli.config.as_deref())?;
            let locales = locale
                .as_deref()
                .map(crosscut::parse_locales)
                .unwrap_or_default();
            // MULTI-TARGET --target dispatch for `check`: when `--target` names
            // more than one run target (web engines chromium,firefox,webkit, or
            // platforms ios,android), run the saved suite on EACH target and diff
            // which repros are red on a SUBSET of targets (a divergence). The
            // single-target / no-target path below stays the rich locale+junit+
            // promotion flow unchanged. Not used with --visual/--record (those
            // are single-shot evidence runs).
            if !visual && !record && !flicker {
                if let Some(raw) = target.as_deref() {
                    let (rts, unknown_t) = crosscut::parse_run_targets(raw);
                    for u in &unknown_t {
                        ctx.say(format!("  warn: unknown target `{u}` (ignored)"));
                    }
                    if rts.len() > 1 {
                        return run_check_targets(
                            &ctx,
                            &loaded,
                            &rts,
                            device.as_deref(),
                            &repro,
                            runs,
                            devices,
                            kind.as_deref(),
                        )
                        .await;
                    }
                }
            }
            // --target / --device device selection. When neither is given and a
            // TTY is present (and not --yes), pick interactively; non-interactive
            // falls back to the config default rather than hanging.
            let selected_device = resolve_check_device(
                &ctx,
                &loaded.config.app.platform,
                target.as_deref(),
                device.as_deref(),
            )
            .await;
            if let Some(d) = &selected_device {
                std::env::set_var("REPROIT_PLATFORM", d.target.as_str());
                std::env::set_var("REPROIT_DEVICE", &d.id);
                ctx.say(format!("  device: {} ({})", d.name, d.target.as_str()));
            }
            if visual {
                let Some(vis) = &loaded.config.visual else {
                    anyhow::bail!("no `visual` section in reproit.yaml");
                };
                let ok = visual::diff(vis, &loaded.root, update)?;
                return Ok(if ok {
                    ExitCode::SUCCESS
                } else {
                    exit_with(Exit::Regression)
                });
            }
            if record {
                // Run ONCE with full evidence + annotated video (was `run`).
                let name = repro
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("--record needs a repro id or alias"))?;
                let journey = resolve_repro_journey(&loaded.root, &name)?;
                let outcome = orchestrator::run_journey(
                    &loaded.config,
                    &loaded.root,
                    &journey,
                    &orchestrator::RunOpts {
                        kind: kind.as_deref(),
                        devices,
                        warm,
                        shots_dir: shots_dir.as_deref(),
                        profile,
                        ..Default::default()
                    },
                )
                .await?;
                return Ok(if outcome.passed {
                    ExitCode::SUCCESS
                } else {
                    exit_with(Exit::Regression)
                });
            }
            if flicker {
                // Record the repro once, then scan the video frame-to-frame for
                // transient render glitches. A single-shot evidence run like
                // --record/--visual.
                let name = repro
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("--flicker needs a repro id or alias"))?;
                let journey = resolve_repro_journey(&loaded.root, &name)?;
                let outcome = orchestrator::run_journey(
                    &loaded.config,
                    &loaded.root,
                    &journey,
                    &orchestrator::RunOpts {
                        kind: kind.as_deref(),
                        devices,
                        warm,
                        shots_dir: shots_dir.as_deref(),
                        profile,
                        ..Default::default()
                    },
                )
                .await?;
                let events =
                    flicker::analyze_run(&outcome.run_dir, &flicker::FlickerCfg::default()).await?;
                let clean = flicker::report(&events);
                return Ok(if clean {
                    ExitCode::SUCCESS
                } else {
                    exit_with(Exit::Regression)
                });
            }
            let times = runs.unwrap_or(loaded.config.gate.runs).max(1);
            // A scripted journey (journeys/<name>.yaml) is a first-class check
            // target. If the name is not a saved repro or a pending finding but a
            // journey file exists, run it as a journey (repros win a name clash).
            if let Some(r) = &repro {
                if repro::resolve(&loaded.root, r).is_none()
                    && find_finding_by_id(&loaded, r).is_none()
                    && journey::exists(&loaded.root, r)
                {
                    let result = journey::run(&loaded, r, times, ctx.json || ctx.quiet).await?;
                    if ctx.json {
                        ctx.emit(&serde_json::json!({
                            "command": "check",
                            "journey": r,
                            "outcome": result.outcome.as_str(),
                            "rate": result.rate(),
                            "exit": result.outcome.exit_code(),
                        }));
                    } else {
                        ctx.say(format!(
                            "\ncheck: {} ({})  journey {r}",
                            result.outcome.as_str().to_uppercase(),
                            result.rate()
                        ));
                    }
                    return Ok(ExitCode::from(result.outcome.exit_code()));
                }
            }
            // `check` with no name = run the whole saved suite; aggregate worst.
            let metas = match &repro {
                // A kept repro (id or alias) first; failing that, a PENDING fuzz
                // finding by id, so you can `check <id>` to confirm a finding
                // replays before you `keep` it.
                Some(r) => vec![match repro::resolve(&loaded.root, r) {
                    Some(m) => m,
                    None => find_finding_by_id(&loaded, r)
                        .map(|f| f.pending_meta())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "no repro or finding `{r}` (by id or alias). List saved repros with `reproit repros`, or find some with `reproit fuzz`."
                            )
                        })?,
                }],
                None => {
                    let all = repro::list(&loaded.root);
                    if all.is_empty() {
                        if ctx.json {
                            ctx.emit(&serde_json::json!({
                                "command": "check",
                                "repros": [],
                                "outcome": "pass",
                                "exit": 0,
                            }));
                            return Ok(ExitCode::SUCCESS);
                        }
                        anyhow::bail!(
                            "no repros to check. Find some with `reproit fuzz`, then `reproit keep`."
                        );
                    }
                    all
                }
            };

            let mut results = Vec::new();
            let mut worst = repro::Outcome::Pass;
            let mut cases: Vec<junit::Case> = Vec::new();
            // Locale runs: either one app-default pass (None) or one pass per
            // locale. For the cross-locale diff we record, per repro id, the set
            // of locales it FAILED in (fail/flaky/stale), so a repro red in one
            // locale and green in another is flagged as a locale-specific bug.
            let locale_runs: Vec<Option<&str>> = if locales.is_empty() {
                vec![None]
            } else {
                locales.iter().map(|l| Some(l.as_str())).collect()
            };
            let mut failed_by_id: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for loc in &locale_runs {
                if let Some(l) = loc {
                    ctx.say(format!("\n=== locale {l} ==="));
                }
                for meta in &metas {
                    let label = match loc {
                        Some(l) => format!("{} @{l}", repro_label(meta)),
                        None => repro_label(meta),
                    };
                    ctx.say(format!("check {label}"));
                    let (result, run_dir) = check_repro(
                        &loaded,
                        &meta.id,
                        times,
                        devices,
                        kind.as_deref(),
                        *loc,
                        ctx.json || ctx.quiet,
                        None,
                    )
                    .await?;
                    worst = worst.max(result.outcome);
                    if result.outcome != repro::Outcome::Pass {
                        if let Some(l) = loc {
                            failed_by_id
                                .entry(meta.id.clone())
                                .or_default()
                                .push(l.to_string());
                        }
                    }
                    cases.push(junit::Case {
                        name: format!("check {label}"),
                        passed: result.outcome == repro::Outcome::Pass,
                        time_s: 0.0,
                        message: format!(
                            "{} ({}); evidence: {}",
                            result.outcome.as_str(),
                            result.rate(),
                            run_dir.display()
                        ),
                    });
                    // Auto-promote: the first time a quarantined repro passes, it
                    // becomes required (write meta).
                    let mut updated = meta.clone();
                    updated.last_checked = Some(chrono::Local::now().to_rfc3339());
                    updated.last_result = Some(result.outcome.as_str().to_string());
                    let mut promoted = false;
                    if result.outcome == repro::Outcome::Pass
                        && meta.status == repro::Status::Quarantined
                    {
                        updated.status = repro::Status::Required;
                        promoted = true;
                    }
                    repro::save_meta(&loaded.root, &updated)?;
                    ctx.say(format!(
                        "  {} {} ({}){}",
                        result.outcome.as_str().to_uppercase(),
                        label,
                        result.rate(),
                        if promoted {
                            "  promoted -> required"
                        } else {
                            ""
                        }
                    ));
                    results.push(serde_json::json!({
                        "id": meta.id,
                        "alias": meta.alias,
                        "locale": loc,
                        "outcome": result.outcome.as_str(),
                        "rate": result.rate(),
                        "green": result.green,
                        "total": result.total,
                        "status": updated.status.as_str(),
                        "promoted": promoted,
                        "exit": result.outcome.exit_code(),
                        "evidence": run_dir.to_string_lossy(),
                    }));
                }
            }
            // Cross-locale diff: a repro that failed in SOME locales but not all
            // is a locale-specific (i18n) finding.
            if locale_runs.len() > 1 {
                let mut any = false;
                for meta in &metas {
                    if let Some(failed) = failed_by_id.get(&meta.id) {
                        if failed.len() < locale_runs.len() {
                            if !any {
                                ctx.say("\nlocale diff: locale-specific failures (i18n):");
                                any = true;
                            }
                            ctx.say(format!(
                                "  {} fails only in: {}",
                                repro_label(meta),
                                failed.join(", ")
                            ));
                        }
                    }
                }
                if !any {
                    ctx.say("\nlocale diff: no locale-specific failures");
                }
            }
            if let Some(path) = junit.as_deref() {
                if let Err(e) = junit::write(path, "check", &cases) {
                    ctx.say(format!(
                        "  warn: could not write junit {}: {e}",
                        path.display()
                    ));
                } else {
                    ctx.say(format!("  junit: {}", path.display()));
                }
            }
            ctx.emit(&serde_json::json!({
                "command": "check",
                "repros": results,
                "outcome": worst.as_str(),
                "exit": worst.exit_code(),
            }));
            ctx.say(format!(
                "\ncheck: {} ({} repro(s))",
                worst.as_str().to_uppercase(),
                metas.len()
            ));
            Ok(exit_with(Exit::from(worst)))
        }
        Cmd::Keep {
            id,
            as_name,
            strict,
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            keep_repro(&ctx, &loaded, id.as_deref(), as_name.as_deref(), strict)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Simplify { repro, to } => {
            let loaded = config::load(cli.config.as_deref())?;
            // Reference repro (kept or a pending finding) for its oracle + meta.
            let meta = repro::resolve(&loaded.root, &repro)
                .or_else(|| find_finding_by_id(&loaded, &repro).map(|f| f.pending_meta()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no repro or finding `{repro}` (by id or alias). List them with `reproit repros`."
                    )
                })?;
            let current = load_repro_actions(&loaded, &meta.id)?;
            let parsed: Vec<String> = serde_json::from_str(&to)
                .map_err(|e| anyhow::anyhow!("--to must be a JSON array of action strings: {e}"))?;
            let candidate = repro::normalize_actions(&parsed);
            if candidate.is_empty() {
                anyhow::bail!("--to is empty");
            }
            // VERIFY the candidate reproduces the same finding (deterministic).
            let times = loaded.config.gate.runs.max(1);
            let (result, _) = check_repro(
                &loaded,
                &meta.id,
                times,
                1,
                None,
                None,
                ctx.json || ctx.quiet,
                Some(&candidate),
            )
            .await?;
            let reproduces = result.outcome == repro::Outcome::Fail;
            let new_id = repro::repro_id(meta.seed, &candidate);
            // Adopt only a verified, no-longer, genuinely-different candidate.
            let adopt = reproduces && candidate.len() <= current.len() && new_id != meta.id;
            if adopt {
                adopt_simplified(&loaded, &meta, &candidate, &new_id)?;
            }
            if ctx.json {
                ctx.emit(&serde_json::json!({
                    "command": "simplify",
                    "repro": meta.id,
                    "reproduces": reproduces,
                    "verdict": result.outcome.as_str(),
                    "from_actions": current.len(),
                    "to_actions": candidate.len(),
                    "adopted": adopt,
                    "new_id": adopt.then(|| new_id.clone()),
                    "alias": meta.alias,
                }));
            } else if adopt {
                let tag = meta
                    .alias
                    .as_deref()
                    .map(|a| format!(" [{a}]"))
                    .unwrap_or_default();
                ctx.say(format!(
                    "  simplified {} ({} actions) -> {} ({} actions){tag}",
                    meta.id,
                    current.len(),
                    new_id,
                    candidate.len()
                ));
            } else if !reproduces {
                ctx.say(format!(
                    "  candidate did NOT reproduce (verdict: {}); kept {}",
                    result.outcome.as_str(),
                    meta.id
                ));
            } else {
                ctx.say(format!(
                    "  candidate reproduces but is not shorter ({} vs {}); kept {}",
                    candidate.len(),
                    current.len(),
                    meta.id
                ));
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Repros => {
            let loaded = config::load(cli.config.as_deref())?;
            let metas = repro::list(&loaded.root);
            if ctx.json {
                let items: Vec<serde_json::Value> = metas
                    .iter()
                    .map(|m| {
                        // The action sequence too, so an agent can see what to
                        // simplify (reproit_simplify) without a second call.
                        let actions = load_repro_actions(&loaded, &m.id).unwrap_or_default();
                        serde_json::json!({
                            "id": m.id,
                            "alias": m.alias,
                            "status": m.status.as_str(),
                            "seed": m.seed,
                            "created": m.created,
                            "last_checked": m.last_checked,
                            "last_result": m.last_result,
                            "actions": actions,
                        })
                    })
                    .collect();
                ctx.emit(&serde_json::json!({ "command": "repros", "repros": items }));
                return Ok(ExitCode::SUCCESS);
            }
            if metas.is_empty() {
                ctx.say("no saved repros. Find some with `reproit fuzz`, then `reproit keep`.");
            } else {
                ctx.say(format!(
                    "  {:<14} {:<18} {:<12} {}",
                    "ID", "ALIAS", "STATUS", "LAST CHECK"
                ));
                for m in &metas {
                    ctx.say(format!(
                        "  {:<14} {:<18} {:<12} {}",
                        m.id,
                        m.alias.as_deref().unwrap_or("-"),
                        m.status.as_str(),
                        m.last_result.as_deref().unwrap_or("never"),
                    ));
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Watch { repro } => {
            let loaded = config::load(cli.config.as_deref())?;
            let video = resolve_repro_video(&loaded, &repro)?;
            if ctx.json {
                ctx.emit(&serde_json::json!({
                    "command": "watch",
                    "id": repro,
                    "video": video.display().to_string(),
                }));
                return Ok(ExitCode::SUCCESS);
            }
            open_in_player(&video)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Fix { run, a11y } => {
            let loaded = config::load(cli.config.as_deref())?;
            if a11y {
                fix::fix_a11y(&loaded.config, &loaded.root).await?;
            } else {
                fix::fix(&loaded.config, &loaded.root, run.as_deref()).await?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Analyze { run } => {
            let loaded = config::load(cli.config.as_deref())?;
            analyze::analyze(&loaded.config, &loaded.root, run.as_deref()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Fuzz {
            journey,
            seed,
            runs,
            budget,
            shrink,
            all,
            frontier,
            from,
            uniform,
            seeds,
            batch,
            profile_timing,
            sim,
            confirm_on_sim,
            cloud,
            app,
            app_idx,
            post_comment,
            soak,
            cycle,
            repeats,
            warm,
            target,
            url,
            headless,
            out,
            replay,
            locale,
            only,
            no_oracles,
            device,
            target_arg,
        } => {
            // The positional TARGET is auto-classified. A URL (https://app.com,
            // or a bare google.com / localhost:3000) points reproit at a deployed
            // app with no reproit.yaml: synthesize a web config rooted at the cwd
            // (so `.reproit/` lands here) and auto-build the map so fuzz has a
            // graph. Anything else (e.g. "login") scopes the hunt to that alias.
            let target_url = target_arg.as_deref().and_then(target_as_url);
            let loaded = if let Some(u) = &target_url {
                let wrd = config::resolve_web_runner_dir()?;
                ctx.say(format!("zero-config web run against {u}"));
                let l = config::synthesize_web(u, &wrd, std::env::current_dir()?)?;
                if !l.root.join(".reproit/appmap.json").exists() {
                    ctx.say("  building the app map (first run; re-run is faster)...");
                    map::build_map(&l.config, &l.root, &journey, false, None).await?;
                }
                l
            } else {
                config::load(cli.config.as_deref())?
            };
            // A non-URL positional scopes the hunt to that alias/node.
            let journey = match &target_arg {
                Some(t) if target_url.is_none() => t.clone(),
                _ => journey,
            };
            // `--soak`: the leak oracle (was `soak`).
            if soak {
                let cycle = cycle
                    .ok_or_else(|| anyhow::anyhow!("--soak needs --cycle \"tap:A;tap:B;...\""))?;
                let args = soak::SoakArgs {
                    journey,
                    cycle,
                    repeats,
                    warm,
                };
                let leak = soak::soak(&loaded.config, &loaded.root, &args).await?;
                return Ok(if leak {
                    exit_with(Exit::Regression)
                } else {
                    ExitCode::SUCCESS
                });
            }
            // The web-engine cross-engine env (URL + headless) travels to the
            // web runner via process env, set per engine inside `run_targets`.
            // `--out`/`--replay` were the old standalone `differential.mjs`
            // surface; the unified routing diffs from each engine's fuzz run, so
            // they no longer apply (kept as accepted flags for compatibility).
            let _ = (&out, &replay);
            if is_web_engines(target.as_deref().unwrap_or("")) {
                let url = url.or_else(|| loaded.config.app.url.clone());
                if let Some(u) = url {
                    std::env::set_var("REPROIT_URL", u);
                }
                std::env::set_var("REPROIT_HEADLESS", if headless { "1" } else { "0" });
            }
            // Oracle filter (--only/--no) and locale list (--locale), shared by
            // every target run below.
            let (oracle_filter, unknown) =
                crosscut::OracleFilter::build(only.as_deref(), no_oracles.as_deref());
            for u in &unknown {
                ctx.say(format!("  warn: unknown oracle category `{u}` (ignored)"));
            }
            let locales = locale
                .as_deref()
                .map(crosscut::parse_locales)
                .unwrap_or_default();

            // `--from <journey>`: resolve the journey to its replay actions
            // host-side now, so a bad/multi-actor journey fails before any drive
            // (and the secret/map resolution happens once, not per seed).
            let from_prefix = match &from {
                Some(name) => Some(journey::prefix_actions(&loaded, name)?),
                None => None,
            };

            let args = fuzz::FuzzArgs {
                journey,
                seed,
                runs,
                budget,
                shrink,
                all,
                frontier,
                uniform,
                seeds_file: seeds,
                batch,
                profile_timing,
                sim,
                confirm_on_sim,
                cloud,
                app,
                app_idx,
                post_comment,
                json: ctx.json,
                locales,
                oracle_filter,
                from_prefix,
            };

            // UNIFIED --target dispatch: ONE path for web ENGINES
            // (chromium/firefox/webkit -> cross-engine differential) AND
            // PLATFORMS (ios|android|web|all -> per-device run). A single target
            // routes the run to its driver; a list runs EACH and diffs for
            // divergence (a finding on a subset of targets, not all).
            let run_targets_parsed = target.as_deref().map(crosscut::parse_run_targets);
            if let Some((targets, unknown_t)) = run_targets_parsed {
                for u in &unknown_t {
                    ctx.say(format!("  warn: unknown target `{u}` (ignored)"));
                }
                if !targets.is_empty() {
                    return run_targets(&ctx, &loaded, &targets, device.as_deref(), args).await;
                }
            } else if device.is_none()
                && !ctx.yes
                && std::io::IsTerminal::is_terminal(&std::io::stdin())
                && run_needs_device_pick(&loaded.config.app.platform, sim)
            {
                // No --target / --device on a TTY, AND the run needs a device.
                // The headless tier (flutter `flutter test`, web CDP) uses none,
                // so prompting there is vestigial; fall through to the headless
                // default run instead. Offer the interactive picker.
                if let Some(dev) = pick_device_interactive(None).await {
                    ctx.say(format!("  selected {} ({})", dev.name, dev.target.as_str()));
                    return run_targets(
                        &ctx,
                        &loaded,
                        &[crosscut::RunTarget::Platform(dev.target)],
                        Some(&dev.id),
                        args,
                    )
                    .await;
                }
            }

            fuzz::fuzz(&loaded.config, &loaded.root, &args).await?;
            // --json: surface the findings artifact (the discovered repro, by
            // content-hash id, plus its seed/actions) so the agent/MCP bridge
            // can keep it without re-parsing the human report.
            if ctx.json {
                match latest_finding(&loaded) {
                    Some(f) => ctx.emit(&serde_json::json!({
                        "command": "fuzz",
                        "found": true,
                        "id": f.id(),
                        "seed": f.seed,
                        "actions": f.actions,
                        "artifact": f.run_dir.to_string_lossy(),
                    })),
                    None => ctx.emit(&serde_json::json!({
                        "command": "fuzz",
                        "found": false,
                    })),
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Mcp => {
            mcp::serve(cli.config.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Platforms => {
            print_platforms();
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Skills { action } => {
            match action {
                SkillsAction::Install {
                    format,
                    global,
                    dir,
                } => skills::install(format, global, dir)?,
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Secrets { action } => {
            auth_cmd(cli.config.as_deref(), action)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Journey { action } => {
            journey_cmd(cli.config.as_deref(), action, &ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Screenshots {
            tour,
            out,
            locale,
            target,
            device,
            no_verify,
            path_template,
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            let locales = locale
                .as_deref()
                .map(crosscut::parse_locales)
                .unwrap_or_default();
            let (targets, unknown) = match target.as_deref() {
                Some(t) => crosscut::parse_run_targets(t),
                None => (Vec::new(), Vec::new()),
            };
            for u in unknown {
                ctx.say(format!("  warn: unknown target `{u}` (ignored)"));
            }
            let devices: Vec<String> = device
                .as_deref()
                .map(|s| {
                    s.split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let args = screenshots::Args {
                tour,
                out,
                locales,
                targets,
                devices,
                verify: if no_verify { Some(false) } else { None },
                path_template,
            };
            let passed = screenshots::run(&ctx, &loaded, args).await?;
            Ok(if passed {
                ExitCode::SUCCESS
            } else {
                exit_with(Exit::Regression)
            })
        }
        Cmd::Import {
            tool,
            path,
            name,
            out,
        } => {
            import::run(&ctx, &tool, &path, name, out.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Cloud { action } => {
            // Cloud commands talk to a remote; an unreachable/erroring cloud is
            // a clean, non-panicking failure with a one-line message (the full
            // chain stays available under --json for scripts).
            match cloud_cmd(cli.config.as_deref(), action, ctx.json).await {
                Ok(()) => Ok(ExitCode::SUCCESS),
                Err(e) => {
                    if ctx.json {
                        ctx.emit(&serde_json::json!({
                            "command": "cloud",
                            "ok": false,
                            "error": e.to_string(),
                        }));
                    } else {
                        eprintln!("cloud: {e}");
                        eprintln!("  (is the cloud reachable? check REPROIT_CLOUD_URL / `reproit cloud login`)");
                    }
                    Ok(exit_with(Exit::Regression))
                }
            }
        }
        Cmd::TuiRun => {
            tui::run()?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Devices => {
            let loaded = config::load(cli.config.as_deref())?;
            let sims = simctl::list_sims(&loaded.config.devices.name_prefix).await;
            if sims.is_empty() {
                println!(
                    "no simulators named {}-*",
                    loaded.config.devices.name_prefix
                );
            }
            for (name, udid, booted) in sims {
                println!(
                    "{name}  {udid}  {}",
                    if booted { "booted" } else { "shutdown" }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Why { dir, top } => {
            let mut files = Vec::new();
            collect_cov_files(std::path::Path::new(&dir), &mut files);
            let runs: Vec<fault::RunCoverage> = files
                .iter()
                .filter_map(|p| {
                    let v: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
                    Some(fault::RunCoverage {
                        passed: v.get("passed").and_then(|x| x.as_bool()).unwrap_or(true),
                        covered: v
                            .get("covered")
                            .and_then(|x| x.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|s| s.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect();
            let failed = runs.iter().filter(|r| !r.passed).count();
            println!(
                "fault localization over {} coverage snapshot(s) ({failed} failing):",
                runs.len()
            );
            let ranked = fault::ochiai(&runs);
            if ranked.is_empty() {
                println!("  nothing to localize (no failing runs, or no coverage)");
            }
            for (elem, susp) in ranked.into_iter().take(top) {
                println!("  {susp:.3}  {elem}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Enumerate available devices across the platform's own tooling: Flutter
/// (`flutter devices --machine`), iOS sims (`xcrun simctl list devices`), and
/// Android (`adb devices`). Best-effort: a missing tool contributes nothing.
/// Returns a flat list the interactive picker numbers and the target dispatch
/// matches against.
async fn enumerate_devices() -> Vec<crosscut::Device> {
    let mut out: Vec<crosscut::Device> = Vec::new();
    // Flutter knows all three platforms; ask it first.
    let flutter = exec::run("flutter", &["devices", "--machine"]).await;
    if flutter.ok() {
        out.extend(crosscut::parse_flutter_devices(&flutter.stdout));
    }
    // iOS simulators (macOS only; the command simply fails elsewhere).
    let sims = exec::run("xcrun", &["simctl", "list", "devices"]).await;
    if sims.ok() {
        for d in crosscut::parse_simctl_devices(&sims.stdout) {
            if !out.iter().any(|e| e.id == d.id) {
                out.push(d);
            }
        }
    }
    // Android devices/emulators.
    let adb = exec::run("adb", &["devices"]).await;
    if adb.ok() {
        for d in crosscut::parse_adb_devices(&adb.stdout) {
            if !out.iter().any(|e| e.id == d.id) {
                out.push(d);
            }
        }
    }
    out
}

/// Interactive device picker: enumerate the platform's devices, print a
/// numbered list, and read a selection from stdin. When `want_name` is given,
/// match it without prompting. Returns None if there are no devices or the
/// selection is invalid/empty (the caller then falls back to the config
/// default rather than hanging).
/// Whether an interactive device picker is worth showing for this run. The
/// headless tier (flutter `flutter test`, web CDP) uses NO device, so prompting
/// for one there is vestigial noise; only pick a device when the run actually
/// needs one: an explicit `--sim` flutter run, or a platform with no headless
/// tier (native/appium). `--target`/`--device` bypass this upstream.
fn run_needs_device_pick(platform: &str, sim: bool) -> bool {
    sim || !(platform.starts_with("flutter") || platform.starts_with("web"))
}

async fn pick_device_interactive(want_name: Option<&str>) -> Option<crosscut::Device> {
    let devices = enumerate_devices().await;
    if devices.is_empty() {
        eprintln!("  no devices found (flutter/simctl/adb reported none)");
        return None;
    }
    if let Some(want) = want_name {
        return devices
            .iter()
            .find(|d| d.name == want || d.id == want)
            .cloned();
    }
    println!("Select a device:");
    for (i, d) in devices.iter().enumerate() {
        println!(
            "  {}) {} [{}] {}{}",
            i + 1,
            d.name,
            d.target.as_str(),
            d.id,
            if d.booted { "  (booted)" } else { "" }
        );
    }
    print!("  > ");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    let choice: usize = line.trim().parse().ok()?;
    if choice == 0 || choice > devices.len() {
        return None;
    }
    Some(devices[choice - 1].clone())
}

/// Resolve the device for a `check` run from `--target`/`--device`. When a
/// `--device` is given, it is matched against the enumerated list (or used
/// verbatim if not found, so an offline-but-known id still routes). When only
/// `--target` is given, the first matching device is chosen. When neither is
/// given and a TTY is present (and not --yes), the interactive picker is shown;
/// non-interactive returns None (the run falls back to the config default
/// rather than hanging on a prompt).
async fn resolve_check_device(
    ctx: &Ctx,
    platform: &str,
    target: Option<&str>,
    device: Option<&str>,
) -> Option<crosscut::Device> {
    if device.is_none() && target.is_none() {
        // Non-interactive, OR a headless-tier run that uses no device: skip the
        // prompt and let the config default (headless) stand.
        if ctx.yes
            || !std::io::IsTerminal::is_terminal(&std::io::stdin())
            || !run_needs_device_pick(platform, false)
        {
            return None;
        }
        return pick_device_interactive(None).await;
    }
    let devices = enumerate_devices().await;
    let want_target = target.and_then(crosscut::Target::parse);
    if let Some(want) = device {
        if let Some(d) = devices.iter().find(|d| d.name == want || d.id == want) {
            return Some(d.clone());
        }
        // Unknown but explicit: route it anyway under the requested (or web) target.
        return Some(crosscut::Device {
            name: want.to_string(),
            id: want.to_string(),
            target: want_target.unwrap_or(crosscut::Target::Web),
            booted: false,
        });
    }
    if let Some(t) = want_target {
        return devices
            .iter()
            .find(|d| d.target == t && d.booted)
            .or_else(|| devices.iter().find(|d| d.target == t))
            .cloned();
    }
    None
}

/// Run the saved repro suite against MULTIPLE run targets and diff which repros
/// are red on a SUBSET of targets (a cross-target divergence). Each target gets
/// its own driver invocation via the same REPROIT_PLATFORM/REPROIT_DEVICE/
/// REPROIT_ENGINE env contract as `run_targets`, ScopedEnv-restored between
/// targets. This is the `check` analog of fuzz's multi-target routing: instead
/// of finding NEW bugs it re-confirms KNOWN repros per target, and a repro that
/// fails on one target but passes on another is the divergence.
///
/// Web engines (chromium/firefox/webkit) are the validated runtime case. Mobile
/// (ios/android) exercises the routing + per-target dispatch + divergence diff,
/// but a real dual-device check needs two booted devices on the host (infra-
/// gated); without a device for a target it routes to the config default and
/// says so.
#[allow(clippy::too_many_arguments)]
async fn run_check_targets(
    ctx: &Ctx,
    loaded: &config::Loaded,
    targets: &[crosscut::RunTarget],
    device: Option<&str>,
    repro: &Option<String>,
    runs: Option<u32>,
    devices: usize,
    kind: Option<&str>,
) -> Result<ExitCode> {
    let times = runs.unwrap_or(loaded.config.gate.runs).max(1);
    // The suite: a single named repro, or every saved repro.
    let metas = match repro {
        Some(r) => vec![repro::resolve(&loaded.root, r).ok_or_else(|| {
            anyhow::anyhow!("no repro `{r}` (by id or alias). List them with `reproit repros`.")
        })?],
        None => repro::list(&loaded.root),
    };
    if metas.is_empty() {
        anyhow::bail!("no repros to check. Find some with `reproit fuzz`, then `reproit keep`.");
    }
    let all_devices = enumerate_devices().await;
    let mut worst = repro::Outcome::Pass;
    // target label -> the set of repro ids that were RED (non-pass) on it.
    let mut red_per_target: Vec<(String, std::collections::BTreeSet<String>)> = Vec::new();
    let mut results = Vec::new();
    for rt in targets {
        let label = rt.label();
        let mut env = Vec::new();
        match rt {
            crosscut::RunTarget::Engine(engine) => {
                ctx.say(format!("target {label}: web engine ({engine})"));
                env.push(("REPROIT_PLATFORM".to_string(), "web".to_string()));
                env.push(("REPROIT_ENGINE".to_string(), engine.clone()));
            }
            crosscut::RunTarget::Platform(t) => {
                let chosen = device
                    .and_then(|want| {
                        all_devices
                            .iter()
                            .find(|d| d.target == *t && (d.name == want || d.id == want))
                    })
                    .or_else(|| all_devices.iter().find(|d| d.target == *t && d.booted))
                    .or_else(|| all_devices.iter().find(|d| d.target == *t));
                match chosen {
                    Some(d) => {
                        ctx.say(format!("target {label}: device {} ({})", d.name, d.id));
                        env.push(("REPROIT_DEVICE".to_string(), d.id.clone()));
                    }
                    None => ctx.say(format!(
                        "target {label}: no device found; using the config default platform \
                         (real dual-device check needs a booted {label} device)"
                    )),
                }
                env.push(("REPROIT_PLATFORM".to_string(), t.as_str().to_string()));
            }
        }
        let _guard = ScopedEnv::set(env);
        ctx.say(format!("\n=== target {label} ==="));
        let mut red = std::collections::BTreeSet::new();
        for meta in &metas {
            let lbl = repro_label(meta);
            let (result, run_dir) = check_repro(
                loaded,
                &meta.id,
                times,
                devices,
                kind,
                None,
                ctx.json || ctx.quiet,
                None,
            )
            .await?;
            worst = worst.max(result.outcome);
            if result.outcome != repro::Outcome::Pass {
                red.insert(meta.id.clone());
            }
            ctx.say(format!(
                "  {} {} ({})",
                result.outcome.as_str().to_uppercase(),
                lbl,
                result.rate()
            ));
            results.push(serde_json::json!({
                "id": meta.id,
                "target": label,
                "outcome": result.outcome.as_str(),
                "rate": result.rate(),
                "evidence": run_dir.to_string_lossy(),
            }));
        }
        red_per_target.push((label, red));
        // _guard drops here, restoring the prior env.
    }
    // Divergence: a repro red on a subset of targets (not all) is a divergence.
    let diverging = crosscut::cross_target_divergence(&red_per_target);
    if diverging.is_empty() {
        ctx.say("\ndivergence: none (every repro behaves the same on all targets)");
    } else {
        ctx.say("\ndivergence: repros that differ across targets:");
        for (id, on) in &diverging {
            let label = metas
                .iter()
                .find(|m| &m.id == id)
                .map(repro_label)
                .unwrap_or_else(|| id.clone());
            ctx.say(format!("  {label} fails only on: {}", on.join(", ")));
        }
    }
    ctx.emit(&serde_json::json!({
        "command": "check",
        "repros": results,
        "outcome": worst.as_str(),
        "exit": worst.exit_code(),
        "divergence": diverging
            .iter()
            .map(|(id, on)| serde_json::json!({ "id": id, "fails_only_on": on }))
            .collect::<Vec<_>>(),
    }));
    ctx.say(format!(
        "\ncheck: {} ({} repro(s) x {} target(s))",
        worst.as_str().to_uppercase(),
        metas.len(),
        targets.len()
    ));
    Ok(exit_with(Exit::from(worst)))
}

/// Run `fuzz` against one or more run targets, routing each to its own driver
/// and diffing for divergence when more than one target is given. ONE path now
/// handles both web ENGINES and PLATFORMS (see `crosscut::RunTarget`):
///
///   - `RunTarget::Engine(e)` (chromium/firefox/webkit) routes through the web
///     backend (the WebCdp runner reads `REPROIT_ENGINE`), forcing the web
///     platform for the run. `fuzz --target chromium,firefox,webkit` thus runs
///     the SAME seeded walk on each engine and diffs the findings. This is the
///     validated runtime case.
///   - `RunTarget::Platform(t)` (ios/android/web) resolves a device from the
///     platform's own device list (simctl/adb/flutter) and runs the loop on it.
///     For mobile (ios/android) the device-resolution + per-target dispatch +
///     divergence diff are exercised here and unit-tested, but a full
///     dual-REAL-device run is infra-gated: it needs two booted devices
///     (a simulator/emulator or a tethered handset) present on the host. When
///     none is found we fall back to the config default platform and say so.
///
/// Every target's run gets a SEPARATE driver invocation: the per-target env
/// (REPROIT_PLATFORM / REPROIT_DEVICE / REPROIT_ENGINE) is set, the run loop
/// executes, then the env is restored so a later target never sees a stale
/// value. With multiple targets, a finding signature present on a SUBSET of
/// targets (some but not all) is reported as a divergence.
async fn run_targets(
    ctx: &Ctx,
    loaded: &config::Loaded,
    targets: &[crosscut::RunTarget],
    device: Option<&str>,
    base: fuzz::FuzzArgs,
) -> Result<ExitCode> {
    let devices = enumerate_devices().await;
    // label -> the set of finding signatures it produced, for the divergence diff.
    let mut per_target: Vec<(String, std::collections::BTreeSet<String>)> = Vec::new();
    for rt in targets {
        let label = rt.label();
        // Build this target's per-run env. RAII-restored after the run (Drop) so
        // a panic mid-target cannot leak a stale REPROIT_* into the next target.
        let mut env = Vec::new();
        match rt {
            crosscut::RunTarget::Engine(engine) => {
                // Cross-engine differential: force the web platform and select
                // the engine. The web runner reads REPROIT_ENGINE; REPROIT_URL
                // carries the page (flag/config). Headless is the CI default.
                ctx.say(format!("target {label}: web engine ({engine})"));
                env.push(("REPROIT_PLATFORM".to_string(), "web".to_string()));
                env.push(("REPROIT_ENGINE".to_string(), engine.clone()));
            }
            crosscut::RunTarget::Platform(t) => {
                // Resolve the device for this platform: an explicit --device that
                // belongs to it, else the first booted device, else the first
                // device. None -> config default platform (mobile dual-real-device
                // runtime is infra-gated; see the doc comment).
                let chosen = device
                    .and_then(|want| {
                        devices
                            .iter()
                            .find(|d| d.target == *t && (d.name == want || d.id == want))
                    })
                    .or_else(|| devices.iter().find(|d| d.target == *t && d.booted))
                    .or_else(|| devices.iter().find(|d| d.target == *t));
                match chosen {
                    Some(d) => {
                        ctx.say(format!("target {label}: device {} ({})", d.name, d.id));
                        env.push(("REPROIT_DEVICE".to_string(), d.id.clone()));
                    }
                    None => ctx.say(format!(
                        "target {label}: no device found; using the config default platform \
                         (real dual-device runtime needs a booted {label} device)"
                    )),
                }
                env.push(("REPROIT_PLATFORM".to_string(), t.as_str().to_string()));
            }
        }
        let _guard = ScopedEnv::set(env);
        ctx.say(format!("\n=== target {label} ==="));
        let sigs = fuzz::fuzz_targeted(&loaded.config, &loaded.root, &base).await?;
        per_target.push((label, sigs));
        // _guard drops here, restoring the prior env.
    }
    report_divergence(ctx, &per_target);
    Ok(ExitCode::SUCCESS)
}

/// Print the cross-target divergence report from per-target finding signatures.
/// A finding on every target is consistent; a finding on a subset is divergence.
fn report_divergence(ctx: &Ctx, per_target: &[(String, std::collections::BTreeSet<String>)]) {
    if per_target.len() < 2 {
        return;
    }
    let diverging = crosscut::cross_target_divergence(per_target);
    if diverging.is_empty() {
        ctx.say("\ndivergence: none (every finding reproduces on all targets)");
    } else {
        ctx.say("\ndivergence: findings that differ across targets:");
        for (sig, on) in &diverging {
            ctx.say(format!("  [{}] only on: {}", sig, on.join(", ")));
        }
    }
}

/// RAII guard that sets a batch of env vars and restores each to its prior value
/// (or removes it when it was previously unset) on Drop, so a per-target run's
/// REPROIT_* never leaks into the next target even on early return or panic.
pub(crate) struct ScopedEnv {
    prior: Vec<(String, Option<String>)>,
}

impl ScopedEnv {
    pub(crate) fn set(vars: Vec<(String, String)>) -> Self {
        let mut prior = Vec::with_capacity(vars.len());
        for (k, v) in vars {
            prior.push((k.clone(), std::env::var(&k).ok()));
            std::env::set_var(&k, &v);
        }
        ScopedEnv { prior }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (k, was) in self.prior.drain(..) {
            match was {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
    }
}

/// Whether a `--target` string names ONLY web browser engines (so `fuzz`/`check
/// --target` routes to the cross-engine differential). A list is web-engine iff
/// every non-empty token is an engine alias; a bare `web` is a platform token.
fn is_web_engines(target: &str) -> bool {
    let toks: Vec<&str> = target
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    !toks.is_empty() && toks.iter().all(|t| crosscut::is_web_engine_token(t))
}

/// Resolve the effective cloud (url, key) for a cloud subcommand: an explicit
/// flag wins, then the env (REPROIT_CLOUD_URL / REPROIT_API_KEY), then the
/// persisted token from `cloud login` (~/.reproit/token). This is the single
/// place the persisted token is read so every `cloud` command honors it.
fn cloud_creds(cloud: Option<String>, key: Option<String>) -> (Option<String>, Option<String>) {
    let persisted = crosscut::load_token(&crosscut::token_path());
    let url = cloud
        .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
        .or_else(|| persisted.as_ref().and_then(|(_, u)| u.clone()));
    let key = key
        .or_else(|| std::env::var("REPROIT_API_KEY").ok())
        .or_else(|| persisted.as_ref().map(|(t, _)| t.clone()));
    (url, key)
}

/// Dispatch the `cloud` subcommands onto the existing triage::*/deliver::*
/// handlers. `login` persists a service token; every other command resolves the
/// token via `cloud_creds` and uses it as a bearer. Network failures surface as
/// a clear message (the triage layer bails rather than panicking).
async fn cloud_cmd(
    config_path: Option<&std::path::Path>,
    action: CloudAction,
    json: bool,
) -> Result<()> {
    match action {
        CloudAction::Login { cloud, key } => {
            let url = cloud
                .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
                .unwrap_or_else(|| "https://cloud.reproit.com".into());
            let token = key.or_else(|| std::env::var("REPROIT_API_KEY").ok());
            let Some(token) = token else {
                anyhow::bail!(
                    "no service token: pass --key or set REPROIT_API_KEY (get one from the cloud dashboard)"
                );
            };
            let path = crosscut::token_path();
            crosscut::save_token(&path, &token, &url)?;
            println!("cloud url:     {url}");
            println!(
                "service token: stored ({} chars) in {}",
                token.len(),
                path.display()
            );
            // Best-effort validation: a GET that needs auth. A failure is a
            // warning, not an error (the token is still saved for later use).
            let probe = triage::ping(&url, Some(&token)).await;
            match probe {
                Ok(()) => println!("validated:     ok (cloud reachable, token accepted)"),
                Err(e) => println!("validated:     warn: could not verify token now ({e})"),
            }
            Ok(())
        }
        CloudAction::Fuzz {
            app,
            journey,
            pr,
            cloud,
            idx,
        } => {
            // Submit a job via the existing fuzz cloud-delivery path: set
            // --cloud + --app, post the PR comment when a PR is linked.
            let loaded = config::load(config_path)?;
            let cloud = cloud.or_else(|| std::env::var("REPROIT_CLOUD_URL").ok());
            if let Some(pr) = pr {
                // PR linking is automatic in the delivery pipeline; record it.
                println!("  linking to PR #{pr}");
            }
            let args = fuzz::FuzzArgs {
                journey,
                seed: 1,
                runs: 3,
                budget: 40,
                shrink: false,
                // Cloud buckets findings server-side; the local run delivers one.
                all: false,
                frontier: false,
                uniform: false,
                seeds_file: None,
                batch: 0,
                profile_timing: false,
                sim: false,
                confirm_on_sim: false,
                cloud,
                app: Some(app),
                app_idx: idx,
                post_comment: pr.is_some(),
                json: false,
                locales: Vec::new(),
                oracle_filter: crosscut::OracleFilter::all(),
                from_prefix: None,
            };
            fuzz::fuzz(&loaded.config, &loaded.root, &args).await
        }
        CloudAction::Buckets {
            app,
            query,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::buckets(&app, query.as_deref(), json, cloud, key).await
        }
        CloudAction::Findings {
            app,
            query,
            export,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            if export {
                // Raw findings JSON straight from GET /v1/errors/:app.
                let v = triage::raw(&app, "", cloud, key).await?;
                println!("{}", serde_json::to_string_pretty(&v)?);
                Ok(())
            } else {
                triage::find(&app, query.as_deref(), cloud, key).await
            }
        }
        CloudAction::BlastRadius {
            app,
            sig,
            idx,
            export,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            if export {
                // Raw cohorts JSON from GET /v1/errors/:app/cohorts.
                let v = triage::raw(&app, "/cohorts", cloud, key).await?;
                println!("{}", serde_json::to_string_pretty(&v)?);
                Ok(())
            } else {
                triage::explain(&app, sig.as_deref(), idx, cloud, key).await
            }
        }
        CloudAction::Reproduce {
            app,
            idx,
            journey,
            run,
            export,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            if export {
                // Raw repro JSON from GET /v1/errors/:app/:idx/repro.
                let v = triage::raw(&app, &format!("/{idx}/repro"), cloud, key).await?;
                println!("{}", serde_json::to_string_pretty(&v)?);
                Ok(())
            } else {
                triage::reproduce(&app, idx, &journey, run, cloud, key).await
            }
        }
        CloudAction::Pull {
            app,
            bucket,
            idx,
            as_name,
            cloud,
            key,
        } => {
            if bucket.is_none() && idx.is_none() {
                anyhow::bail!("cloud pull needs either --bucket <id> or --idx <n>");
            }
            // Resolve the local repro store root so the pulled repro lands as a
            // first-class saved repro under .reproit/repros/, just like `keep`.
            let loaded = config::load(config_path)?;
            let (cloud, key) = cloud_creds(cloud, key);
            triage::pull(
                &loaded.root,
                &app,
                bucket.as_deref(),
                idx,
                &as_name,
                cloud,
                key,
            )
            .await
        }
        CloudAction::Triage {
            app,
            bucket,
            status,
            fixed_in_build,
            assignee,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::triage(
                &app,
                &bucket,
                status.as_deref(),
                fixed_in_build.as_deref(),
                assignee,
                json,
                cloud,
                key,
            )
            .await
        }
        CloudAction::ResolutionEvents { app, cloud, key } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::resolution_events(&app, json, cloud, key).await
        }
        CloudAction::Timeline {
            app,
            bucket,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::timeline(&app, &bucket, json, cloud, key).await
        }
        CloudAction::Diagnose {
            app,
            report,
            run,
            journey,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::diagnose(&app, &report, run, &journey, cloud, key).await
        }
        CloudAction::Query {
            app,
            query,
            export,
            cloud,
            key,
        } => {
            // Raw data out for your own analysis: GET /v1/errors/:app, filtered
            // by --query when given. With --export, emit the raw JSON; otherwise
            // render the findings list (the data behind the view).
            let (cloud, key) = cloud_creds(cloud, key);
            if export {
                let v = triage::raw(&app, "", cloud, key).await?;
                let v = triage::filter_errors(v, query.as_deref());
                println!("{}", serde_json::to_string_pretty(&v)?);
                Ok(())
            } else {
                triage::find(&app, query.as_deref(), cloud, key).await
            }
        }
    }
}

/// A human label for a repro in CLI output: `<id> (<alias>)` when an alias is
/// set, else just the id.
fn repro_label(m: &repro::Meta) -> String {
    match &m.alias {
        Some(a) => format!("{} ({a})", m.id),
        None => m.id.clone(),
    }
}

/// One finding from a fuzz artifact: the seed, the minimized action sequence,
/// and the source `fuzz.md`'s run dir (for evidence/copying).
struct Finding {
    seed: u64,
    actions: Vec<String>,
    run_dir: PathBuf,
}

impl Finding {
    /// The content-hash id of this finding (seed + normalized actions).
    fn id(&self) -> String {
        repro::repro_id(self.seed, &self.actions)
    }

    /// An in-memory `Meta` for a finding that has NOT been kept yet, so `check`
    /// can replay it straight from the fuzz artifact (the "confirm before you
    /// keep" path). It is never written to disk: status is quarantined, with no
    /// alias and no creation stamp. The trigger index is the full minimized
    /// length (the finding fired at the end of its own sequence).
    fn pending_meta(&self) -> repro::Meta {
        repro::Meta {
            id: self.id(),
            alias: None,
            status: repro::Status::Quarantined,
            seed: self.seed,
            created: String::new(),
            last_checked: None,
            last_result: None,
            trigger_index: Some(repro::normalize_actions(&self.actions).len()),
            trigger_sig: None,
            oracle: None,
        }
    }
}

/// Find a fuzz finding by its content-hash id, scanning EVERY run dir under the
/// evidence out dir (not just the latest), so `check <id>` can confirm any
/// finding the last `fuzz` reported, before it is `keep`-ed. Returns the first
/// dir whose `fuzz.md` repro block hashes to `id`.
fn find_finding_by_id(loaded: &config::Loaded, id: &str) -> Option<Finding> {
    let base = loaded.root.join(&loaded.config.evidence.out_dir);
    for e in std::fs::read_dir(&base).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() && p.join("fuzz.md").exists() {
            if let Ok(md) = std::fs::read_to_string(p.join("fuzz.md")) {
                if let Some((seed, actions)) = parse_fuzz_report(&md) {
                    if repro::repro_id(seed, &actions) == id {
                        return Some(Finding {
                            seed,
                            actions,
                            run_dir: p,
                        });
                    }
                }
            }
        }
    }
    None
}

/// Find the latest fuzz finding artifact: the most-recent run dir under the
/// evidence out dir that holds a `fuzz.md` (the discovered repro report). The
/// out dir doubles as the fuzz artifact root (`fuzz` writes findings there).
fn latest_finding(loaded: &config::Loaded) -> Option<Finding> {
    let base = loaded.root.join(&loaded.config.evidence.out_dir);
    let mut runs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(&base).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() && p.join("fuzz.md").exists() {
            let t = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            runs.push((t, p));
        }
    }
    runs.sort_by_key(|(t, _)| *t);
    let (_, run_dir) = runs.pop()?;
    let md = std::fs::read_to_string(run_dir.join("fuzz.md")).ok()?;
    let (seed, actions) = parse_fuzz_report(&md)?;
    Some(Finding {
        seed,
        actions,
        run_dir,
    })
}

/// Parse a `fuzz.md` report into (seed, repro actions). The report header is
/// `# fuzz finding (seed N)` and the repro block is the fenced code under a
/// `## repro (...)` heading (one action per line). Pure, so it is unit-tested.
fn parse_fuzz_report(md: &str) -> Option<(u64, Vec<String>)> {
    let seed = md.lines().find_map(|l| {
        let i = l.find("(seed ")? + "(seed ".len();
        let rest = &l[i..];
        let end = rest.find(')')?;
        rest[..end].trim().parse::<u64>().ok()
    })?;
    // The repro block: the first ``` fence that follows the `## repro` heading.
    let mut in_repro_section = false;
    let mut in_fence = false;
    let mut actions = Vec::new();
    for line in md.lines() {
        if line.starts_with("## repro") {
            in_repro_section = true;
            continue;
        }
        if !in_repro_section {
            continue;
        }
        if line.trim_start().starts_with("```") {
            if in_fence {
                break; // closing fence: repro block done
            }
            in_fence = true;
            continue;
        }
        if in_fence {
            let a = line.trim();
            if !a.is_empty() {
                actions.push(a.to_string());
            }
        }
    }
    Some((seed, actions))
}

/// Parse the `## oracle` block fuzz.md emits into (oracle category, sig). The
/// block is three `- key: \`value\`` lines (oracle / invariant / sig); the sig
/// is empty for non-graph findings. Returns (None, None) when no block is
/// present (an older report), in which case `check` falls back to the crash
/// path. Pure, so it is unit-tested.
fn parse_fuzz_oracle(md: &str) -> (Option<String>, Option<String>) {
    let field = |key: &str| -> Option<String> {
        md.lines().find_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix(&format!("- {key}:"))?;
            Some(rest.trim().trim_matches('`').trim().to_string())
        })
    };
    let oracle = field("oracle").filter(|s| !s.is_empty());
    let sig = field("sig").filter(|s| !s.is_empty());
    (oracle, sig)
}

/// `keep`: take a finding from the latest fuzz artifact, compute its content
/// hash id, and write the committed store dir + meta.json. The store dir name
/// IS the content hash (stable across machines, self-deduping). Default status
/// is quarantined; `--strict` lands it required. `--as` sets the alias.
/// Video container extensions reproit's backends can emit: Playwright writes
/// `.webm`, the sim/native tier `.mov`, and the annotated delivery clip `.mp4`.
const VIDEO_EXTS: [&str; 3] = ["mp4", "mov", "webm"];

fn is_video(p: &Path) -> bool {
    match p.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let ext = ext.to_ascii_lowercase();
            VIDEO_EXTS.contains(&ext.as_str())
        }
        None => false,
    }
}

/// Resolve the recording to play for a repro, caching it into the gitignored
/// per-id media slot so future `watch`es are instant and precise.
///
/// Lookup order: the per-id media slot (`.reproit/media/<id>.*`) first; else the
/// newest recording under `.reproit/runs/` (the one you just produced with
/// `check --record`), which we then copy into the media slot. Bails with a
/// how-to if neither exists. `.reproit/media/` is gitignored, so a cached
/// recording can never be committed by accident.
fn resolve_repro_video(loaded: &config::Loaded, id_or_alias: &str) -> Result<PathBuf> {
    let root = loaded.root.as_path();
    // Key media by the canonical content-hash id (so an alias and its id share
    // one cached file); fall back to the raw arg for a pending finding.
    let id = repro::resolve(root, id_or_alias)
        .map(|m| m.id)
        .unwrap_or_else(|| id_or_alias.to_string());
    let media_dir = root.join(".reproit/media");

    // 1. Already cached for this id.
    if let Some(v) = newest_video_in(&media_dir, Some(&id)) {
        return Ok(v);
    }
    // 2. Newest recording from any run; promote it into the per-id media slot.
    if let Some(src) = newest_video_in(&root.join(".reproit/runs"), None) {
        std::fs::create_dir_all(&media_dir)?;
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("webm");
        let dest = media_dir.join(format!("{id}.{ext}"));
        std::fs::copy(&src, &dest)
            .map_err(|e| anyhow::anyhow!("caching recording to {}: {e}", dest.display()))?;
        return Ok(dest);
    }
    anyhow::bail!(
        "no recording for `{id_or_alias}`. Make one with:  reproit check {id_or_alias} --record"
    )
}

/// Newest video file under `dir` (recursively), by modification time. When
/// `stem` is set, only files whose name stem equals it are considered (the
/// per-id media slot); when None, any video counts (scanning run dirs).
fn newest_video_in(dir: &Path, stem: Option<&str>) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !is_video(&p) {
                continue;
            }
            if let Some(want) = stem {
                if p.file_stem().and_then(|s| s.to_str()) != Some(want) {
                    continue;
                }
            }
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
                best = Some((mtime, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Open a file in the OS default application (the user's video player). Uses the
/// platform opener directly so there's no extra dependency.
fn open_in_player(path: &Path) -> Result<()> {
    println!("  opening {}", path.display());
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(path).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .status()
    } else {
        std::process::Command::new("xdg-open").arg(path).status()
    };
    match result {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => anyhow::bail!(
            "the video player exited with {s} (file: {})",
            path.display()
        ),
        Err(e) => anyhow::bail!(
            "could not launch a video player ({e}). The recording is at:\n  {}",
            path.display()
        ),
    }
}

fn keep_repro(
    ctx: &Ctx,
    loaded: &config::Loaded,
    id: Option<&str>,
    as_name: Option<&str>,
    strict: bool,
) -> Result<()> {
    let root = loaded.root.as_path();
    // Resolve the finding to keep: a specific one by id (any finding the last
    // fuzz reported, so `keep <id>` pairs with `check <id>`), or the latest when
    // no id is given.
    let finding = match id {
        Some(want) => find_finding_by_id(loaded, want).ok_or_else(|| {
            anyhow::anyhow!(
                "no fuzz finding with id `{want}` under {}. List ids from the last `reproit fuzz`, \
                 or omit the id to keep the latest finding.",
                loaded.config.evidence.out_dir
            )
        })?,
        None => latest_finding(loaded).ok_or_else(|| {
            anyhow::anyhow!(
                "no fuzz finding under {}. Run `reproit fuzz` first.",
                loaded.config.evidence.out_dir
            )
        })?,
    };
    let computed = finding.id();
    let dir = repro::repro_dir(root, &computed);
    // Repros are content-addressed, so the same case keeps to the same id:
    // re-keeping is a no-op-ish "already saved" that must PRESERVE the existing
    // guard's history (status promotion, check results, created stamp, alias)
    // rather than clobber it back to a fresh quarantine.
    let existing = repro::load_meta(root, &computed);
    std::fs::create_dir_all(&dir)?;
    // Store the replay config so `check` can reproduce the case deterministically.
    let replay = serde_json::json!({ "seed": finding.seed, "replay": finding.actions });
    std::fs::write(
        dir.join("replay.json"),
        serde_json::to_string_pretty(&replay)?,
    )?;
    // Carry the discovering report for human reference (best-effort).
    let _ = std::fs::copy(finding.run_dir.join("fuzz.md"), dir.join("fuzz.md"));

    // Status: a fresh keep lands quarantined (or required with --strict); a
    // RE-keep preserves the existing status, so re-running keep never demotes a
    // guard that already went green (--strict can still upgrade it to required).
    let status = if strict {
        repro::Status::Required
    } else {
        existing
            .as_ref()
            .map(|m| m.status)
            .unwrap_or(repro::Status::Quarantined)
    };
    // Alias: an explicit `--as` sets (or renames) the alias; without it, an
    // existing alias is kept rather than wiped.
    let alias = as_name
        .map(String::from)
        .or_else(|| existing.as_ref().and_then(|m| m.alias.clone()));
    // Record the finding's TRIGGER POINT so `check` can tell "the fix changed
    // downstream navigation" (a miss AFTER the trigger -> still PASS) from "the
    // path to the bug is gone" (a miss BEFORE the trigger -> STALE). The saved
    // `actions` are the minimized sequence that LEADS TO the finding, so the
    // finding fired after performing all of them: the trigger index is that
    // count. (The fuzz report does not currently carry the trigger state sig, so
    // `trigger_sig` stays None and the index does the work.)
    let trigger_index = Some(repro::normalize_actions(&finding.actions).len());
    // Record the finding's ORACLE category and, for graph invariants, the
    // VIOLATING state sig, so `check` re-confirms the SAME finding by its oracle
    // (a graph dead-end repro re-evaluates the invariant; a crash repro keeps the
    // exception path). `keep` reads these from the `## oracle` block fuzz.md now
    // emits. This also fixes the gap the macOS run noted: keep recorded no
    // trigger_sig for graph findings.
    let md = std::fs::read_to_string(finding.run_dir.join("fuzz.md")).unwrap_or_default();
    let (oracle, finding_sig) = parse_fuzz_oracle(&md);
    // Only graph findings need the violating sig re-evaluated; crash findings use
    // the exception path and keep trigger_sig for the existing sig-reached logic.
    let trigger_sig = finding_sig.filter(|s| !s.is_empty());
    let meta = repro::Meta {
        id: computed.clone(),
        alias: alias.clone(),
        status,
        seed: finding.seed,
        // Preserve the original creation stamp on a re-keep; stamp now on a fresh
        // save.
        created: existing
            .as_ref()
            .map(|m| m.created.clone())
            .unwrap_or_else(|| chrono::Local::now().to_rfc3339()),
        last_checked: existing.as_ref().and_then(|m| m.last_checked.clone()),
        last_result: existing.as_ref().and_then(|m| m.last_result.clone()),
        trigger_index,
        trigger_sig,
        oracle,
    };
    repro::save_meta(root, &meta)?;

    // Was this already in the suite? If so, report it as "already saved" (and
    // note an alias rename) instead of pretending it's a fresh keep.
    let prior_alias = existing.as_ref().and_then(|m| m.alias.clone());
    let renamed = match (&prior_alias, as_name) {
        (Some(old), Some(new)) if old != new => Some((old.clone(), new.to_string())),
        _ => None,
    };
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "keep",
            "id": computed,
            "alias": meta.alias,
            "status": status.as_str(),
            "already_saved": existing.is_some(),
            "renamed_from": renamed.as_ref().map(|(old, _)| old.clone()),
            "seed": finding.seed,
            "actions": finding.actions,
            "dir": dir.to_string_lossy(),
        }));
    } else if existing.is_some() {
        match &renamed {
            Some((old, new)) => ctx.say(format!(
                "  already saved ({computed}); alias {old} -> {new}"
            )),
            None => {
                let label = alias.as_deref().unwrap_or(&computed);
                ctx.say(format!("  already saved as {label} ({})", status.as_str()));
            }
        }
        ctx.say(format!("  check: reproit check {computed}"));
    } else {
        ctx.say(format!("  kept {} ({})", computed, status.as_str()));
        if let Some(a) = &alias {
            ctx.say(format!("  alias: {a}"));
        }
        ctx.say(format!("  verify: reproit check {computed}"));
    }
    Ok(())
}

/// The action sequence a repro currently replays: from its committed
/// `replay.json`, or from a pending fuzz finding when it hasn't been kept yet.
fn load_repro_actions(loaded: &config::Loaded, id: &str) -> Result<Vec<String>> {
    let dir = repro::repro_dir(&loaded.root, id);
    if dir.join("replay.json").exists() {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("replay.json"))?)?;
        Ok(v.get("replay")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    } else if let Some(f) = find_finding_by_id(loaded, id) {
        Ok(f.actions)
    } else {
        anyhow::bail!("no repro or finding `{id}`")
    }
}

/// Adopt a verified, simpler action sequence AS the repro: write the new
/// content-hash store dir (carrying the alias, status, and oracle), and remove
/// the superseded one. The trigger is the candidate's full length (a clean
/// agent-proposed repro ends at the action that fires the finding).
fn adopt_simplified(
    loaded: &config::Loaded,
    meta: &repro::Meta,
    candidate: &[String],
    new_id: &str,
) -> Result<()> {
    let root = loaded.root.as_path();
    let new_dir = repro::repro_dir(root, new_id);
    std::fs::create_dir_all(&new_dir)?;
    let replay = serde_json::json!({ "seed": meta.seed, "replay": candidate });
    std::fs::write(
        new_dir.join("replay.json"),
        serde_json::to_string_pretty(&replay)?,
    )?;
    let new_meta = repro::Meta {
        id: new_id.to_string(),
        alias: meta.alias.clone(),
        status: meta.status,
        seed: meta.seed,
        created: if meta.created.is_empty() {
            chrono::Local::now().to_rfc3339()
        } else {
            meta.created.clone()
        },
        last_checked: None,
        last_result: None,
        trigger_index: Some(repro::normalize_actions(candidate).len()),
        trigger_sig: meta.trigger_sig.clone(),
        oracle: meta.oracle.clone(),
    };
    repro::save_meta(root, &new_meta)?;
    // Carry the discovering report and retire the superseded KEPT repro (a
    // pending finding has no committed dir to remove).
    let old_dir = repro::repro_dir(root, &meta.id);
    if old_dir != new_dir && old_dir.join("replay.json").exists() {
        let _ = std::fs::copy(old_dir.join("fuzz.md"), new_dir.join("fuzz.md"));
        let _ = std::fs::remove_dir_all(&old_dir);
    }
    Ok(())
}

/// Resolve the journey a kept repro replays under (for `--record`). Repros
/// replay through the explorer journey, fed the stored replay.json; the journey
/// name carried is the default explorer.
fn resolve_repro_journey(root: &std::path::Path, name: &str) -> Result<String> {
    repro::resolve(root, name)
        .ok_or_else(|| anyhow::anyhow!("no repro `{name}` (by id or alias)"))?;
    Ok("explore".to_string())
}

/// Run one repro N times and classify the aggregate outcome (pass/fail/flaky/
/// stale). Each replay writes the stored action sequence to the fuzz config the
/// explorer reads, runs the explorer on the platform's execution tier (headless
/// for Flutter, the real tier for web-cdp/appium/desktop, mirroring how `fuzz`
/// selects), and the per-run drive log is classified by
/// `repro::verdict_from_log`. Returns the result + the last run dir.
#[allow(clippy::too_many_arguments)]
async fn check_repro(
    loaded: &config::Loaded,
    id: &str,
    times: u32,
    devices: usize,
    kind: Option<&str>,
    locale: Option<&str>,
    quiet: bool,
    // When set, replay these actions INSTEAD of the repro's saved sequence, but
    // classify against the repro's oracle. This is the verify primitive behind
    // `simplify`: "does this alternate (agent-proposed) sequence still reproduce
    // the same finding?" The seed is kept; the trigger's oracle still selects the
    // crash/graph path.
    override_actions: Option<&[String]>,
) -> Result<(repro::CheckResult, PathBuf)> {
    // Crash-reporter suppression for native check replays (which can crash the
    // target app while re-confirming a crash repro). Inert for web/headless.
    // Restored on Drop, including the early `?` returns below.
    let _crash_guard = match platform::resolve(&loaded.config.app.platform) {
        Some(p) => crashreporter::CrashReporterGuard::engage(p.backend),
        None => crashreporter::CrashReporterGuard::engage_inert(),
    };
    let dir = repro::repro_dir(&loaded.root, id);
    // Replay source: a KEPT repro's store (replay.json + meta trigger) when it
    // exists, else a PENDING fuzz finding by id read straight from the artifact,
    // so `check <id>` can confirm a finding BEFORE it is `keep`-ed. For the
    // pending case the trigger is derived from the finding itself (full minimized
    // length; oracle/sig from its fuzz.md).
    let (replay, trigger): (serde_json::Value, repro::Trigger) = if dir.join("replay.json").exists()
    {
        let replay = serde_json::from_str(&std::fs::read_to_string(dir.join("replay.json"))?)?;
        // The finding's trigger context, recorded at `keep`. A repro kept
        // before this field existed loads with all None, so the classifier
        // falls back to its first-action heuristic.
        let trigger = match repro::load_meta(&loaded.root, id) {
            Some(m) => repro::Trigger {
                index: m.trigger_index,
                sig: m.trigger_sig,
                oracle: m.oracle,
            },
            None => repro::Trigger::unknown(),
        };
        (replay, trigger)
    } else if let Some(f) = find_finding_by_id(loaded, id) {
        let md = std::fs::read_to_string(f.run_dir.join("fuzz.md")).unwrap_or_default();
        let (oracle, sig) = parse_fuzz_oracle(&md);
        let replay = serde_json::json!({ "seed": f.seed, "replay": f.actions });
        let trigger = repro::Trigger {
            index: Some(repro::normalize_actions(&f.actions).len()),
            sig,
            oracle,
        };
        (replay, trigger)
    } else {
        anyhow::bail!(
                "no repro or finding `{id}`; keep it from a fuzz finding (`reproit keep`) or run `reproit fuzz` first"
            );
    };

    // Verify an alternate sequence (simplify): replace the actions, keep the seed
    // and the oracle so the verdict still answers "does this reproduce the SAME
    // finding?".
    let replay = match override_actions {
        Some(actions) => {
            let seed = replay.get("seed").cloned().unwrap_or(serde_json::json!(0));
            serde_json::json!({ "seed": seed, "replay": actions })
        }
        None => replay,
    };

    // The fuzz config the explorer reads on each replay.
    let cfg_path = loaded.root.join(".reproit/fuzz_config.json");
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    let mut defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    // LOCALE contract: the locale travels to the runner as REPROIT_LOCALE (a
    // dart-define for Flutter, an env var for the rest, both via the
    // orchestrator's define list), so a repro can be replayed under each locale.
    if let Some(loc) = locale {
        defines.push((crosscut::LOCALE_ENV.to_string(), loc.to_string()));
    }

    let _ = devices; // a repro replays on one device; kept for parity.
                     // The N repeat-replays (flakiness detection) run in a SINGLE drive session:
                     // we hand the runner a batch of N identical replays, so the browser/app
                     // launches ONCE instead of N cold starts (the agent inner loop's main
                     // latency). The runner brackets each replay with SEED:BEGIN/SEED:END, so we
                     // split the one drive log back into N per-replay segments and classify each
                     // exactly as before. A single replay (times == 1) keeps the legacy bare-config
                     // shape, byte-for-byte. This is a pure latency change: same N replays, same
                     // per-replay verdict, same determinism.
    let config = if times <= 1 {
        replay.clone()
    } else {
        serde_json::json!({ "batch": (0..times).map(|_| replay.clone()).collect::<Vec<_>>() })
    };
    std::fs::write(&cfg_path, config.to_string())?;
    // Select the execution tier the same way `fuzz` does: Flutter replays on the
    // headless tier; every other backend (web-cdp, appium, desktop) routes
    // through the real tier. Without this, web repros could never be replayed.
    let outcome = orchestrator::run_journey_tier(
        &loaded.config,
        &loaded.root,
        "explore",
        &orchestrator::RunOpts {
            kind,
            devices: 1,
            warm: false,
            extra_defines: &defines,
            ..Default::default()
        },
        false,
    )
    .await?;
    let full_log = std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
    // One segment per replay (the whole log for the single-run path).
    let segments = fuzz::split_log_segments(&full_log);
    let mut verdicts = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        // Surface each replay's REPRO verdict (did the original finding
        // reproduce?), not just the drive's PASS/FAIL completion. For
        // graph-invariant repros a drive can complete (PASS) while the finding
        // does NOT reproduce (clean), so raw PASS/FAIL is misleading alone.
        let verdict = repro::verdict_from_log_with_trigger(seg, outcome.passed, &trigger);
        if !quiet {
            println!("  run {}/{}: {}", i + 1, segments.len(), verdict.as_str());
        }
        verdicts.push(verdict);
    }
    let last_dir = outcome.run_dir;
    // Neutralize: a later warm run must not replay this case.
    let _ = std::fs::write(&cfg_path, "{}");
    Ok((repro::CheckResult::from_verdicts(&verdicts), last_dir))
}

/// Print the platform support matrix: every registered UI framework, the
/// backend it routes to, and whether it runs today.
fn print_platforms() {
    println!("Platform support matrix (UI framework -> introspection backend)\n");
    println!("  {:<16} {:<26} {:<8}", "PLATFORM", "BACKEND", "STATUS");
    for p in platform::all() {
        println!(
            "  {:<16} {:<26} {:<8}",
            p.id,
            p.backend.as_str(),
            p.status.label()
        );
    }
    println!(
        "\n  live = validated   beta = wired, unvalidated   planned = routed, runner not built\n\
         \n  The point: Qt/GTK/WinUI/Avalonia/wxWidgets share ONE backend per OS\n\
         (they all publish to the OS accessibility API), Electron/Tauri reuse the\n\
         web backend, and only immediate-mode GUIs (imgui, clay) need an in-app hook."
    );
}

/// Resolve the vault path from config (or cwd default when no config is found).
fn resolve_vault_path(config_path: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Ok(l) = config::load(config_path) {
        Ok(l.root.join(
            l.config
                .auth
                .vault
                .clone()
                .unwrap_or_else(|| ".reproit/secrets.vault".into()),
        ))
    } else {
        Ok(std::env::current_dir()?.join(".reproit/secrets.vault"))
    }
}

fn journey_cmd(
    config_path: Option<&std::path::Path>,
    action: JourneyAction,
    ctx: &Ctx,
) -> Result<()> {
    let loaded = config::load(config_path)?;
    match action {
        JourneyAction::List => {
            let journeys = journey::list(&loaded.root)?;
            if ctx.json {
                ctx.emit(&serde_json::json!({ "journeys": journeys }));
            } else if journeys.is_empty() {
                ctx.say("no journeys yet (author one with `reproit journey save`)");
            } else {
                for j in &journeys {
                    match &j.error {
                        Some(e) => ctx.say(format!("  {:<16} (broken: {e})", j.name)),
                        None => {
                            let setup = j
                                .setup
                                .as_ref()
                                .map(|s| format!(", setup {s}"))
                                .unwrap_or_default();
                            ctx.say(format!("  {:<16} {} steps{setup}", j.name, j.steps));
                        }
                    }
                }
            }
        }
        JourneyAction::Save { name, spec } => {
            let spec = match spec {
                Some(s) => s,
                None => {
                    use std::io::Read;
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            let path = journey::save(&loaded.root, &name, &spec)?;
            let rel = path.strip_prefix(&loaded.root).unwrap_or(&path);
            if ctx.json {
                ctx.emit(&serde_json::json!({
                    "saved": name,
                    "path": rel.to_string_lossy(),
                    "next": format!("reproit check {name}"),
                }));
            } else {
                ctx.say(format!("  saved {}", rel.display()));
                ctx.say(format!("  run it: reproit check {name}"));
            }
        }
    }
    Ok(())
}

fn auth_cmd(config_path: Option<&std::path::Path>, action: AuthAction) -> Result<()> {
    let vpath = resolve_vault_path(config_path)?;
    match action {
        AuthAction::Set { key, value } => {
            let val = match value {
                Some(v) => v,
                None => {
                    use std::io::Read;
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s.trim_end_matches(['\n', '\r']).to_string()
                }
            };
            if val.is_empty() {
                anyhow::bail!("empty value; pass --value or pipe the secret on stdin");
            }
            let mut v = auth::Vault::open(&vpath)?;
            v.set(&key, &val);
            v.save()?;
            println!("  stored {key} in {}", vpath.display());
        }
        AuthAction::SetTotp { key, secret } => {
            let Some(code) = auth::totp_now(&secret) else {
                anyhow::bail!("not a valid base32 TOTP secret");
            };
            let mut v = auth::Vault::open(&vpath)?;
            v.set(&key, &secret);
            v.save()?;
            println!("  stored TOTP {key} (current code {code})");
        }
        AuthAction::List => {
            let v = auth::Vault::open(&vpath)?;
            let keys: Vec<&String> = v.keys().collect();
            if keys.is_empty() {
                println!("  vault is empty ({})", vpath.display());
            } else {
                for k in keys {
                    println!("  {k}");
                }
            }
        }
        AuthAction::Remove { key } => {
            let mut v = auth::Vault::open(&vpath)?;
            if v.remove(&key) {
                v.save()?;
                println!("  removed {key}");
            } else {
                println!("  no such key: {key}");
            }
        }
        AuthAction::Test { account } => {
            let loaded = config::load(config_path)?;
            let acct = loaded
                .config
                .auth
                .accounts
                .iter()
                .find(|a| a.name == account)
                .ok_or_else(|| anyhow::anyhow!("no account named {account} in reproit.yaml"))?;
            let env = auth::secret_env(&loaded.config.auth, &loaded.root)?;
            let ns = format!(
                "REPROIT_SECRET_{}",
                account
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() {
                        c.to_ascii_uppercase()
                    } else {
                        '_'
                    })
                    .collect::<String>()
            );
            println!("account {account}:");
            for (k, val) in &env {
                if !k.starts_with(&ns) {
                    continue;
                }
                if k.ends_with("_PASSWORD") {
                    println!("  {k} = (set, {} chars, hidden)", val.len());
                } else {
                    println!("  {k} = {val}");
                }
            }
            if acct.password_ref.is_some() && !env.iter().any(|(k, _)| k.ends_with("_PASSWORD")) {
                println!("  warn: passwordRef set but key not found in vault");
            }
        }
    }
    Ok(())
}

async fn doctor(config_path: Option<&std::path::Path>) -> Result<()> {
    let mut ok = true;
    // Platform-specific tool checks. Default to flutter when no config is
    // present (the common first-run case).
    let loaded = config::load(config_path).ok();
    let web = loaded
        .as_ref()
        .map(|l| l.config.app.platform == "web-playwright")
        .unwrap_or(false);

    let checks: &[(&str, &str)] = if web {
        &[("node", "web runner (Playwright)")]
    } else {
        &[
            ("xcrun", "simulator control (Xcode command line tools)"),
            ("ffmpeg", "video compositing"),
            ("flutter", "driving the app"),
        ]
    };
    for (bin, why) in checks {
        let found = exec::which(bin).await;
        println!(
            "  {}  {bin}  ({why})",
            if found { "ok " } else { "MISSING" }
        );
        ok &= found;
    }
    if web {
        // Playwright + chromium present in the configured web runner dir.
        if let Some(l) = &loaded {
            if let Some(dir) = &l.config.app.web_runner_dir {
                let runner = l.root.join(dir).join("node_modules/playwright");
                let present = runner.exists();
                println!(
                    "  {}  playwright in {} ({})",
                    if present { "ok " } else { "MISSING" },
                    dir,
                    if present {
                        "installed"
                    } else {
                        "run npm install + npx playwright install chromium"
                    }
                );
                ok &= present;
            }
        }
    } else {
        let sims = exec::run("xcrun", &["simctl", "list", "devices", "booted"]).await;
        println!(
            "  {}  simctl reachable",
            if sims.ok() { "ok " } else { "MISSING" }
        );
        ok &= sims.ok();
    }

    // LLM provider check: advisory only. The runner works without one; the
    // authoring agent and failure analyzer need it.
    match config::load(config_path) {
        Ok(loaded) => match llm::from_spec(&loaded.config.llm.to_spec()) {
            Ok(b) => match b.check().await {
                Ok(()) => println!("  ok   llm: {}", b.name()),
                Err(e) => println!(
                    "  warn llm: {} ({e}); runner works, authoring will not",
                    b.name()
                ),
            },
            Err(e) => {
                println!("  warn llm: {e}");
            }
        },
        Err(_) => println!("  --   llm: no reproit.yaml found, skipping"),
    }

    if !ok {
        std::process::exit(1);
    }
    println!("\nall checks passed");
    Ok(())
}

/// Recursively collect files ending in `.cov.json` under `dir` (coverage
/// snapshots written by instrumented runs). Best-effort: unreadable dirs are
/// skipped.
fn collect_cov_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_cov_files(&p, out);
        } else if p.to_string_lossy().ends_with(".cov.json") {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_as_url_classifies_urls_vs_aliases() {
        // Explicit scheme: kept as-is.
        assert_eq!(
            target_as_url("https://app.com").as_deref(),
            Some("https://app.com")
        );
        assert_eq!(
            target_as_url("http://x.io/a").as_deref(),
            Some("http://x.io/a")
        );
        // Bare host (no scheme): a dotted domain defaults to https.
        assert_eq!(
            target_as_url("google.com").as_deref(),
            Some("https://google.com")
        );
        assert_eq!(
            target_as_url("app.vercel.app/dash").as_deref(),
            Some("https://app.vercel.app/dash")
        );
        // Loopback / dev servers default to http.
        assert_eq!(
            target_as_url("localhost:3000").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            target_as_url("127.0.0.1:8117/").as_deref(),
            Some("http://127.0.0.1:8117/")
        );
        // host:port with no dot is still a URL.
        assert_eq!(
            target_as_url("myhost:3000").as_deref(),
            Some("https://myhost:3000")
        );
        // Bare words are aliases, not URLs.
        assert_eq!(target_as_url("login"), None);
        assert_eq!(target_as_url("checkout"), None);
        assert_eq!(target_as_url(""), None);
    }

    #[test]
    fn parse_fuzz_report_extracts_seed_and_repro_actions() {
        // The exact shape modes/fuzz.rs::write_report emits.
        let md = "\
# fuzz finding (seed 42)

## invariants violated

- **no-exception** (1)

## findings

- `no-exception` **EXCEPTION CAUGHT BY WIDGETS LIBRARY**: boom

## repro (2 actions, shrunk from 7)

```
tap:Login
tap:Submit
```

Replay: write {\"replay\": [...]} to .reproit/fuzz_config.json ...
";
        let (seed, actions) = parse_fuzz_report(md).expect("parse");
        assert_eq!(seed, 42);
        assert_eq!(actions, vec!["tap:Login", "tap:Submit"]);
        // The id is what `keep` would store under.
        assert_eq!(
            repro::repro_id(seed, &actions),
            repro::repro_id(42, &["tap:Login", "tap:Submit"])
        );
    }

    #[test]
    fn pending_meta_lets_a_finding_be_checked_before_keep() {
        // A finding not yet kept: its in-memory Meta carries the same content-hash
        // id keep would store under, is quarantined, has no alias/created stamp,
        // and triggers at the end of its own minimized sequence.
        let f = Finding {
            seed: 42,
            actions: vec!["tap:Login".into(), "tap:Submit".into()],
            run_dir: std::path::PathBuf::from("/tmp/nonexistent-run"),
        };
        let m = f.pending_meta();
        assert_eq!(m.id, repro::repro_id(42, &["tap:Login", "tap:Submit"]));
        assert_eq!(m.id, f.id());
        assert_eq!(m.status, repro::Status::Quarantined);
        assert_eq!(m.seed, 42);
        assert!(m.alias.is_none());
        assert!(m.created.is_empty());
        assert!(m.last_checked.is_none());
        assert_eq!(m.trigger_index, Some(2));
    }

    #[test]
    fn parse_fuzz_oracle_reads_graph_block() {
        // The `## oracle` block fuzz.md emits for a graph finding carries the
        // oracle category and the violating state sig that `check` re-evaluates.
        let md = "\
# fuzz finding (seed 9)

## invariants violated

- **no-dead-end** (1)

## oracle

- oracle: `graph`
- invariant: `no-dead-end`
- sig: `advanced`

## findings

- `no-dead-end` **GRAPH**: state advanced is a dead end

## repro (1 actions)

```
tap:Advanced
```
";
        let (oracle, sig) = parse_fuzz_oracle(md);
        assert_eq!(oracle.as_deref(), Some("graph"));
        assert_eq!(sig.as_deref(), Some("advanced"));
    }

    #[test]
    fn parse_fuzz_oracle_crash_block_has_no_sig() {
        let md = "\
# fuzz finding (seed 1)

## oracle

- oracle: `crash`
- invariant: `no-exception`
- sig: ``

## findings
";
        let (oracle, sig) = parse_fuzz_oracle(md);
        assert_eq!(oracle.as_deref(), Some("crash"));
        assert_eq!(sig, None);
    }

    #[test]
    fn parse_fuzz_oracle_absent_block_is_none() {
        // An older report with no `## oracle` block -> fall back to crash path.
        let md = "# fuzz finding (seed 1)\n\n## findings\n";
        assert_eq!(parse_fuzz_oracle(md), (None, None));
    }

    #[test]
    fn parse_fuzz_report_handles_empty_repro_block() {
        let md = "# fuzz finding (seed 5)\n\n## repro (0 actions)\n\n```\n```\n";
        let (seed, actions) = parse_fuzz_report(md).expect("parse");
        assert_eq!(seed, 5);
        assert!(actions.is_empty());
    }

    #[test]
    fn parse_fuzz_report_without_seed_is_none() {
        assert!(parse_fuzz_report("# not a finding\n\nblah\n").is_none());
    }

    #[test]
    fn web_engine_targets_route_to_the_cross_engine_path() {
        // A list of only engine names routes to the cross-engine differential.
        assert!(is_web_engines("chromium,firefox,webkit"));
        assert!(is_web_engines("chrome,safari"));
        // A bare `web` (or any platform token) is NOT the engine path: it is a
        // platform run. ios/android likewise route to the platform path.
        assert!(!is_web_engines("web"));
        assert!(!is_web_engines("ios,android"));
        // Mixed engine+platform is NOT all-engine -> platform path.
        assert!(!is_web_engines("chromium,ios"));
        assert!(!is_web_engines(""));
    }

    #[test]
    fn headless_tier_skips_the_device_picker() {
        // flutter/web default to a headless tier (flutter test / web CDP) that
        // uses no device, so the interactive picker is vestigial and not offered.
        assert!(!run_needs_device_pick("flutter-ios-sim", false));
        assert!(!run_needs_device_pick("web-playwright", false));
        // --sim turns a flutter run into a real sim run, which DOES need a device.
        assert!(run_needs_device_pick("flutter-ios-sim", true));
        // A platform with no headless tier (native/appium) always needs a device.
        assert!(run_needs_device_pick("native-ios", false));
        assert!(run_needs_device_pick("appium-android", false));
    }

    #[test]
    fn scoped_env_restores_prior_value_and_removes_unset_keys() {
        // ScopedEnv is what guarantees a per-target REPROIT_* never leaks into
        // the next target (Task 1) AND the same Drop pattern underpins the
        // crash-reporter restore (Task 2). Use unique keys to avoid clobbering
        // anything real in the test process.
        let set_key = "REPROIT_TEST_SCOPED_SET";
        let unset_key = "REPROIT_TEST_SCOPED_UNSET";
        std::env::set_var(set_key, "original");
        std::env::remove_var(unset_key);
        {
            let _guard = ScopedEnv::set(vec![
                (set_key.to_string(), "during".to_string()),
                (unset_key.to_string(), "during".to_string()),
            ]);
            assert_eq!(std::env::var(set_key).as_deref(), Ok("during"));
            assert_eq!(std::env::var(unset_key).as_deref(), Ok("during"));
        }
        // After drop: the previously-set key is restored to its old value, and
        // the previously-unset key is removed entirely.
        assert_eq!(std::env::var(set_key).as_deref(), Ok("original"));
        assert!(std::env::var(unset_key).is_err());
        std::env::remove_var(set_key);
    }
}
