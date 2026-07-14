//! reproit: reproducible AI QA. Deterministic multi-device test orchestration
//! with evidence capture. See docs/cli.md.

// These two doc-format lints (new in clippy 1.93) fire on intentionally aligned
// hanging-indent doc tables (e.g. model/repro.rs) whose alignment aids reading.
// Keep the alignment rather than reflow it to satisfy a purely stylistic lint.
#![allow(clippy::doc_overindented_list_items, clippy::doc_lazy_continuation)]

// Top-level: CLI entry, config, and cross-cutting infra.
mod auth;
#[path = "model/backend.rs"]
mod backend;
mod capsule;
mod config;
#[path = "model/contracts.rs"]
mod contracts;
mod crashreporter;
mod crosscut;
mod exec;
mod init;
mod junit;
mod layout;
mod mcp;
#[path = "model/observation.rs"]
mod observation;
mod skills;
mod update;
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
// Native desktop accessibility runners, dispatched as hidden `__uia` / `__atspi`
// subcommands (the Windows/Linux twins of the macOS `swift macos-ax.swift`
// backend). Each is OS-gated: the module compiles only on its host OS, so
// `cargo build --workspace` on any other target pulls neither the module nor its
// platform dependency (see the target-gated stanzas in Cargo.toml). On the wrong
// OS the subcommand still exists but returns a clear "unsupported" error, so the
// binary builds and dispatches on every target.
#[cfg(target_os = "linux")]
#[path = "backends/atspi.rs"]
mod atspi;
#[cfg(windows)]
#[path = "backends/uia.rs"]
mod uia;
#[path = "backends/vmservice.rs"]
mod vmservice;
// modes/, the user-facing commands.
#[path = "modes/a2ui.rs"]
mod a2ui;
#[path = "modes/analyze.rs"]
mod analyze;
#[path = "modes/backend_headless.rs"]
mod backend_headless;
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
#[path = "modes/pwfuzz.rs"]
mod pwfuzz;
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

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
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

    pub(crate) fn confirmed(&self) -> bool {
        self.yes
    }
}

/// Version string stamped by build.rs: a clean `0.1.<commit-count>` for an
/// install / clean build, plus a `(<rev>-dirty <date>)` suffix ONLY for local
/// working builds with uncommitted edits. So `cargo install` shows a plain
/// `0.1.64` while a dev build is obviously identifiable.
pub(crate) const VERSION: &str = env!("REPROIT_VERSION");

#[derive(Parser)]
#[command(
    name = "reproit",
    version = VERSION,
    about = "Find UI failures and keep every confirmed bug reproducible"
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

/// Turn a bug id into the command that already owns its execution semantics.
///
/// `reproit` is itself the verb ("reproduce it"), so the public fast path is
/// deliberately `reproit <id>`, not `reproit check <id>` or the redundant
/// `reproit reproduce <id>`. Production buckets pull + replay; local findings
/// and saved repros use the deterministic check path. The explicit verbs stay
/// available for scripts and for `check`'s whole-suite form.
fn expand_direct_bug_arg(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let mut index = 1;
    while let Some(arg) = args.get(index).and_then(|arg| arg.to_str()) {
        match arg {
            "--json" | "--quiet" | "--yes" => index += 1,
            "--config" => index += 2,
            _ if arg.starts_with("--config=") => index += 1,
            _ => break,
        }
    }
    let Some(first) = args.get(index).and_then(|arg| arg.to_str()) else {
        return args;
    };
    let command = if first.starts_with("bkt_") {
        Some(("__replay-bucket", None))
    } else if first.starts_with("fnd_") || first.starts_with("rep_") {
        Some(("check", Some("--repro-id")))
    } else {
        None
    };
    if let Some((command, internal_arg)) = command {
        args.insert(index, command.into());
        if let Some(internal_arg) = internal_arg {
            args.insert(index + 1, internal_arg.into());
        }
    }
    args
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
    // A URL never contains whitespace, so a target with a space is a command line
    // (e.g. `less sample.txt`), not a bare host that happens to end in a TLD-like
    // token -- this is what lets `scan "lazygit --flag"` reach executable detection
    // instead of being misread as `https://lazygit --flag`.
    if t.chars().any(char::is_whitespace) {
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
    // A dotted host is a real host only if its LAST label is TLD-like (alphabetic,
    // 2+ chars), so `google.com` is a URL but an alias like `checkout.2` (numeric
    // last label) is not -- OR every label is numeric (an IPv4 address). (A bare
    // `host:port` is still treated as a URL; `screen:1` vs `myhost:3000` can't be
    // told apart by shape, so that case is left as-is.)
    let labels: Vec<&str> = host.split('.').collect();
    let dotted_host = labels.len() >= 2 && {
        let last = labels.last().copied().unwrap_or("");
        let tld_like = last.len() >= 2 && last.chars().all(|c| c.is_ascii_alphabetic());
        let ipv4 = labels.len() == 4
            && labels
                .iter()
                .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()));
        tld_like || ipv4
    };
    if is_loopback || dotted_host || has_port {
        let scheme = if is_loopback { "http" } else { "https" };
        return Some(format!("{scheme}://{t}"));
    }
    None
}

/// Does `p` name an existing executable file (unix: with an exec bit)?
fn is_executable_file(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Extensions to try when resolving a bare command name on `PATH`: just the name
/// on unix; the Windows `PATHEXT` set (so `lazygit` finds `lazygit.exe`) plus the
/// bare name (in case it already carries an extension) on Windows.
fn path_executable_extensions() -> Vec<String> {
    #[cfg(windows)]
    {
        let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        let mut exts: Vec<String> = raw
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        exts.push(String::new());
        exts
    }
    #[cfg(not(windows))]
    {
        vec![String::new()]
    }
}

/// Is `prog` (a bare command name) resolvable to an executable on `PATH`? Honors
/// Windows `PATHEXT`, so `htop` matches `htop.exe` there.
fn command_on_path(prog: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let exts = path_executable_extensions();
    std::env::split_paths(&paths).any(|dir| {
        exts.iter()
            .any(|ext| is_executable_file(&dir.join(format!("{prog}{ext}"))))
    })
}

/// A non-URL target that names a runnable TERMINAL executable: an existing
/// executable file path, or a bare command resolvable on `PATH` (e.g. `lazygit`,
/// `htop`). Returns the command line to run in a PTY (`reproit scan <exe>`),
/// args preserved. `None` for anything that isn't clearly an executable, so a
/// saved alias / journey / map node remains a scoped target.
fn target_as_executable(t: &str) -> Option<String> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    // The first whitespace token is the program; the rest are its args.
    let prog = t.split_whitespace().next()?;
    // A path (a separator -- `/` everywhere, also `\` on Windows): must point at an
    // existing executable file. A bare name: resolve it on PATH.
    let is_path = prog.contains('/') || (cfg!(windows) && prog.contains('\\'));
    let ok = if is_path {
        is_executable_file(std::path::Path::new(prog))
    } else {
        command_on_path(prog)
    };
    ok.then(|| t.to_string())
}

async fn ensure_app_map(ctx: &Ctx, loaded: &config::Loaded, journey: &str) -> Result<()> {
    let replace = match map::map_freshness(&loaded.root)? {
        map::MapFreshness::Current => return Ok(()),
        map::MapFreshness::Missing => {
            ctx.say("  learning app structure (first run)...");
            false
        }
        map::MapFreshness::Stale(reasons) => {
            ctx.say(format!(
                "  app model changed ({}); refreshing automatically...",
                reasons.join(", ")
            ));
            true
        }
    };
    rebuild_app_map(loaded, journey, None, false, None, replace).await
}

async fn rebuild_app_map(
    loaded: &config::Loaded,
    journey: &str,
    budget: Option<u32>,
    label: bool,
    from: Option<&Path>,
    replace: bool,
) -> Result<()> {
    let snapshot = if replace {
        Some(map::begin_full_rebuild(&loaded.root)?)
    } else {
        None
    };
    let mut result =
        map::build_map(&loaded.config, &loaded.root, journey, budget, label, from).await;
    if result.is_ok() && replace && !map::appmap_path(&loaded.root).is_file() {
        result = Err(anyhow::anyhow!(
            "could not refresh the internal app model; the app was not reachable"
        ));
    }
    if result.is_err() {
        if let Some(snapshot) = snapshot {
            map::restore_map(snapshot)?;
        }
    }
    result
}

/// SAFETY gate for a zero-config TUI fuzz: it drives a REAL process with REAL
/// side effects (synthetic keystrokes can send messages, run shell commands,
/// write/delete files), so confirm before launching. Always warns; proceeds on
/// `--yes`, else prompts on a TTY, else refuses (CI must pass `--yes`).
fn confirm_tui_fuzz(ctx: &Ctx, exe: &str) -> bool {
    eprintln!(
        "  WARNING: reproit will drive `{exe}` in a PTY by sending SYNTHETIC KEYSTROKES.\n  \
         A real terminal app can have real side effects (send messages, run shell\n  \
         commands, write or delete files). Point it at a THROWAWAY / sandboxed instance."
    );
    if ctx.yes {
        return true;
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("  Refusing without confirmation. Re-run with --yes to proceed.");
        return false;
    }
    use std::io::Write;
    eprint!("  Proceed? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn auth_prompt(label: &str, _secret: bool) -> Result<String> {
    use std::io::Write;
    print!("  {label}: ");
    std::io::stdout().flush()?;
    #[cfg(unix)]
    if _secret {
        let _ = std::process::Command::new("stty").arg("-echo").status();
    }
    let mut value = String::new();
    std::io::stdin().read_line(&mut value)?;
    #[cfg(unix)]
    if _secret {
        let _ = std::process::Command::new("stty").arg("echo").status();
        println!();
    }
    let value = value.trim().to_string();
    if value.is_empty() {
        anyhow::bail!("{label} cannot be empty");
    }
    Ok(value)
}

// A clap subcommand enum: variants carry their flags by value and are
// instantiated once at startup, so the size spread between variants is
// irrelevant (and unavoidable for a rich CLI).
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Cmd {
    /// Detect the current app and create the smallest working reproit setup.
    /// After initialization, use `reproit scan` or `reproit fuzz`.
    Init {
        /// Running web app to initialize. A URL always selects the web UI workflow.
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
    /// Record one replayable repro id ONCE with full evidence + an annotated
    /// video. Use after `fuzz` prints an fnd_... id or after `keep`; for quick
    /// visible-issue audit clips, use `scan --record`.
    Record {
        /// The repro to record: pending fnd_..., saved rep_..., or alias.
        repro: String,
        /// Optional sub-variant, passed as --dart-define=PROMPT_KIND=<kind>
        #[arg(long)]
        kind: Option<String>,
        /// Number of concurrent devices (multi-actor)
        #[arg(long, default_value_t = 1)]
        devices: usize,
        /// Reuse the previous build (--no-build). Only valid when the last build
        /// was this same journey.
        #[arg(long)]
        warm: bool,
        /// Capture SHOOT screenshots into this directory
        #[arg(long)]
        shots_dir: Option<PathBuf>,
        /// Drive in profile mode (AOT) for representative perf
        #[arg(long)]
        profile: bool,
        /// After recording, scan the video for transient render glitches (intra-run
        /// flicker: a frame that diverges then snaps back). No baseline needed.
        #[arg(long)]
        flicker: bool,
    },
    /// Visual-regression the current capture against the committed baseline:
    /// per-pixel tolerance, ignore regions, and `--update` to accept the current
    /// capture. What is compared is driven by the `visual` section in
    /// reproit.yaml.
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
    /// List confirmed production bugs, impact-ranked. Uses the project selected
    /// by `reproit cloud setup`, so the normal form is simply `reproit bugs`.
    Bugs {
        /// Filter by message, signature, or bucket id.
        query: Option<String>,
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Internal route for the direct `reproit bkt_...` form.
    #[command(name = "__replay-bucket", hide = true)]
    ReplayBucket {
        /// Production bucket/finding id (bkt_...).
        issue: String,
        /// Cloud app id (default: $REPROIT_CLOUD_APP).
        #[arg(long)]
        app: Option<String>,
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
    /// Update a production bug's lifecycle state. Example:
    /// `reproit triage bkt_... fixed --fixed-in-build 1.2.3`.
    Triage {
        issue: String,
        status: String,
        #[arg(long = "fixed-in-build")]
        fixed_in_build: Option<String>,
        #[arg(long)]
        assignee: Option<i64>,
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Open a repro's recorded video in your default player. Recordings live
    /// under .reproit/recordings/repro/ (gitignored); make one with `record <id>`.
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
    /// Scan the app for stable bugs simply VISIBLE on each screen (broken
    /// content, blank screens, broken assets, broken routes): crawl every reachable screen once and
    /// report one finding per screen+issue. The fast default "what's wrong here".
    /// `--record` saves quick audit clips; use `record <id>` for a fuzz repro.
    Scan {
        /// What to scan. An A2UI JSON/JSONL stream runs against the official
        /// React and Lit renderers. A URL (https://app.com) runs zero-config against that
        /// deployed app; a terminal EXECUTABLE (e.g. `lazygit`, `htop`, or a path)
        /// runs zero-config in a PTY; any other value scopes the crawl to that
        /// alias/node in a reproit.yaml.
        #[arg(value_name = "TARGET")]
        target_arg: Option<String>,
        /// Coverage budget: how many actions the crawl may take to reach screens.
        #[arg(long, default_value_t = 60)]
        budget: u32,
        /// Force the simulator tier (default: headless / web).
        #[arg(long)]
        sim: bool,
        /// After the crawl, record an annotated clip (a red box on the bug) for
        /// each boxable stable finding. Web only.
        #[arg(long)]
        record: bool,
        /// Where the `--record` clips land (default:
        /// .reproit/recordings/scan/<scan-run>/).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Extra HTTP header injected into the browser context, `"Name: value"`.
        /// Repeatable. Use it to pass a WAF clearance cookie
        /// (`--header "Cookie: cf_clearance=..."`), an auth bearer, or a preview
        /// token so a challenge-fronted or authed target is reachable.
        #[arg(long = "header", value_name = "NAME: VALUE")]
        header: Vec<String>,
    },
    /// Find confirmed, replayable bugs through deeper interaction exploration.
    /// Reproit learns and refreshes its internal app model automatically.
    /// Stable, objective detectors are on by default. Specialist detectors are
    /// opt-in with `--only`; `--soak` runs the leak cycle.
    Fuzz {
        /// What to fuzz (optional). An A2UI JSON/JSONL stream is checked across
        /// the official React and Lit renderers with schema-valid mutations. A PLAYWRIGHT TEST file
        /// (`reproit fuzz your-test.spec.ts`) is run under trace; reproit replays
        /// its actions to reach its deep state, then fuzzes onward for the bugs the
        /// test never covered (you wrote the test; reproit finds the bugs you
        /// didn't). A URL (https://app.com) is auto-detected and runs zero-config
        /// against that deployed app; a terminal EXECUTABLE (e.g. `lazygit`, or a
        /// path) runs zero-config in a PTY; no reproit.yaml needed for any of these.
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
        /// Deprecated compatibility flag: confirmation now minimizes by default.
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
    /// Diagnose local setup: config, runner deps, app URL, and cloud credentials.
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
        after_help = "Run:     reproit journey <name>\nCreate:  reproit journey create <name>\nList:    reproit journey list"
    )]
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
    /// Sign in to Reproit Cloud and persist the validated project key locally.
    Login {
        /// Cloud base URL (default: https://cloud.reproit.com).
        #[arg(long)]
        cloud: Option<String>,
        /// Project key, sk_live_... (default: $REPROIT_CLOUD_KEY).
        #[arg(long)]
        key: Option<String>,
        /// Optional app id used to validate project access.
        #[arg(long)]
        app: Option<String>,
    },
    /// (internal) PTY-driven terminal-UI runner; spawned by the tui backend
    #[command(name = "__tui", hide = true)]
    TuiRun,
    /// (internal) Windows UI Automation runner; spawned by the desktop-uia backend
    #[command(name = "__uia", hide = true)]
    UiaRun,
    /// (internal) Linux AT-SPI runner; spawned by the desktop-atspi backend
    #[command(name = "__atspi", hide = true)]
    AtspiRun,
    /// Refresh the release cache without delaying the calling command.
    #[command(name = "__update-check", hide = true)]
    UpdateCheck,
}

#[derive(Subcommand)]
enum DebugAction {
    /// Inspect or force maintenance of reproit's internal app model.
    Map {
        #[command(subcommand)]
        action: Option<MapAction>,
    },
}

/// `repro` subcommands: advanced operations that act on an existing repro.
#[derive(Subcommand)]
enum ReproAction {
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
    /// LLM-read the source into the candidate (semantic) map, reconcile, report.
    Semantic,
    /// Coverage diff: screens the code declares vs screens verified by the crawl.
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
    /// Authenticate with a cloud/project key (sk_live_..., distinct from the
    /// app auth vault) and VALIDATE it against the cloud. Reads $REPROIT_CLOUD_KEY /
    /// $REPROIT_CLOUD_URL when not passed.
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
    /// Validates and persists the project key, binds this GitHub repo for hosted
    /// reproduction (repository_dispatch) via PUT /v1/apps/:app/integrations,
    /// writes .github/workflows/reproit-repro.yml, and prints the remaining
    /// manual steps (the repo secret + the SDK start call). Create the project in
    /// the dashboard first, then pass its appId with --app.
    Setup {
        /// The existing project's app id (from the dashboard).
        #[arg(long)]
        app: String,
        /// Project key, sk_live_... (default: $REPROIT_CLOUD_KEY / persisted login)
        #[arg(long)]
        key: Option<String>,
        /// Cloud base URL (default: $REPROIT_CLOUD_URL)
        #[arg(long)]
        cloud: Option<String>,
        /// GitHub fine-grained PAT (Contents read/write on the app repo) the
        /// cloud uses to dispatch reproduction (default: $REPROIT_DISPATCH_TOKEN).
        #[arg(long)]
        dispatch_token: Option<String>,
        /// Override the auto-detected GitHub repo (owner/name).
        #[arg(long)]
        repo: Option<String>,
        /// Where to write the workflow (default: .github/workflows/reproit-repro.yml).
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
    /// the `bucketId` that direct reproduction, `triage`, and `timeline` use via
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
        /// Content-addressed bucket id. Omit with --sig to resolve by signature.
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
        /// Content-addressed bucket id to reproduce. With `--as`, does pull -> check.
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
    /// cloud boundary in the check loop: fetches the bucket's replay package and
    /// writes it as a saved repro under `.reproit/repros/` named `--as <name>`,
    /// the SAME on-disk shape `keep` produces. Afterwards `reproit check <name>`
    /// runs the standard local, network-free verification and `reproit repros`
    /// lists it -- indistinguishable from a locally found repro.
    /// Fetches the content-addressed `GET /v1/apps/:app/buckets/:bucket`.
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
    /// Powers the MCP diagnose entry point.
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
enum JourneyAction {
    /// Run a journey by name (`reproit journey <name>`).
    #[command(external_subcommand)]
    Run(Vec<String>),
    /// List saved journeys with a one-line summary of each.
    List,
    /// Create or overwrite a journey from a JSON spec, e.g.
    /// {"setup":"login(guest)","steps":[{"do":"tap:key:testid:add"}]}.
    /// Validates the structure (and against the map if one exists) before
    /// writing journeys/<name>.yaml. Reads the spec from stdin if --spec omitted.
    Create {
        /// Journey name (the file stem under journeys/).
        name: String,
        /// The journey as a JSON object: {"setup"?, "steps":[...]}.
        #[arg(long)]
        spec: Option<String>,
    },
}

#[derive(Subcommand)]
enum AuthAction {
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
    /// Discover, generate, and clean-run a multi-screen login flow for an account.
    Discover { account: String },
    /// Validate a configured account: refs, vault keys, TOTP, and login journey.
    Doctor { account: String },
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AuthStrategyArg {
    Password,
    PasswordOtp,
    PhoneOtp,
    EmailLink,
    OauthTest,
    Session,
    Api,
}

impl AuthStrategyArg {
    fn config(self) -> config::AuthStrategy {
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

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse_from(expand_direct_bug_arg(std::env::args_os().collect()));
    let ctx = cli.ctx();
    if !matches!(&cli.command, Cmd::Update { .. } | Cmd::UpdateCheck) {
        update::notice_and_schedule(VERSION, cli.quiet, cli.json);
    }
    match cli.command {
        Cmd::Init {
            target,
            platform,
            force,
        } => {
            let root = std::env::current_dir()?;
            if let Some(target) = target {
                if platform.as_deref().is_some_and(|value| value != "web") {
                    anyhow::bail!(
                        "a URL initializes the web UI workflow; remove --platform or use --platform web"
                    );
                }
                let url = target_as_url(&target).ok_or_else(|| {
                    anyhow::anyhow!("init target must be a web URL, got {target:?}")
                })?;
                let runner = config::ensure_web_runner_dir(VERSION, &|message| ctx.say(message))?;
                init::init_web_url(&root, &url, &runner, force)?;
            } else {
                init::init(&root, platform.as_deref(), force)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Update { check } => {
            update::run(VERSION, check).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::UpdateCheck => {
            let _ = update::refresh_cache(VERSION).await;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Doctor => {
            doctor(cli.config.as_deref(), &ctx).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Login { cloud, key, app } => {
            match cloud_cmd(
                cli.config.as_deref(),
                CloudAction::Login { cloud, key, app },
                ctx.json,
            )
            .await
            {
                Ok(()) => Ok(ExitCode::SUCCESS),
                Err(e) => {
                    if ctx.json {
                        ctx.emit(&serde_json::json!({
                            "command": "login",
                            "ok": false,
                            "error": e.to_string(),
                        }));
                    } else {
                        eprintln!("login: {e}");
                    }
                    Ok(exit_with(Exit::Regression))
                }
            }
        }
        // Advanced graph diagnostics. Normal workflows call ensure_app_map and
        // never require users or agents to manage this lifecycle explicitly.
        Cmd::Debug {
            action: DebugAction::Map { action },
        } => {
            // Bare `debug map` forces a full structural rebuild.
            let action = action.unwrap_or(MapAction::Structural {
                journey: "explore".to_string(),
                budget: None,
                label: false,
                from: None,
                platform: None,
                force: false,
            });
            match action {
                MapAction::Structural {
                    journey,
                    budget,
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
                    rebuild_app_map(&loaded, &journey, budget, label, from.as_deref(), true)
                        .await?;
                    if ctx.json {
                        let m = map::load_map(&loaded.root, &loaded.config);
                        ctx.emit(&serde_json::json!({
                            "command": "debug map structural",
                            "states": m.states.len(),
                            "transitions": m.transitions.len(),
                            "budget": budget,
                            "map_path": map::appmap_path(&loaded.root).to_string_lossy(),
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
                            ensure_app_map(&ctx, &loaded, "explore").await?;
                            map::appmap_path(&loaded.root)
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
                            ensure_app_map(&ctx, &loaded, "explore").await?;
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
                    ensure_app_map(&ctx, &loaded, "explore").await?;
                    let cm = mapplan::plan(&loaded, ctx.quiet).await?;
                    if ctx.json {
                        let mut v = mapplan::coverage_json(&cm);
                        v["command"] = "debug map semantic".into();
                        ctx.emit(&v);
                    }
                    Ok(ExitCode::SUCCESS)
                }
                MapAction::Coverage => {
                    let loaded = config::load(cli.config.as_deref())?;
                    ensure_app_map(&ctx, &loaded, "explore").await?;
                    mapplan::cover(&loaded, ctx.json)?;
                    Ok(ExitCode::SUCCESS)
                }
                MapAction::Converge => {
                    let loaded = config::load(cli.config.as_deref())?;
                    ensure_app_map(&ctx, &loaded, "explore").await?;
                    mapplan::converge_cmd(&loaded, ctx.json)?;
                    Ok(ExitCode::SUCCESS)
                }
            }
        }
        // `record`: run a repro ONCE with full evidence + annotated video,
        // REPLAYING the kept repro. The runner draws the annotated overlay (paced
        // action HUD + the red finding box) ONLY when it is fed a replay; without
        // one it explores freely and the "annotated video" is a random walk, not
        // the bug. So we write the repro's stored action sequence to the fuzz
        // config the runner reads and hand the path to the orchestrator as a
        // define, instead of running a bare `explore` journey. `--flicker` then
        // scans the recorded video for transient render glitches.
        Cmd::Record {
            repro,
            kind,
            devices,
            warm,
            shots_dir,
            profile,
            flicker,
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            let journey = resolve_repro_journey(&loaded.root, &repro)?;
            let meta = repro::resolve(&loaded.root, &repro)
                .ok_or_else(|| anyhow::anyhow!("no repro `{repro}` (by id or alias)"))?;
            let replay_path = repro::repro_dir(&loaded.root, &meta.id).join("replay.json");
            let mut replay: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&replay_path).map_err(|e| {
                    anyhow::anyhow!(
                        "reading replay for `{repro}` ({}): {e}",
                        replay_path.display()
                    )
                })?)?;
            // Tell the runner which finding this clip is for, so the annotated box
            // is scoped to JUST that oracle's issue (one box), not every problem
            // on the page. The runner reads `highlight` from the config.
            let saved_meta = repro::load_meta(&loaded.root, &meta.id);
            if let Some(saved) = saved_meta.as_ref() {
                minimize_record_replay(&mut replay, saved);
            }
            if let Some(oracle) = saved_meta.and_then(|m| m.oracle) {
                if let Some(obj) = replay.as_object_mut() {
                    obj.insert("highlight".to_string(), serde_json::Value::String(oracle));
                }
            }
            let cfg_path = layout::fuzz_config_path(&loaded.root);
            std::fs::create_dir_all(cfg_path.parent().unwrap())?;
            std::fs::write(&cfg_path, replay.to_string())?;
            let extra = vec![(
                "REPROIT_FUZZ_CONFIG".to_string(),
                cfg_path.to_string_lossy().into_owned(),
            )];
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
                    extra_defines: &extra,
                    // `record` produces an annotated video, so the runner must
                    // record even when evidence.video is off.
                    record_video: true,
                    ..Default::default()
                },
            )
            .await?;
            // `--flicker`: scan the just-recorded video frame-to-frame for
            // transient render glitches (a frame that diverges then snaps back).
            if flicker {
                let events =
                    flicker::analyze_run(&outcome.run_dir, &flicker::FlickerCfg::default()).await?;
                let clean = flicker::report(&events);
                return Ok(if clean {
                    ExitCode::SUCCESS
                } else {
                    exit_with(Exit::Regression)
                });
            }
            return Ok(if outcome.passed {
                ExitCode::SUCCESS
            } else {
                exit_with(Exit::Regression)
            });
        }
        // `baseline`: the visual oracle. Diff the current capture against the
        // committed baseline (per-pixel tolerance + ignore regions); `--update`
        // accepts the current capture as the new baseline.
        Cmd::Baseline { update } => {
            let loaded = config::load(cli.config.as_deref())?;
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
        // `check`: run saved repros and classify each pass/fail/flaky/stale (the
        // four-outcome CI contract). With no name, runs the whole suite and
        // aggregates the worst outcome. Recording and baseline diff are their own
        // verbs now (`record`/`baseline`).
        Cmd::Check {
            repro,
            devices,
            kind,
            runs,
            junit,
            strict,
            locale,
            target,
            device,
        } => {
            if let Some(id) = repro.as_deref() {
                if let Some(code) = backend_headless::try_replay(&ctx, id).await? {
                    return Ok(code);
                }
                if let Some(code) = a2ui::try_replay(&ctx, id)? {
                    return Ok(code);
                }
            }
            let loaded = config::load(cli.config.as_deref())?;
            ensure_app_map(&ctx, &loaded, "explore").await?;
            let locales = locale
                .as_deref()
                .map(crosscut::parse_locales)
                .unwrap_or_default();
            // MULTI-TARGET --target dispatch for `check`: when `--target` names
            // more than one run target (web engines chromium,firefox,webkit, or
            // platforms ios,android), run the saved suite on EACH target and diff
            // which repros are red on a SUBSET of targets (a divergence). The
            // single-target / no-target path below stays the rich locale+junit+
            // promotion flow unchanged.
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
                                "no repro or finding `{r}` (by id or alias). List saved bugs with `reproit bugs`, or find some with `reproit fuzz`."
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
                        Some(l) => format!("{} @{l}", check_label(meta)),
                        None => check_label(meta),
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
                    // Quarantined repros are "reported but non-blocking" in a
                    // WHOLE-SUITE check (no id): a fresh keep can't break CI before
                    // it has proven green once. But an EXPLICIT `check <id>` is the
                    // user verifying THAT bug -- if it still reproduces it must be
                    // RED (exit non-zero), so the find -> check(RED) -> fix ->
                    // check(GREEN) -> guard loop is honest. `--strict` blocks
                    // everywhere; required repros always gate.
                    let blocks =
                        strict || repro.is_some() || meta.status != repro::Status::Quarantined;
                    let effective = if blocks {
                        result.outcome
                    } else {
                        repro::Outcome::Pass
                    };
                    worst = worst.max(effective);
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
                        "id": public_json_id(meta),
                        "kind": public_json_kind(meta),
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
                                check_label(meta),
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
        Cmd::Repro {
            action: ReproAction::Simplify { repro, to },
        } => {
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
                    "repro": public_json_id(&meta),
                    "kind": public_json_kind(&meta),
                    "reproduces": reproduces,
                    "verdict": result.outcome.as_str(),
                    "from_actions": current.len(),
                    "to_actions": candidate.len(),
                    "adopted": adopt,
                    "new_id": adopt.then(|| repro::display_repro_id(&new_id)),
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
                    public_json_id(&meta),
                    current.len(),
                    repro::display_repro_id(&new_id),
                    candidate.len()
                ));
            } else if !reproduces {
                ctx.say(format!(
                    "  candidate did NOT reproduce (verdict: {}); kept {}",
                    result.outcome.as_str(),
                    public_json_id(&meta)
                ));
            } else {
                ctx.say(format!(
                    "  candidate reproduces but is not shorter ({} vs {}); kept {}",
                    candidate.len(),
                    current.len(),
                    public_json_id(&meta)
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
                            "id": repro::display_repro_id(&m.id),
                            "kind": "repro",
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
                        repro::display_repro_id(&m.id),
                        m.alias.as_deref().unwrap_or("-"),
                        m.status.as_str(),
                        m.last_result.as_deref().unwrap_or("never"),
                    ));
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Bugs {
            query,
            app,
            cloud,
            key,
        } => {
            let app = cloud_app_id(app)?;
            let (cloud, key) = cloud_creds(cloud, key);
            triage::buckets(&app, query.as_deref(), ctx.json, cloud, key).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::ReplayBucket {
            issue,
            app,
            as_name,
            no_run,
            cloud,
            key,
        } => {
            let app = cloud_app_id(app)?;
            let alias = as_name.unwrap_or_else(|| issue.clone());
            let (cloud, key) = cloud_creds(cloud, key);
            let loaded = config::load(cli.config.as_deref())?;
            triage::reproduce_bucket(
                &loaded.root,
                &app,
                &issue,
                &alias,
                !no_run,
                None,
                ctx.json,
                cloud,
                key,
            )
            .await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Triage {
            issue,
            status,
            fixed_in_build,
            assignee,
            app,
            cloud,
            key,
        } => {
            let app = cloud_app_id(app)?;
            let (cloud, key) = cloud_creds(cloud, key);
            triage::triage(
                &app,
                &issue,
                Some(&status),
                fixed_in_build.as_deref(),
                assignee,
                ctx.json,
                cloud,
                key,
            )
            .await?;
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
        Cmd::Fix { run } => {
            let loaded = config::load(cli.config.as_deref())?;
            fix::fix(&loaded.config, &loaded.root, run.as_deref()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Analyze { run } => {
            let loaded = config::load(cli.config.as_deref())?;
            analyze::analyze(&loaded.config, &loaded.root, run.as_deref()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Scan {
            target_arg,
            budget,
            sim,
            record,
            out,
            header,
        } => {
            let configured_backend = if target_arg.is_none() {
                backend_config_target(cli.config.as_deref())?
            } else {
                None
            };
            if let Some(path) = target_arg.as_deref().map(PathBuf::from) {
                if path.is_file() && a2ui::looks_like_target(&path) {
                    if record {
                        anyhow::bail!("A2UI streams produce a minimal JSON reproduction, so `scan --record` does not apply");
                    }
                    return a2ui::run_target(&ctx, &path, "scan", 1, 1);
                }
            }
            // `--header "Name: value"` (repeatable) -> a JSON object the web runner
            // reads into the browser context's extraHTTPHeaders (clearance / auth /
            // preview tokens). Set before the runner is spawned so it is inherited.
            if !header.is_empty() {
                let mut map = serde_json::Map::new();
                for h in &header {
                    if let Some((name, value)) = h.split_once(':') {
                        let name = name.trim();
                        if !name.is_empty() {
                            map.insert(name.to_string(), serde_json::Value::from(value.trim()));
                        }
                    } else {
                        return Err(anyhow::anyhow!(
                            "invalid --header {h:?}: expected \"Name: value\""
                        ));
                    }
                }
                std::env::set_var(
                    "REPROIT_EXTRA_HEADERS",
                    serde_json::Value::Object(map).to_string(),
                );
            }
            if let Some(path) = target_arg.as_deref().map(PathBuf::from) {
                if path.is_file() && backend_headless::looks_like_schema(&path) {
                    if record {
                        anyhow::bail!(
                            "backend streams produce a structural reproduction, so `scan --record` does not apply"
                        );
                    }
                    return backend_headless::run_target(&ctx, &path, "scan", 1, 1).await;
                }
            } else if let Some((path, config)) = configured_backend {
                if record {
                    anyhow::bail!(
                        "backend streams produce a structural reproduction, so `scan --record` does not apply"
                    );
                }
                return backend_headless::run_configured_target(&ctx, &path, "scan", 1, 1, config)
                    .await;
            }
            // Zero-config targets: a URL synthesizes a web config; a bare terminal
            // EXECUTABLE (when there's no project config) synthesizes a TUI/PTY
            // config. Both auto-build the map. Anything else scopes to an
            // alias/journey/node in a reproit.yaml (which wins over an executable
            // of the same name, so a project is never hijacked).
            let target_url = target_arg.as_deref().and_then(target_as_url);
            let mut synthesized = target_url.is_some();
            let loaded = if let Some(u) = &target_url {
                let wrd = config::ensure_web_runner_dir(VERSION, &|m| ctx.say(m))?;
                ctx.say(format!("zero-config web run against {u}"));
                let l = config::synthesize_web(u, &wrd, std::env::current_dir()?)?;
                ensure_app_map(&ctx, &l, "explore").await?;
                l
            } else {
                match config::load(cli.config.as_deref()) {
                    Ok(l) => {
                        ensure_app_map(&ctx, &l, "explore").await?;
                        l
                    }
                    Err(e) => match target_arg.as_deref().and_then(target_as_executable) {
                        Some(exe) => {
                            if !confirm_tui_fuzz(&ctx, &exe) {
                                return Ok(ExitCode::SUCCESS);
                            }
                            ctx.say(format!("zero-config TUI run against `{exe}`"));
                            let l = config::synthesize_tui(&exe, std::env::current_dir()?)?;
                            ensure_app_map(&ctx, &l, "explore").await?;
                            synthesized = true;
                            l
                        }
                        None => return Err(e),
                    },
                }
            };
            let journey = match &target_arg {
                Some(t) if !synthesized => t.clone(),
                _ => "explore".to_string(),
            };
            let args = fuzz::ScanArgs {
                journey,
                seed: 1,
                budget,
                sim,
                json: ctx.json,
                record,
                out,
            };
            let summary = fuzz::scan(&loaded.config, &loaded.root, &args).await?;
            // A cut-short crawl (timeout/killed) checked only some screens; exit
            // non-zero so CI/agents never read an incomplete scan as a clean pass.
            // Confirmed scan findings are also regressions, matching A2UI scan.
            return Ok(if summary.complete && summary.issues == 0 {
                ExitCode::SUCCESS
            } else {
                exit_with(Exit::Regression)
            });
        }
        Cmd::Fuzz {
            journey,
            seed,
            runs,
            budget,
            shrink: _shrink,
            no_confirm,
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
            bucket,
            post_comment,
            soak,
            cycle,
            repeats,
            warm,
            target,
            url,
            headless,
            locale,
            only,
            no_oracles,
            device,
            target_arg,
        } => {
            let configured_backend = if target_arg.is_none() {
                backend_config_target(cli.config.as_deref())?
            } else {
                None
            };
            if let Some(path) = target_arg.as_deref().map(PathBuf::from) {
                if path.is_file() && a2ui::looks_like_target(&path) {
                    return a2ui::run_target(&ctx, &path, "fuzz", seed, runs);
                }
                if path.is_file() && backend_headless::looks_like_schema(&path) {
                    return backend_headless::run_target(&ctx, &path, "fuzz", seed, runs).await;
                }
            } else if let Some((path, config)) = configured_backend {
                return backend_headless::run_configured_target(
                    &ctx, &path, "fuzz", seed, runs, config,
                )
                .await;
            }
            // The positional TARGET is auto-classified. A URL (https://app.com,
            // or a bare google.com / localhost:3000) points reproit at a deployed
            // app with no reproit.yaml: synthesize a web config rooted at the cwd
            // (so `.reproit/` lands here) and auto-build the map so fuzz has a
            // graph. A bare terminal EXECUTABLE (when there's no project config)
            // synthesizes a TUI/PTY config the same way. Anything else (e.g.
            // "login") scopes the hunt to that alias/node in a reproit.yaml.
            // A Playwright TEST file (`.spec.ts/.spec.js/.test.*`) is detected
            // first: reproit RUNS the test under trace, reads its action sequence,
            // and fuzzes OUTWARD from the deep state the test reached. The pitch:
            // "you wrote the test; reproit finds the bugs you didn't" -- the test's
            // own actions become the per-seed replay prefix (incl. login fills, so
            // auth comes for free), and its first page.goto becomes the start URL.
            let pw_test: Option<PathBuf> = target_arg.as_deref().and_then(|t| {
                let p = PathBuf::from(t);
                let is_test = pwfuzz::looks_like_playwright_test(t)
                    || (pwfuzz::has_pw_test_ext(t) && p.is_file());
                is_test.then_some(p)
            });
            let mut pw_capture: Option<pwfuzz::Capture> = None;
            let target_url = if pw_test.is_some() {
                None
            } else {
                target_arg.as_deref().and_then(target_as_url)
            };
            let mut synthesized = target_url.is_some() || pw_test.is_some();
            let loaded = if let Some(test_path) = &pw_test {
                let wrd = config::ensure_web_runner_dir(VERSION, &|m| ctx.say(m))?;
                let cap = pwfuzz::capture(&wrd, test_path, &|m| ctx.say(m))?;
                let base = cap.base_url.clone().or_else(|| cap.goto_url.clone());
                let Some(base) = base else {
                    return Err(anyhow::anyhow!(
                        "the test never called page.goto, so reproit has no app URL to fuzz. \
                         Add a `await page.goto(...)` to the test."
                    ));
                };
                ctx.say(format!(
                    "zero-config web run from Playwright test against {base}"
                ));
                let l = config::synthesize_web(&base, &wrd, std::env::current_dir()?)?;
                ensure_app_map(&ctx, &l, &journey).await?;
                pw_capture = Some(cap);
                l
            } else if let Some(u) = &target_url {
                let wrd = config::ensure_web_runner_dir(VERSION, &|m| ctx.say(m))?;
                ctx.say(format!("zero-config web run against {u}"));
                let l = config::synthesize_web(u, &wrd, std::env::current_dir()?)?;
                ensure_app_map(&ctx, &l, &journey).await?;
                l
            } else {
                match config::load(cli.config.as_deref()) {
                    Ok(l) => {
                        let map_journey = if target_arg.is_some() {
                            "explore"
                        } else {
                            &journey
                        };
                        ensure_app_map(&ctx, &l, map_journey).await?;
                        l
                    }
                    Err(e) => match target_arg.as_deref().and_then(target_as_executable) {
                        Some(exe) => {
                            if !confirm_tui_fuzz(&ctx, &exe) {
                                return Ok(ExitCode::SUCCESS);
                            }
                            ctx.say(format!("zero-config TUI run against `{exe}`"));
                            let l = config::synthesize_tui(&exe, std::env::current_dir()?)?;
                            ensure_app_map(&ctx, &l, &journey).await?;
                            synthesized = true;
                            l
                        }
                        None => return Err(e),
                    },
                }
            };
            // A non-URL, non-executable positional scopes the hunt to that alias/node.
            let journey = match &target_arg {
                Some(t) if !synthesized => t.clone(),
                _ => journey,
            };
            // `--soak`: the leak oracle.
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

            // A multi-actor `--from` is a verified shared-state checkpoint, not
            // a linear replay prefix. The journey conductor keeps its authored
            // business setup immutable while the multi-user fuzzer appends,
            // confirms, and shrinks seeded cross-actor schedules.
            if let Some(name) = from.as_deref() {
                if journey::is_multi_actor_target(&loaded, name)? {
                    if !locales.is_empty() {
                        return Err(anyhow::anyhow!("multi-user checkpoint fuzzing does not yet fan out `--locale`; put the desired locale in the app configuration"));
                    }
                    ctx.say(format!("fuzz: multi-user checkpoint `{name}`"));
                    let summary = journey::fuzz_multi_checkpoint(
                        &loaded,
                        name,
                        seed,
                        runs,
                        budget,
                        !no_confirm,
                    )
                    .await?;
                    ctx.say(format!(
                        "multi-user fuzz: {} confirmed bug(s), {} candidate(s)",
                        summary.confirmed, summary.candidates
                    ));
                    return Ok(if summary.confirmed > 0 {
                        exit_with(Exit::Regression)
                    } else {
                        ExitCode::SUCCESS
                    });
                }
            }

            // `--from <journey>`: resolve the journey to its replay actions
            // host-side now, so a bad/multi-actor journey fails before any drive
            // (and the secret/map resolution happens once, not per seed).
            //
            // A Playwright-test target produces the SAME kind of replay prefix from
            // the captured trace: its mapped actions become the per-seed prefix the
            // runner replays before exploring, and its start URL pins the runner's
            // gotoUrl so every seed lands on the same page the test reached.
            let from_prefix = if let Some(cap) = &pw_capture {
                pwfuzz::report(cap, pw_test.as_deref().unwrap_or(Path::new("test")), &|m| {
                    ctx.say(m)
                });
                // The start URL already rode into the synthesized config's app.url
                // (-> REPROIT_URL -> the runner's APP_URL/START_URL), so every seed
                // lands on the page the test reached before replaying the prefix.
                let prefix = cap.replay_prefix();
                if prefix.is_empty() {
                    ctx.say(
                        "  no replayable actions captured from the test; fuzzing from the \
                         start URL only.",
                    );
                    None
                } else {
                    Some(prefix)
                }
            } else {
                match &from {
                    Some(name) => Some(journey::prefix_actions(&loaded, name)?),
                    None => None,
                }
            };

            let args = fuzz::FuzzArgs {
                journey,
                seed,
                runs,
                budget,
                shrink: !no_confirm,
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
                app_bucket: bucket,
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
                if let Some(dev) = pick_device_interactive(
                    None,
                    &crosscut::platform_targets(&loaded.config.app.platform),
                )
                .await
                {
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

            let fuzz_summary = fuzz::fuzz(&loaded.config, &loaded.root, &args).await?;
            // --json: surface the findings artifact (the discovered repro, by
            // content-hash id, plus its seed/actions) so the agent/MCP bridge
            // can keep it without re-parsing the human report.
            if ctx.json {
                match latest_finding(&loaded) {
                    Some(f) => ctx.emit(&serde_json::json!({
                        "command": "fuzz",
                        "complete": fuzz_summary.complete,
                        "seeds_run": fuzz_summary.seeds_run,
                        "seeds_requested": fuzz_summary.seeds_requested,
                        "found": true,
                        "id": repro::display_finding_id(&f.id()),
                        "kind": "finding",
                        "seed": f.seed,
                        "actions": f.actions,
                        "artifact": f.run_dir.to_string_lossy(),
                    })),
                    None => ctx.emit(&serde_json::json!({
                        "command": "fuzz",
                        "complete": fuzz_summary.complete,
                        "seeds_run": fuzz_summary.seeds_run,
                        "seeds_requested": fuzz_summary.seeds_requested,
                        "found": false,
                    })),
                }
            }
            Ok(if fuzz_summary.complete {
                ExitCode::SUCCESS
            } else {
                exit_with(Exit::Regression)
            })
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
        Cmd::Auth {
            account,
            strategy,
            email,
            phone,
            username,
            password,
            otp,
            totp_secret,
            session,
            user_id,
            validate_text,
            no_discover,
        } => {
            let loaded = config::load(cli.config.as_deref())?;
            let exists = loaded
                .config
                .auth
                .accounts
                .iter()
                .any(|a| a.name == account);
            let mut strategy = strategy;
            let mut email = email;
            let mut phone = phone;
            let mut password = password;
            let mut otp = otp;
            let has_new_values = strategy.is_some()
                || email.is_some()
                || phone.is_some()
                || username.is_some()
                || password.is_some()
                || otp.is_some()
                || totp_secret.is_some()
                || session.is_some();
            if exists && !has_new_values {
                discover_and_verify_login(cli.config.as_deref(), &account).await?;
            } else {
                if !exists && !has_new_values {
                    if ctx.yes || !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                        anyhow::bail!("new account `{account}` needs credentials; pass --email/--phone/--session");
                    }
                    println!(
                        "  Setting up {account}. Login mapping and verification are automatic."
                    );
                    println!("  Sign-in type: [1] email/password  [2] phone/OTP");
                    match auth_prompt("choice", false)?.as_str() {
                        "1" => {
                            strategy = Some(AuthStrategyArg::Password);
                            email = Some(auth_prompt("email", false)?);
                            password = Some(auth_prompt("password", true)?);
                        }
                        "2" => {
                            strategy = Some(AuthStrategyArg::PhoneOtp);
                            phone = Some(auth_prompt("phone", false)?);
                            otp = Some(auth_prompt("test OTP", true)?);
                        }
                        other => anyhow::bail!("unknown sign-in type `{other}`"),
                    }
                }
                let strategy = strategy.or_else(|| {
                    if session.is_some() { Some(AuthStrategyArg::Session) }
                    else if phone.is_some() { Some(AuthStrategyArg::PhoneOtp) }
                    else if otp.is_some() || totp_secret.is_some() { Some(AuthStrategyArg::PasswordOtp) }
                    else if email.is_some() || username.is_some() || password.is_some() { Some(AuthStrategyArg::Password) }
                    else { None }
                }).ok_or_else(|| anyhow::anyhow!(
                    "cannot create `{account}` without credentials; pass --email/--phone/--session (strategy is inferred)"
                ))?;
                auth_cmd(
                    cli.config.as_deref(),
                    AuthAction::Add {
                        account,
                        strategy,
                        email,
                        phone,
                        username,
                        password,
                        otp,
                        totp_secret,
                        session,
                        user_id,
                        validate_text,
                        no_discover,
                    },
                )
                .await?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Journey { action } => {
            if matches!(
                &action,
                JourneyAction::Create { .. } | JourneyAction::Run(_)
            ) {
                let loaded = config::load(cli.config.as_deref())?;
                ensure_app_map(&ctx, &loaded, "explore").await?;
            }
            if let JourneyAction::Run(args) = &action {
                let [name] = args.as_slice() else {
                    anyhow::bail!("usage: reproit journey <name>");
                };
                let loaded = config::load(cli.config.as_deref())?;
                let result = journey::run(
                    &loaded,
                    name,
                    loaded.config.gate.runs.max(1),
                    ctx.json || ctx.quiet,
                )
                .await?;
                if ctx.json {
                    ctx.emit(&serde_json::json!({
                        "command": "journey",
                        "journey": name,
                        "outcome": result.outcome.as_str(),
                        "rate": result.rate(),
                        "exit": result.outcome.exit_code(),
                    }));
                } else {
                    ctx.say(format!(
                        "\njourney: {} ({})  {name}",
                        result.outcome.as_str().to_uppercase(),
                        result.rate()
                    ));
                }
                return Ok(ExitCode::from(result.outcome.exit_code()));
            }
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
            ensure_app_map(&ctx, &loaded, "explore").await?;
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
            let loaded = config::load(cli.config.as_deref())?;
            ensure_app_map(&ctx, &loaded, "explore").await?;
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
                        eprintln!(
                            "  (is the cloud reachable? check REPROIT_CLOUD_URL / `reproit login`)"
                        );
                    }
                    Ok(exit_with(Exit::Regression))
                }
            }
        }
        Cmd::TuiRun => {
            tui::run()?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::UiaRun => {
            #[cfg(windows)]
            {
                uia::run()?;
                Ok(ExitCode::SUCCESS)
            }
            #[cfg(not(windows))]
            {
                anyhow::bail!("__uia (Windows UI Automation) is unsupported on this platform")
            }
        }
        Cmd::AtspiRun => {
            #[cfg(target_os = "linux")]
            {
                atspi::run()?;
                Ok(ExitCode::SUCCESS)
            }
            #[cfg(not(target_os = "linux"))]
            {
                anyhow::bail!("__atspi (Linux AT-SPI) is unsupported on this platform")
            }
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
        Cmd::Repro {
            action: ReproAction::Why { dir, top },
        } => {
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

/// Whether the interactive device picker should appear for this run. The picker
/// selects a simulator that REPROIT provisions, which only the FlutterDrive
/// backend does (it boots the sim via simctl). Every other backend brings its
/// own target (Appium via caps, web a browser, desktop the host, TUI a PTY), so
/// none need reproit's picker. Even FlutterDrive defaults to the headless
/// `flutter test` tier (no device) unless --sim. `--target`/`--device` bypass
/// this upstream.
fn run_needs_device_pick(platform: &str, sim: bool) -> bool {
    match platform::backend(platform) {
        Some(b) if b.provisions_device() => sim,
        _ => false,
    }
}

/// Interactive device picker: enumerate devices, filter to the targets the
/// project supports, print a numbered list, and read a selection from stdin.
/// When `want_name` is given, match it without prompting. Returns None if there
/// are no devices or the selection is invalid/empty (the caller then falls back
/// to the config default rather than hanging).
async fn pick_device_interactive(
    want_name: Option<&str>,
    allowed: &[crosscut::Target],
) -> Option<crosscut::Device> {
    let mut devices = enumerate_devices().await;
    if !allowed.is_empty() {
        devices.retain(|d| allowed.contains(&d.target));
    }
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
        return pick_device_interactive(None, &crosscut::platform_targets(platform)).await;
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
/// Web engines (chromium/firefox/webkit) are the direct runtime fanout. Mobile
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
                "id": repro::display_repro_id(&meta.id),
                "kind": "repro",
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
                .unwrap_or_else(|| repro::display_repro_id(id));
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
            .map(|(id, on)| serde_json::json!({
                "id": repro::display_repro_id(id),
                "kind": "repro",
                "fails_only_on": on
            }))
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
///     the SAME seeded walk on each engine and diffs the findings.
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
        let result = fuzz::fuzz_targeted(&loaded.config, &loaded.root, &base).await?;
        let complete = result.complete;
        per_target.push((label, result.signatures));
        if !complete {
            return Ok(exit_with(Exit::Regression));
        }
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

/// Resolve the effective cloud (url, key) for a cloud subcommand. Precedence:
///   url:  --cloud flag > $REPROIT_CLOUD_URL > the persisted login url.
///   key:  --key flag > $REPROIT_CLOUD_KEY (the project key, sk_live_...) >
///         the persisted login key.
/// This is the single place the persisted login is read so every `cloud` command
/// honors it.
fn cloud_creds(cloud: Option<String>, key: Option<String>) -> (Option<String>, Option<String>) {
    let persisted = crosscut::load_token(&crosscut::token_path());
    let url = cloud
        .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
        .or_else(|| persisted.as_ref().and_then(|(_, u)| u.clone()));
    let key = key
        .or_else(|| std::env::var("REPROIT_CLOUD_KEY").ok())
        .or_else(|| persisted.as_ref().map(|(t, _)| t.clone()));
    (url, key)
}

/// Resolve the selected cloud project. Explicit flag and environment override
/// the profile written by setup; no command should make users repeatedly paste
/// an app id after they selected it once.
fn cloud_app_id(app: Option<String>) -> Result<String> {
    app.or_else(|| std::env::var("REPROIT_CLOUD_APP").ok())
        .or_else(|| crosscut::load_cloud_app(&crosscut::token_path()))
        .ok_or_else(|| {
            anyhow::anyhow!("no cloud project selected: run `reproit cloud setup --app <app>` once")
        })
}

/// Dispatch the `cloud` subcommands onto the existing triage::*/deliver::*
/// handlers. `login` persists the cloud/project key; every other command resolves
/// the key via `cloud_creds` and uses it as a bearer. Network failures surface as
/// a clear message (the triage layer bails rather than panicking).
async fn cloud_cmd(
    config_path: Option<&std::path::Path>,
    action: CloudAction,
    json: bool,
) -> Result<()> {
    match action {
        CloudAction::Login { cloud, key, app } => {
            let url = cloud
                .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
                .unwrap_or_else(|| "https://cloud.reproit.com".into());
            // Key precedence: --key > REPROIT_CLOUD_KEY (project key).
            let token = key.or_else(|| std::env::var("REPROIT_CLOUD_KEY").ok());
            let Some(token) = token else {
                anyhow::bail!(
                    "no cloud key: pass --key or set REPROIT_CLOUD_KEY (the sk_live_... project key from the cloud dashboard)"
                );
            };
            // Validate BEFORE persisting: a login that stores an unusable key is a
            // worse failure mode than failing loudly now. With --app, validate
            // against the app's buckets; otherwise against /v1/me. A 401/403 fails
            // clearly (bad key); a transient network error is a soft warning (the
            // key may still be fine, so we store it and let the user retry).
            match triage::validate_login(&url, &token, app.as_deref()).await {
                Ok(desc) => {
                    let path = crosscut::token_path();
                    crosscut::save_token(&path, &token, &url)?;
                    println!("cloud url:     {url}");
                    println!(
                        "cloud key:     stored ({} chars) in {}",
                        token.len(),
                        path.display()
                    );
                    println!("validated:     ok ({desc})");
                    Ok(())
                }
                Err(e) => anyhow::bail!(
                    "login failed and no credential was stored: {e}. Reproit only saves a key after the cloud verifies it"
                ),
            }
        }
        CloudAction::Setup {
            app,
            key,
            cloud,
            dispatch_token,
            repo,
            workflow_path,
            no_workflow,
        } => {
            // Root at the git repo top (where `.github/workflows` must live),
            // independent of any reproit.yaml (which may be nested or absent);
            // fall back to cwd when not in a git repo. The platform hint for the
            // SDK line is a best-effort read of a local config, and must NOT
            // decide the root (a config found by climbing ancestors would write
            // the workflow to the wrong tree).
            let root = triage::git_toplevel()
                .map(Ok)
                .unwrap_or_else(std::env::current_dir)?;
            let platform = config::load(config_path)
                .ok()
                .map(|l| l.config.app.platform);
            triage::setup(
                &root,
                &app,
                cloud,
                key,
                dispatch_token,
                repo,
                workflow_path,
                !no_workflow,
                platform,
            )
            .await
        }
        CloudAction::Fuzz {
            app,
            journey,
            pr,
            cloud,
            bucket,
        } => {
            // Run through the existing local fuzz engine with Cloud delivery:
            // set --cloud + --app, then post the PR comment when linked.
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
                app_bucket: bucket,
                post_comment: pr.is_some(),
                json: false,
                locales: Vec::new(),
                oracle_filter: crosscut::OracleFilter::all(),
                from_prefix: None,
            };
            fuzz::fuzz(&loaded.config, &loaded.root, &args)
                .await
                .map(|_| ())
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
                // Raw findings JSON straight from GET /v1/errors/:app, with
                // the same message filter as the rendered view.
                let v = triage::raw(&app, "", cloud, key).await?;
                let v = triage::filter_errors(v, query.as_deref());
                println!("{}", serde_json::to_string_pretty(&v)?);
                Ok(())
            } else {
                triage::find(&app, query.as_deref(), cloud, key).await
            }
        }
        CloudAction::BlastRadius {
            app,
            bucket,
            sig,
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
                triage::explain(&app, bucket.as_deref(), sig.as_deref(), cloud, key).await
            }
        }
        CloudAction::ReplayDispatch {
            app,
            bucket,
            as_name,
            run,
            run_id,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            // Bucket-first: pull -> check, in one step. Reuses the pull + check
            // code paths so the saved repro carries its fixture.
            let loaded = config::load(config_path)?;
            triage::reproduce_bucket(
                &loaded.root,
                &app,
                &bucket,
                &as_name,
                run,
                run_id,
                json,
                cloud,
                key,
            )
            .await
        }
        CloudAction::Pull {
            app,
            bucket,
            top,
            as_name,
            cloud,
            key,
        } => {
            // Resolve the local repro store root so the pulled repro lands as a
            // first-class saved repro under .reproit/repros/, just like `keep`.
            let loaded = config::load(config_path)?;
            let (cloud, key) = cloud_creds(cloud, key);
            let bucket = match (bucket, top) {
                (Some(bucket), false) => bucket,
                (None, true) => triage::top_bucket_id(&app, cloud.clone(), key.clone()).await?,
                (None, false) => {
                    anyhow::bail!("missing bucket: pass --bucket <bkt_...> or use --top")
                }
                (Some(_), true) => unreachable!("clap conflicts_with prevents --bucket + --top"),
            };
            triage::pull(&loaded.root, &app, &bucket, &as_name, json, cloud, key).await
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
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::diagnose(&app, &report, run, cloud, key).await
        }
        CloudAction::Query {
            app,
            query,
            export,
            cloud,
            key,
        } => {
            // Bucket-first data out for your own analysis: GET
            // /v1/apps/:app/buckets, filtered by --query when given. With
            // --export, emit the raw JSON; otherwise render the same list as
            // `cloud buckets`.
            let (cloud, key) = cloud_creds(cloud, key);
            if export {
                let v = triage::raw_buckets(&app, cloud, key).await?;
                let v = triage::filter_buckets(v, query.as_deref());
                println!("{}", serde_json::to_string_pretty(&v)?);
                Ok(())
            } else {
                triage::buckets(&app, query.as_deref(), false, cloud, key).await
            }
        }
    }
}

/// A human label for a repro in CLI output: `<id> (<alias>)` when an alias is
/// set, else just the id.
fn repro_label(m: &repro::Meta) -> String {
    let id = repro::display_repro_id(&m.id);
    match &m.alias {
        Some(a) => format!("{id} ({a})"),
        None => id,
    }
}

fn pending_label(id: &str) -> String {
    repro::display_finding_id(id)
}

fn check_label(m: &repro::Meta) -> String {
    if m.created.is_empty() {
        pending_label(&m.id)
    } else {
        repro_label(m)
    }
}

fn public_json_id(m: &repro::Meta) -> String {
    if m.created.is_empty() {
        repro::display_finding_id(&m.id)
    } else {
        repro::display_repro_id(&m.id)
    }
}

fn public_json_kind(m: &repro::Meta) -> &'static str {
    if m.created.is_empty() {
        "finding"
    } else {
        "repro"
    }
}

/// One finding from a fuzz artifact: the seed, the minimized action sequence,
/// and the source `fuzz.md`'s run dir (for evidence/copying).
struct Finding {
    id: String,
    seed: u64,
    actions: Vec<String>,
    run_dir: PathBuf,
}

impl Finding {
    /// Scoped content id persisted by fuzz (target + bug + replay identity).
    fn id(&self) -> String {
        self.id.clone()
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
            record_url: None,
            record_action: None,
        }
    }
}

/// Find a fuzz finding by its content-hash id, scanning EVERY run dir under the
/// evidence out dir (not just the latest), so `check <id>` can confirm any
/// finding the last `fuzz` reported, before it is `keep`-ed. Returns the first
/// dir whose `fuzz.md` repro block hashes to `id`.
fn find_finding_by_id(loaded: &config::Loaded, id: &str) -> Option<Finding> {
    // Direct `reproit fnd_...` syntax is normalized into the internal raw id
    // before command dispatch. Explicit `check fnd_...` still arrives prefixed.
    // Accept both forms at this internal lookup boundary.
    let id = repro::raw_finding_id(id)
        .or_else(|| (id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit())).then_some(id))?;
    let durable = loaded.root.join(".reproit/findings").join(id);
    if let Some(finding) = finding_from_report_dir(&durable, id) {
        return Some(finding);
    }
    let base = loaded.root.join(&loaded.config.evidence.out_dir);
    for e in std::fs::read_dir(&base).ok()?.flatten() {
        let p = e.path();
        if let Some(finding) = finding_from_report_dir(&p, id) {
            return Some(finding);
        }
    }
    None
}

fn finding_from_report_dir(run_dir: &Path, id: &str) -> Option<Finding> {
    if !run_dir.is_dir() {
        return None;
    }
    let md = std::fs::read_to_string(run_dir.join("fuzz.md")).ok()?;
    let (seed, actions) = parse_fuzz_report(&md)?;
    // New reports persist their scoped finding id. Legacy reports remain
    // addressable through their historical seed+actions hash.
    let declared = parse_fuzz_finding_id(&md);
    let matches = declared.as_deref() == Some(id)
        || (declared.is_none() && repro::repro_id(seed, &actions) == id);
    matches.then_some(Finding {
        id: id.to_string(),
        seed,
        actions,
        run_dir: run_dir.to_path_buf(),
    })
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
        id: parse_fuzz_finding_id(&md).unwrap_or_else(|| repro::repro_id(seed, &actions)),
        seed,
        actions,
        run_dir,
    })
}

fn parse_fuzz_finding_id(md: &str) -> Option<String> {
    md.lines().find_map(|line| {
        line.trim()
            .strip_prefix("<!-- finding-id:")
            .and_then(|value| value.strip_suffix("-->"))
            .map(str::trim)
            .filter(|value| value.len() == 12 && value.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_string)
    })
}

/// Parse a `fuzz.md` report into (seed, repro actions). The report header is
/// `# fuzz finding (seed N)` and the repro block is the fenced code under a
/// `## confirmed repro (...)` heading (one action per line). Pure, so it is
/// unit-tested.
fn parse_fuzz_report(md: &str) -> Option<(u64, Vec<String>)> {
    let seed = md.lines().find_map(|l| {
        let i = l.find("(seed ")? + "(seed ".len();
        let rest = &l[i..];
        let end = rest.find(')')?;
        rest[..end].trim().parse::<u64>().ok()
    })?;
    // The repro block: the first fence after the report writer's confirmed
    // repro heading.
    let mut in_repro_section = false;
    let mut in_fence = false;
    let mut actions = Vec::new();
    for line in md.lines() {
        if line.starts_with("## confirmed repro") {
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

/// Prefer a direct screen URL for recordings. Legacy repros lack this metadata
/// and retain their original full replay unchanged.
fn minimize_record_replay(replay: &mut serde_json::Value, meta: &repro::Meta) {
    let Some(url) = meta.record_url.as_ref() else {
        return;
    };
    let Some(obj) = replay.as_object_mut() else {
        return;
    };
    obj.insert("gotoUrl".into(), serde_json::Value::String(url.clone()));
    let actions = meta
        .record_action
        .iter()
        .cloned()
        .map(serde_json::Value::String)
        .collect();
    obj.insert("replay".into(), serde_json::Value::Array(actions));
}

fn web_record_metadata(
    app_url: Option<&str>,
    oracle: Option<&str>,
    sig: Option<&str>,
    log: &str,
) -> (Option<String>, Option<String>) {
    let (Some(app_url), Some(oracle), Some(sig)) = (app_url, oracle, sig) else {
        return (None, None);
    };
    let state_present = matches!(
        oracle,
        "content-bug"
            | "choice-anomaly"
            | "broken-route"
            | "occlusion"
            | "security"
            | "stuck-keyboard"
            | "blank-screen"
            | "broken-asset"
            | "zoom-reflow"
            | "invariant"
            | "safe-area"
    );
    if !state_present && oracle != "flicker" {
        return (None, None);
    }
    let obs = crate::map::parse_run(log);
    let Some(route) = obs.routes.get(sig) else {
        return (None, None);
    };
    let Some(origin) = app_url_origin(app_url) else {
        return (None, None);
    };
    let url = format!("{origin}{route}");
    if state_present {
        return (Some(url), None);
    }
    let action = obs
        .rerenders
        .keys()
        .chain(obs.paint_flickers.keys())
        .find_map(|(from, action)| (from == sig).then(|| action.clone()));
    match action {
        Some(action) => (Some(url), Some(action)),
        None => (None, None),
    }
}

fn app_url_origin(url: &str) -> Option<&str> {
    let authority = url.find("://")? + 3;
    let end = url[authority..]
        .find(['/', '?', '#'])
        .map(|i| authority + i)
        .unwrap_or(url.len());
    Some(&url[..end])
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
/// per-id recording slot so future `watch`es are instant and precise.
///
/// Lookup order: the per-id recording slot
/// (`.reproit/recordings/repro/<id>/video.*`) first; else the newest recording
/// under `.reproit/runs/` (the one you just produced with `record <id>`), which
/// we then copy into the per-id slot. Bails with a how-to if neither exists.
/// `.reproit/recordings/` is gitignored, so cached videos can never be committed
/// by accident.
fn resolve_repro_video(loaded: &config::Loaded, id_or_alias: &str) -> Result<PathBuf> {
    let root = loaded.root.as_path();
    // Key media by the canonical content-hash id (so an alias and its id share
    // one cached file); pending findings use their public `fnd_...` id.
    let id = if let Some(m) = repro::resolve(root, id_or_alias) {
        m.id
    } else if let Some(id) = repro::raw_finding_id(id_or_alias) {
        id.to_string()
    } else {
        anyhow::bail!(
            "no repro or finding `{id_or_alias}`. Use a saved alias, rep_..., or fnd_..."
        );
    };
    let recording_dir = layout::repro_recording_dir(root, &id);

    // 1. Already cached for this id.
    if let Some(v) = newest_video_in(&recording_dir, Some("video")) {
        return Ok(v);
    }
    // 2. Newest recording from any run; promote it into the per-id recording slot.
    if let Some(src) = newest_video_in(&root.join(&loaded.config.evidence.out_dir), None) {
        std::fs::create_dir_all(&recording_dir)?;
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("webm");
        let dest = layout::repro_video_path(root, &id, ext);
        std::fs::copy(&src, &dest)
            .map_err(|e| anyhow::anyhow!("caching recording to {}: {e}", dest.display()))?;
        return Ok(dest);
    }
    anyhow::bail!("no recording for `{id_or_alias}`. Make one with:  reproit record {id_or_alias}")
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
    let finding_capsule = root
        .join(".reproit/findings")
        .join(&computed)
        .join("capsule-id");
    if let Ok(id) = std::fs::read_to_string(finding_capsule) {
        std::fs::write(dir.join("capsule-id"), id)?;
    }
    let finding_contract = root
        .join(".reproit/findings")
        .join(&computed)
        .join("contract.json");
    if finding_contract.exists() {
        std::fs::copy(finding_contract, dir.join("contract.json"))?;
    }
    let finding_backend_contract = root
        .join(".reproit/findings")
        .join(&computed)
        .join("backend-contract.json");
    if finding_backend_contract.exists() {
        std::fs::copy(finding_backend_contract, dir.join("backend-contract.json"))?;
    }

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
    // Record the finding's ORACLE category and violating state sig. `keep` reads
    // these from the `## oracle` block fuzz.md emits.
    let md = std::fs::read_to_string(finding.run_dir.join("fuzz.md")).unwrap_or_default();
    let (oracle, finding_sig) = parse_fuzz_oracle(&md);
    // Crash findings use the exception path; state findings retain the signature
    // for direct recording and existing sig-reached logic.
    let trigger_sig = finding_sig.filter(|s| !s.is_empty());
    let log = std::fs::read_to_string(finding.run_dir.join("drive-a.log")).unwrap_or_default();
    let (record_url, record_action) = web_record_metadata(
        loaded.config.app.url.as_deref(),
        oracle.as_deref(),
        trigger_sig.as_deref(),
        &log,
    );
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
        record_url,
        record_action,
    };
    repro::save_meta(root, &meta)?;

    // Was this already in the suite? If so, report it as "already saved" (and
    // note an alias rename) instead of pretending it's a fresh keep.
    let prior_alias = existing.as_ref().and_then(|m| m.alias.clone());
    let renamed = match (&prior_alias, as_name) {
        (Some(old), Some(new)) if old != new => Some((old.clone(), new.to_string())),
        _ => None,
    };
    let public_id = repro::display_repro_id(&computed);
    let source_id = repro::display_finding_id(&computed);
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "keep",
            "id": public_id,
            "kind": "repro",
            "source_id": source_id,
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
                "  already saved ({}); alias {old} -> {new}",
                public_id
            )),
            None => {
                let label = alias.as_deref().unwrap_or(&public_id);
                ctx.say(format!("  already saved as {label} ({})", status.as_str()));
            }
        }
        ctx.say(format!("  reproduce: reproit {public_id}"));
    } else {
        ctx.say(format!("  kept {} ({})", public_id, status.as_str()));
        if let Some(a) = &alias {
            ctx.say(format!("  alias: {a}"));
        }
        ctx.say(format!("  verify: reproit {public_id}"));
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
/// Build the simplified repro's replay.json: the minimized ACTIONS plus the seed,
/// carrying over the property-matched fixture (`inputs`/`locale`) from the source
/// repro so a data-dependent bug still reproduces after simplification (simplify
/// minimizes actions, never the data). A source without a fixture (a path-only
/// repro, or a pending finding with no replay.json) yields the bare
/// `{seed, replay}`. Pure, so it is unit-tested.
fn build_simplified_replay(
    seed: u64,
    candidate: &[String],
    src_replay: &serde_json::Value,
) -> serde_json::Value {
    let mut replay = serde_json::json!({ "seed": seed, "replay": candidate });
    for k in ["inputs", "locale"] {
        if let Some(v) = src_replay.get(k) {
            replay[k] = v.clone();
        }
    }
    replay
}

fn adopt_simplified(
    loaded: &config::Loaded,
    meta: &repro::Meta,
    candidate: &[String],
    new_id: &str,
) -> Result<()> {
    let root = loaded.root.as_path();
    let new_dir = repro::repro_dir(root, new_id);
    std::fs::create_dir_all(&new_dir)?;
    // Carry the property-matched fixture (inputs/locale) from the source repro so a
    // data-dependent bug still reproduces after simplification: we minimize ACTIONS,
    // never the data. A path-only repro (or a pending finding with no replay.json)
    // carries neither, so this is inert for non-data bugs.
    let src_replay: serde_json::Value =
        std::fs::read_to_string(repro::repro_dir(root, &meta.id).join("replay.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
    let replay = build_simplified_replay(meta.seed, candidate, &src_replay);
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
        record_url: meta.record_url.clone(),
        record_action: meta.record_action.clone(),
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

/// Resolve the journey a kept repro replays under (for `record`). Repros
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
    let frozen_contract = crate::contracts::FrozenContractGuard::load(&dir.join("contract.json"))
        .or_else(|| {
            crate::contracts::FrozenContractGuard::load(
                &loaded
                    .root
                    .join(".reproit/findings")
                    .join(id)
                    .join("contract.json"),
            )
        });
    let frozen_backend = crate::backend::FrozenBackendGuard::load(
        &dir.join("backend-contract.json"),
    )
    .or_else(|| {
        crate::backend::FrozenBackendGuard::load(
            &loaded
                .root
                .join(".reproit/findings")
                .join(id)
                .join("backend-contract.json"),
        )
    });
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

    // Verify an alternate sequence (simplify): replace ONLY the actions, keeping
    // the seed AND the property-matched fixture (inputs/locale) so the verdict
    // still answers "does this reproduce the SAME finding?". Dropping the fixture
    // here would re-run each candidate WITHOUT the data a data-dependent bug needs,
    // so the minimization would shrink against a bug that never fires (garbage
    // result), and the adopted minimal repro would lose its data and stop
    // reproducing. Clone + overwrite the action list to preserve the rest.
    let replay = match override_actions {
        Some(actions) => {
            let mut r = replay.clone();
            r["replay"] = serde_json::json!(actions);
            r
        }
        None => replay,
    };

    // The fuzz config the explorer reads on each replay.
    let cfg_path = layout::fuzz_config_path(&loaded.root);
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    let mut defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    // LOCALE contract: the locale travels to the runner as REPROIT_LOCALE (a
    // dart-define for Flutter, an env var for the rest, both via the
    // orchestrator's define list), so a repro can be replayed under each locale.
    // Precedence: an explicit `--locale` (the cross-locale matrix) wins; otherwise
    // fall back to a `locale` pinned in the stored replay.json by `cloud pull` /
    // `reproduce` (the property-matched fixture's locale), so a locale-dependent
    // prod bug reproduces under a plain `reproit check <name>` without the caller
    // having to remember which locale it came from. The runner reads `inputs` off
    // the config directly, but reads locale ONLY from REPROIT_LOCALE, so the
    // fixture locale must be lifted here.
    let fixture_locale = replay.get("locale").and_then(|v| v.as_str());
    if let Some(loc) = locale.or(fixture_locale) {
        defines.push((crosscut::LOCALE_ENV.to_string(), loc.to_string()));
    }
    let finding_capsule_link = loaded
        .root
        .join(".reproit/findings")
        .join(id)
        .join("capsule-id");
    let kept_capsule_link = dir.join("capsule-id");
    let capsule_id = std::fs::read_to_string(&finding_capsule_link)
        .or_else(|_| std::fs::read_to_string(&kept_capsule_link))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut capsule_plaintext = None;
    if let Some(capsule_id) = capsule_id {
        let capsule = capsule::Capsule::load(&loaded.root, &capsule_id)?;
        let missing = capsule.missing_required_replay_capabilities();
        if !missing.is_empty() {
            anyhow::bail!(
                "capsule `{capsule_id}` cannot replay on `{}`; missing capability: {}",
                loaded.config.app.platform,
                missing.join(", ")
            );
        }
        let guard = capsule::Capsule::materialize_plaintext(&loaded.root, &capsule_id)?;
        defines.push((
            "REPROIT_CAPSULE".into(),
            guard.path().to_string_lossy().into_owned(),
        ));
        capsule_plaintext = Some(guard);
    }

    let _ = devices; // a repro replays on one device; kept for parity.
                     // The N repeat-replays (flakiness detection) run in a SINGLE drive session:
                     // we hand the runner a batch of N identical replays, so the browser/app
                     // launches ONCE instead of N cold starts (the agent inner loop's main
                     // latency). The runner brackets each replay with SEED:BEGIN/SEED:END, so we
                     // split the one drive log back into N per-replay segments and classify each
                     // exactly as before. A single replay (times == 1) keeps the compact
                     // bare-config shape. This is a pure latency change: same N replays, same
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
        let mut verdict = repro::verdict_from_log_with_trigger(seg, outcome.passed, &trigger);
        if let Some(guard) = &frozen_contract {
            let observations = crate::observation::from_runner_log(seg, &[]);
            if guard.reproduces(&observations) {
                verdict = repro::RunVerdict::Broke;
            }
        }
        if frozen_backend
            .as_ref()
            .is_some_and(|guard| guard.reproduces(seg))
        {
            verdict = repro::RunVerdict::Broke;
        }
        if !quiet {
            println!("  run {}/{}: {}", i + 1, segments.len(), verdict.as_str());
        }
        verdicts.push(verdict);
    }
    let last_dir = outcome.run_dir;
    drop(capsule_plaintext);
    // Neutralize: a later warm run must not replay this case.
    let _ = std::fs::write(&cfg_path, "{}");
    Ok((repro::CheckResult::from_verdicts(&verdicts), last_dir))
}

/// Print the platform support matrix: every registered UI framework and the
/// backend it routes to.
fn print_platforms() {
    println!("Platform support matrix (UI framework -> introspection backend)\n");
    println!("  {:<16} {:<26} CAPABILITY", "PLATFORM", "BACKEND");
    for p in platform::all() {
        println!("  {:<16} {:<26} {}", p.id, p.backend.as_str(), p.note);
    }
    println!(
        "\n  All listed platform IDs are live. Local readiness still depends on \
         `reproit doctor` and host tooling.\n\
         \n  The point: Qt/GTK/WinUI/Avalonia/wxWidgets share ONE backend per OS\n\
         (they publish to the OS accessibility API), Electron/Tauri reuse the\n\
         web backend, Appium covers native mobile, TUI uses a PTY, and only\n\
         immediate-mode GUIs (imgui, clay) need an in-app hook."
    );
}

/// Resolve the vault path from config (or cwd default when no config is found).
fn resolve_vault_path(config_path: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Ok(l) = config::load(config_path) {
        Ok(l.config
            .auth
            .vault
            .as_ref()
            .map(|path| l.root.join(path))
            .unwrap_or_else(|| layout::secrets_vault_path(&l.root)))
    } else {
        Ok(layout::secrets_vault_path(&std::env::current_dir()?))
    }
}

fn resolve_config_path(config_path: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = config_path {
        return Ok(p.to_path_buf());
    }
    let mut dir = std::env::current_dir()?;
    loop {
        let p = dir.join("reproit.yaml");
        if p.exists() {
            return Ok(p);
        }
        if !dir.pop() {
            anyhow::bail!("no reproit.yaml found; pass --config or run `reproit init` first");
        }
    }
}

fn backend_config_target(
    config_path: Option<&Path>,
) -> Result<Option<(PathBuf, backend::BackendConfig)>> {
    let path = match config_path {
        Some(path) if path.is_file() => path.to_path_buf(),
        Some(path) => anyhow::bail!("config file {} does not exist", path.display()),
        None => {
            let mut directory = std::env::current_dir()?;
            loop {
                let candidate = directory.join("reproit.yaml");
                if candidate.is_file() {
                    break candidate;
                }
                if !directory.pop() {
                    return Ok(None);
                }
            }
        }
    };
    let document: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&path)?)?;
    if document.get("app").is_some() {
        return Ok(None);
    }
    let Some(backend) = document.get("backend") else {
        return Ok(None);
    };
    let config: backend::BackendConfig = serde_yaml::from_value(backend.clone())?;
    if !config.enabled {
        return Ok(None);
    }
    let schema = config
        .schemas
        .first()
        .context("backend.enabled is true but backend.schemas is empty")?;
    let target = path.parent().unwrap_or_else(|| Path::new(".")).join(schema);
    if !target.is_file() {
        anyhow::bail!("backend schema {} does not exist", target.display());
    }
    Ok(Some((target, config)))
}

fn yaml_str(s: impl Into<String>) -> serde_yaml::Value {
    serde_yaml::Value::String(s.into())
}

fn yaml_mapping_mut(v: &mut serde_yaml::Value) -> Result<&mut serde_yaml::Mapping> {
    match v {
        serde_yaml::Value::Mapping(m) => Ok(m),
        _ => anyhow::bail!("reproit.yaml must be a YAML mapping"),
    }
}

fn yaml_child_mapping<'a>(
    parent: &'a mut serde_yaml::Mapping,
    key: &str,
) -> Result<&'a mut serde_yaml::Mapping> {
    let k = yaml_str(key);
    if !parent.contains_key(&k) {
        parent.insert(k.clone(), serde_yaml::Value::Mapping(Default::default()));
    }
    match parent.get_mut(&k) {
        Some(serde_yaml::Value::Mapping(m)) => Ok(m),
        _ => anyhow::bail!("`{key}` in reproit.yaml must be a mapping"),
    }
}

fn yaml_child_sequence<'a>(
    parent: &'a mut serde_yaml::Mapping,
    key: &str,
) -> Result<&'a mut Vec<serde_yaml::Value>> {
    let k = yaml_str(key);
    if !parent.contains_key(&k) {
        parent.insert(k.clone(), serde_yaml::Value::Sequence(Vec::new()));
    }
    match parent.get_mut(&k) {
        Some(serde_yaml::Value::Sequence(s)) => Ok(s),
        _ => anyhow::bail!("`{key}` in reproit.yaml must be a list"),
    }
}

fn account_ref(account: &str, field: &str) -> String {
    format!("{account}.{field}")
}

fn insert_yaml_opt(map: &mut serde_yaml::Mapping, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|s| !s.trim().is_empty()) {
        map.insert(yaml_str(key), yaml_str(value));
    }
}

fn store_secret_opt(
    vault: &mut auth::Vault,
    account: &str,
    field: &str,
    value: Option<String>,
) -> Option<String> {
    let key = account_ref(account, field);
    if let Some(value) = value.filter(|s| !s.is_empty()) {
        vault.set(&key, &value);
    }
    Some(key)
}

fn update_account_config(
    config_path: &Path,
    account: &str,
    strategy: config::AuthStrategy,
    refs: &AuthRefs,
    user_id: Option<String>,
    validate_text: Option<String>,
) -> Result<()> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", config_path.display()))?;
    let root = yaml_mapping_mut(&mut doc)?;
    let auth = yaml_child_mapping(root, "auth")?;
    let accounts = yaml_child_sequence(auth, "accounts")?;
    accounts.retain(|v| {
        !matches!(
            v,
            serde_yaml::Value::Mapping(m)
                if m.get(yaml_str("name")).and_then(serde_yaml::Value::as_str) == Some(account)
        )
    });

    let mut acct = serde_yaml::Mapping::new();
    acct.insert(yaml_str("name"), yaml_str(account));
    acct.insert(yaml_str("strategy"), yaml_str(strategy.as_str()));
    insert_yaml_opt(&mut acct, "userId", user_id);
    insert_yaml_opt(&mut acct, "usernameRef", refs.username_ref.clone());
    insert_yaml_opt(&mut acct, "emailRef", refs.email_ref.clone());
    insert_yaml_opt(&mut acct, "phoneRef", refs.phone_ref.clone());
    insert_yaml_opt(&mut acct, "passwordRef", refs.password_ref.clone());
    insert_yaml_opt(&mut acct, "totpRef", refs.totp_ref.clone());
    insert_yaml_opt(&mut acct, "otpRef", refs.otp_ref.clone());
    insert_yaml_opt(&mut acct, "storageRef", refs.storage_ref.clone());
    if let Some(text) = validate_text.filter(|s| !s.trim().is_empty()) {
        let mut validate = serde_yaml::Mapping::new();
        validate.insert(yaml_str("text"), yaml_str(text));
        acct.insert(yaml_str("validate"), serde_yaml::Value::Mapping(validate));
    }
    accounts.push(serde_yaml::Value::Mapping(acct));

    std::fs::write(config_path, serde_yaml::to_string(&doc)?)
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(())
}

#[derive(Default)]
struct AuthRefs {
    username_ref: Option<String>,
    email_ref: Option<String>,
    phone_ref: Option<String>,
    password_ref: Option<String>,
    totp_ref: Option<String>,
    otp_ref: Option<String>,
    storage_ref: Option<String>,
}

fn default_auth_refs(account: &str, strategy: config::AuthStrategy) -> AuthRefs {
    let mut refs = AuthRefs::default();
    match strategy {
        config::AuthStrategy::Password => {
            refs.username_ref = Some(account_ref(account, "username"));
            refs.password_ref = Some(account_ref(account, "password"));
        }
        config::AuthStrategy::PasswordOtp => {
            refs.username_ref = Some(account_ref(account, "username"));
            refs.password_ref = Some(account_ref(account, "password"));
            refs.totp_ref = Some(account_ref(account, "totp"));
        }
        config::AuthStrategy::PhoneOtp => {
            refs.phone_ref = Some(account_ref(account, "phone"));
            refs.otp_ref = Some(account_ref(account, "otp"));
        }
        config::AuthStrategy::EmailLink => {
            refs.email_ref = Some(account_ref(account, "email"));
        }
        config::AuthStrategy::OauthTest
        | config::AuthStrategy::Session
        | config::AuthStrategy::Api => {
            refs.storage_ref = Some(account_ref(account, "session"));
        }
    }
    refs
}

fn journey_cmd(
    config_path: Option<&std::path::Path>,
    action: JourneyAction,
    ctx: &Ctx,
) -> Result<()> {
    let loaded = config::load(config_path)?;
    match action {
        JourneyAction::Run(_) => unreachable!("journey runs are handled asynchronously"),
        JourneyAction::List => {
            let journeys = journey::list(&loaded.root)?;
            if ctx.json {
                ctx.emit(&serde_json::json!({ "journeys": journeys }));
            } else if journeys.is_empty() {
                ctx.say("no journeys yet (author one with `reproit journey create`)");
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
        JourneyAction::Create { name, spec } => {
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
                    "next": format!("reproit journey {name}"),
                }));
            } else {
                ctx.say(format!("  saved {}", rel.display()));
                ctx.say(format!("  run it: reproit journey {name}"));
            }
        }
    }
    Ok(())
}

async fn auth_cmd(config_path: Option<&std::path::Path>, action: AuthAction) -> Result<()> {
    let vpath = resolve_vault_path(config_path)?;
    match action {
        AuthAction::Add {
            account,
            strategy,
            email,
            phone,
            username,
            password,
            otp,
            totp_secret,
            session,
            user_id,
            validate_text,
            no_discover,
        } => {
            if account.trim().is_empty() {
                anyhow::bail!("account name cannot be empty");
            }
            let strategy = strategy.config();
            let config_file = resolve_config_path(config_path)?;
            let mut refs = default_auth_refs(&account, strategy);
            let mut vault = auth::Vault::open(&vpath)?;

            if email.is_some() {
                refs.email_ref = store_secret_opt(&mut vault, &account, "email", email);
                if matches!(
                    strategy,
                    config::AuthStrategy::Password | config::AuthStrategy::PasswordOtp
                ) && refs.username_ref == Some(account_ref(&account, "username"))
                {
                    refs.username_ref = Some(account_ref(&account, "email"));
                }
            }
            if phone.is_some() {
                refs.phone_ref = store_secret_opt(&mut vault, &account, "phone", phone);
            }
            if username.is_some() {
                refs.username_ref = store_secret_opt(&mut vault, &account, "username", username);
            }
            if password.is_some() {
                refs.password_ref = store_secret_opt(&mut vault, &account, "password", password);
            }
            if otp.is_some() {
                refs.otp_ref = store_secret_opt(&mut vault, &account, "otp", otp);
            }
            if let Some(secret) = totp_secret {
                let Some(code) = auth::totp_now(&secret) else {
                    anyhow::bail!("not a valid base32 TOTP secret");
                };
                let key = account_ref(&account, "totp");
                vault.set(&key, &secret);
                refs.totp_ref = Some(key);
                println!("  TOTP ok (current code {code})");
            }
            if session.is_some() {
                refs.storage_ref = store_secret_opt(&mut vault, &account, "session", session);
            }

            vault.save()?;
            update_account_config(
                &config_file,
                &account,
                strategy,
                &refs,
                user_id,
                validate_text,
            )?;
            println!(
                "  account {account} ({}) written to {}",
                strategy.as_str(),
                config_file.display()
            );
            println!("  vault: {}", vpath.display());
            println!("  use it in journeys with: setup: login({account})");
            if matches!(
                strategy,
                config::AuthStrategy::Session
                    | config::AuthStrategy::Api
                    | config::AuthStrategy::OauthTest
            ) {
                println!("  session-style setup can use: setup: auth({account})");
            } else if !no_discover {
                discover_and_verify_login(config_path, &account).await?;
            }
        }
        AuthAction::Discover { account } => {
            discover_and_verify_login(config_path, &account).await?;
        }
        AuthAction::Doctor { account } => {
            auth_account_doctor(config_path, &account)?;
        }
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

/// Map the unauthenticated UI, infer an account-specific login journey from
/// semantic fields/transitions, then prove it in a clean run before presenting
/// it as usable. The generated YAML remains reviewable project state; secrets
/// stay as vault placeholders and never enter the file.
async fn discover_and_verify_login(
    config_path: Option<&std::path::Path>,
    account: &str,
) -> Result<()> {
    let loaded = config::load(config_path)?;
    let freshness = map::map_freshness(&loaded.root)?;
    if !matches!(&freshness, map::MapFreshness::Current) {
        println!("  updating login structure from the current app...");
        rebuild_app_map(
            &loaded,
            "explore",
            Some(30),
            false,
            None,
            matches!(&freshness, map::MapFreshness::Stale(_)),
        )
        .await?;
    }
    let account_cfg = loaded
        .config
        .auth
        .accounts
        .iter()
        .find(|a| a.name == account)
        .ok_or_else(|| anyhow::anyhow!("unknown auth account `{account}`"))?;
    let strategy = account_cfg
        .strategy
        .ok_or_else(|| anyhow::anyhow!("account `{account}` has no auth strategy"))?;
    let validate_text = account_cfg
        .validate
        .as_ref()
        .and_then(|v| v.text.as_deref());
    let appmap = map::load_map(&loaded.root, &loaded.config);
    let spec = journey::discover_login_spec(&appmap, account, strategy, validate_text)?;
    let name = format!("login-{account}");
    let path = journey::save(&loaded.root, &name, &spec)?;
    println!("  generated {}", path.display());
    println!("  verifying login from a clean state...");
    let result = journey::run(&loaded, &name, 1, false).await?;
    if result.outcome != repro::Outcome::Pass {
        anyhow::bail!(
            "discovered login did not verify ({}); generated journey kept for review at {}",
            result.outcome.as_str(),
            path.display()
        );
    }
    println!("  login verified: setup: login({account})");
    Ok(())
}

fn vault_has(vault: &auth::Vault, key: &Option<String>) -> bool {
    key.as_ref().is_some_and(|k| vault.get(k).is_some())
}

fn auth_account_doctor(config_path: Option<&std::path::Path>, account: &str) -> Result<()> {
    let loaded = config::load(config_path)?;
    let vpath = resolve_vault_path(config_path)?;
    let acct = loaded
        .config
        .auth
        .accounts
        .iter()
        .find(|a| a.name == account)
        .ok_or_else(|| anyhow::anyhow!("no account named {account} in reproit.yaml"))?;
    let strategy = acct.strategy.unwrap_or(config::AuthStrategy::Password);
    let vault = auth::Vault::open(&vpath)?;
    let login_journey = loaded.root.join("journeys/login.yaml");

    let mut ok = true;
    let mut check = |name: &str, passed: bool, detail: String| {
        ok &= passed;
        println!(
            "  {:7} {name}: {detail}",
            if passed { "ok" } else { "MISSING" }
        );
    };
    println!("account {account} ({})", strategy.as_str());
    check(
        "vault",
        vpath.exists(),
        if vpath.exists() {
            vpath.display().to_string()
        } else {
            format!("{} does not exist yet", vpath.display())
        },
    );
    match strategy {
        config::AuthStrategy::Password => {
            check(
                "identifier",
                acct.username.is_some()
                    || vault_has(&vault, &acct.username_ref)
                    || vault_has(&vault, &acct.email_ref),
                "usernameRef/emailRef or username".into(),
            );
            check(
                "password",
                vault_has(&vault, &acct.password_ref),
                "passwordRef".into(),
            );
            check(
                "login",
                login_journey.exists(),
                login_journey.display().to_string(),
            );
        }
        config::AuthStrategy::PasswordOtp => {
            check(
                "identifier",
                acct.username.is_some()
                    || vault_has(&vault, &acct.username_ref)
                    || vault_has(&vault, &acct.email_ref),
                "usernameRef/emailRef or username".into(),
            );
            check(
                "password",
                vault_has(&vault, &acct.password_ref),
                "passwordRef".into(),
            );
            let totp_ok = acct
                .totp_ref
                .as_ref()
                .and_then(|k| vault.get(k))
                .and_then(auth::totp_now)
                .is_some();
            check(
                "totp",
                totp_ok || vault_has(&vault, &acct.otp_ref),
                "totpRef or otpRef".into(),
            );
            check(
                "login",
                login_journey.exists(),
                login_journey.display().to_string(),
            );
        }
        config::AuthStrategy::PhoneOtp => {
            check(
                "phone",
                vault_has(&vault, &acct.phone_ref),
                "phoneRef".into(),
            );
            check(
                "otp",
                vault_has(&vault, &acct.otp_ref) || vault_has(&vault, &acct.totp_ref),
                "otpRef, totpRef, or provider adapter".into(),
            );
            check(
                "login",
                login_journey.exists(),
                login_journey.display().to_string(),
            );
        }
        config::AuthStrategy::EmailLink => {
            check(
                "email",
                vault_has(&vault, &acct.email_ref),
                "emailRef".into(),
            );
            check(
                "login",
                login_journey.exists(),
                login_journey.display().to_string(),
            );
        }
        config::AuthStrategy::OauthTest
        | config::AuthStrategy::Session
        | config::AuthStrategy::Api => {
            check(
                "session",
                vault_has(&vault, &acct.storage_ref),
                "storageRef".into(),
            );
        }
    }
    if let Some(validate) = &acct.validate {
        if let Some(text) = &validate.text {
            println!("  ok      validate: text `{text}`");
        } else if let Some(state) = &validate.state {
            println!("  ok      validate: state `{state}`");
        }
    } else {
        println!(
            "  warn    validate: add validate.text or validate.state for clearer auth failures"
        );
    }
    if !ok {
        anyhow::bail!("auth account {account} is not ready");
    }
    Ok(())
}

#[derive(Serialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    required: bool,
    detail: String,
    fix: Option<String>,
}

fn doctor_push(
    checks: &mut Vec<DoctorCheck>,
    name: impl Into<String>,
    ok: bool,
    required: bool,
    detail: impl Into<String>,
    fix: Option<String>,
) {
    checks.push(DoctorCheck {
        name: name.into(),
        ok,
        required,
        detail: detail.into(),
        fix,
    });
}

fn playwright_browser_cache_dirs(runner_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(explicit) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH") {
        candidates.push(PathBuf::from(explicit));
    }
    candidates.push(runner_dir.join("node_modules/.cache/ms-playwright"));
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(PathBuf::from(local).join("ms-playwright"));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join("Library/Caches/ms-playwright"));
        }
    } else if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        candidates.push(PathBuf::from(cache).join("ms-playwright"));
    } else if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".cache/ms-playwright"));
    }
    candidates
}

fn populated_directory(path: &Path) -> bool {
    path.is_dir() && std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

fn render_doctor(checks: &[DoctorCheck]) {
    for c in checks {
        let status = if c.ok {
            "ok"
        } else if c.required {
            "MISSING"
        } else {
            "warn"
        };
        println!("  {status:7} {}", c.name);
        if !c.detail.is_empty() {
            println!("          {}", c.detail);
        }
        if !c.ok {
            if let Some(fix) = &c.fix {
                println!("          fix: {fix}");
            }
        }
    }
}

fn doctor_optional_path(
    checks: &mut Vec<DoctorCheck>,
    name: &str,
    root: &std::path::Path,
    value: Option<&str>,
    fix: &str,
) {
    let Some(value) = value.filter(|s| !s.trim().is_empty()) else {
        doctor_push(
            checks,
            name,
            false,
            false,
            "not configured",
            Some(fix.into()),
        );
        return;
    };
    let path = std::path::Path::new(value);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let exists = resolved.exists() || command_on_path(value);
    doctor_push(
        checks,
        name,
        exists,
        false,
        resolved.display().to_string(),
        Some(fix.into()),
    );
}

async fn doctor(config_path: Option<&std::path::Path>, ctx: &Ctx) -> Result<()> {
    let mut checks = Vec::new();
    let loaded = match config::load(config_path) {
        Ok(loaded) => {
            doctor_push(
                &mut checks,
                "config",
                true,
                true,
                format!("loaded project root {}", loaded.root.display()),
                None,
            );
            Some(loaded)
        }
        Err(e) => {
            doctor_push(
                &mut checks,
                "config",
                false,
                true,
                e.to_string(),
                Some("run from a project with reproit.yaml, pass --config, or start with `reproit init`".into()),
            );
            None
        }
    };

    let web = loaded
        .as_ref()
        .map(|l| l.config.app.platform == "web")
        .unwrap_or(false);

    if let Some(l) = &loaded {
        let platform = platform::resolve(&l.config.app.platform);
        if let Some(p) = platform {
            doctor_push(
                &mut checks,
                "platform",
                true,
                true,
                format!("{} via {}", p.id, p.backend.as_str()),
                None,
            );
            let native_causal = matches!(
                l.config.app.platform.as_str(),
                "web" | "electron" | "flutter" | "swift-ios"
            );
            doctor_push(
                &mut checks,
                "causal replay",
                native_causal,
                false,
                if native_causal {
                    String::from("automatic HTTP capture + fail-closed replay is wired for this framework")
                } else {
                    String::from("UI reproduction works; network-dependent confirmation requires the Reproit SDK transport hook")
                },
                (!native_causal).then(|| {
                    "enable the framework SDK causal transport; otherwise network-dependent observations remain candidates".into()
                }),
            );
            if let Some(required) = p.backend.required_os() {
                let host = std::env::consts::OS;
                doctor_push(
                    &mut checks,
                    "host os",
                    host == required,
                    false,
                    format!("host={host}, required={required}"),
                    Some(format!(
                        "run this project on {required} or in a matching VM"
                    )),
                );
            }
        }

        if web {
            let node_ok = exec::which("node").await;
            doctor_push(
                &mut checks,
                "node",
                node_ok,
                true,
                "required for the Playwright web runner",
                Some("install Node.js 18+ (`brew install node` on macOS)".into()),
            );
            match &l.config.app.url {
                Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
                    doctor_push(&mut checks, "app url", true, true, url.clone(), None);
                }
                Some(url) => {
                    doctor_push(
                        &mut checks,
                        "app url",
                        false,
                        true,
                        format!("configured value is `{url}`"),
                        Some("set app.url to an http(s) URL reachable from this machine".into()),
                    );
                }
                None => {
                    doctor_push(
                        &mut checks,
                        "app url",
                        false,
                        true,
                        "app.url is missing",
                        Some(
                            "add app.url to reproit.yaml or run with --url where supported".into(),
                        ),
                    );
                }
            }

            match &l.config.app.web_runner_dir {
                Some(dir) => {
                    let runner_dir = l.root.join(dir);
                    let runner = runner_dir.join("runner.mjs");
                    let node_modules = runner_dir.join("node_modules");
                    let playwright = node_modules.join("playwright");
                    let browser_candidates = playwright_browser_cache_dirs(&runner_dir);
                    let browser = browser_candidates
                        .iter()
                        .find(|path| populated_directory(path));
                    doctor_push(
                        &mut checks,
                        "web runner",
                        runner.exists(),
                        true,
                        runner_dir.display().to_string(),
                        Some("set app.webRunnerDir to reproit-cli/runners/web or install the packaged runner".into()),
                    );
                    doctor_push(
                        &mut checks,
                        "playwright package",
                        playwright.exists(),
                        true,
                        node_modules.display().to_string(),
                        Some("run `npm ci` in the web runner directory".into()),
                    );
                    doctor_push(
                        &mut checks,
                        "playwright browser",
                        browser.is_some(),
                        false,
                        browser
                            .unwrap_or(&browser_candidates[0])
                            .display()
                            .to_string(),
                        Some(
                            "run `npx playwright install chromium` in the web runner directory"
                                .into(),
                        ),
                    );
                }
                None => {
                    doctor_push(
                        &mut checks,
                        "web runner",
                        false,
                        false,
                        "app.webRunnerDir is not set; reproit will try its self-provisioned runner",
                        Some("set REPROIT_WEB_RUNNER_DIR for source checkouts if auto-provisioning fails".into()),
                    );
                }
            }
        } else if let Some(p) = platform {
            match p.backend {
                platform::Backend::FlutterDrive => {
                    for (bin, why) in [
                        ("xcrun", "simulator control"),
                        ("ffmpeg", "video/evidence tooling"),
                        ("flutter", "Flutter app driving"),
                    ] {
                        let found = exec::which(bin).await;
                        doctor_push(
                            &mut checks,
                            bin,
                            found,
                            true,
                            why,
                            Some(format!("install {bin} for Flutter/iOS simulator runs")),
                        );
                    }
                    let sims = exec::run("xcrun", &["simctl", "list", "devices", "booted"]).await;
                    doctor_push(
                        &mut checks,
                        "simctl reachable",
                        sims.ok(),
                        true,
                        "can query booted simulators",
                        Some("install Xcode command line tools and accept Xcode licenses".into()),
                    );
                }
                platform::Backend::Appium => {
                    doctor_push(
                        &mut checks,
                        "appium url",
                        l.config.app.appium_url.is_some(),
                        true,
                        l.config
                            .app
                            .appium_url
                            .clone()
                            .unwrap_or_else(|| "missing".into()),
                        Some("set app.appiumUrl, usually http://127.0.0.1:4723".into()),
                    );
                    doctor_push(
                        &mut checks,
                        "appium caps",
                        l.config.app.appium_caps.is_some(),
                        true,
                        "desired capabilities present",
                        Some("set app.appiumCaps for the app or bundle under test".into()),
                    );
                }
                platform::Backend::WebCdp => {
                    let node_ok = exec::which("node").await;
                    doctor_push(
                        &mut checks,
                        "node",
                        node_ok,
                        true,
                        "required for Electron/Tauri/webview runners",
                        Some("install Node.js 18+ (`brew install node` on macOS)".into()),
                    );
                    doctor_optional_path(
                        &mut checks,
                        "executable",
                        &l.root,
                        l.config.app.executable.as_deref(),
                        "set app.executable to the built Electron/Tauri app",
                    );
                    doctor_optional_path(
                        &mut checks,
                        "runner dir",
                        &l.root,
                        l.config.app.runner_dir.as_deref(),
                        "set app.runnerDir to the directory containing reproit runners",
                    );
                }
                platform::Backend::DesktopAx
                | platform::Backend::DesktopUia
                | platform::Backend::DesktopAtspi
                | platform::Backend::Instrumented
                | platform::Backend::Tui => {
                    let target = l.config.app.executable.as_deref().or_else(|| {
                        (!l.config.app.bundle_id.trim().is_empty())
                            .then_some(l.config.app.bundle_id.as_str())
                    });
                    doctor_optional_path(
                        &mut checks,
                        "executable",
                        &l.root,
                        target,
                        "set app.executable (or bundleId on macOS) to the app under test",
                    );
                    doctor_optional_path(
                        &mut checks,
                        "runner dir",
                        &l.root,
                        l.config.app.runner_dir.as_deref(),
                        "set app.runnerDir to the directory containing reproit runners",
                    );
                }
            }
        }
    }

    let persisted = crosscut::load_token(&crosscut::token_path());
    let cloud_url = std::env::var("REPROIT_CLOUD_URL")
        .ok()
        .or_else(|| persisted.as_ref().and_then(|(_, u)| u.clone()));
    let cloud_key = std::env::var("REPROIT_CLOUD_KEY")
        .ok()
        .or_else(|| persisted.as_ref().map(|(t, _)| t.clone()));
    doctor_push(
        &mut checks,
        "cloud url",
        cloud_url.is_some(),
        false,
        cloud_url.unwrap_or_else(|| "not configured".into()),
        Some("set REPROIT_CLOUD_URL or run `reproit login --key <sk_live_...>`".into()),
    );
    doctor_push(
        &mut checks,
        "cloud key",
        cloud_key.is_some(),
        false,
        cloud_key
            .as_ref()
            .map(|k| format!("configured ({} chars), not printed", k.len()))
            .unwrap_or_else(|| "not configured".into()),
        Some("set REPROIT_CLOUD_KEY or run `reproit login --key <sk_live_...>`".into()),
    );
    if let Some(l) = &loaded {
        let app_id = std::env::var("REPROIT_CLOUD_APP").ok();
        doctor_push(
            &mut checks,
            "cloud app",
            app_id.is_some(),
            false,
            app_id.unwrap_or_else(|| {
                format!("not set; local app platform is {}", l.config.app.platform)
            }),
            Some("set REPROIT_CLOUD_APP or pass --app to cloud commands".into()),
        );
    }

    match &loaded {
        Some(loaded) => match llm::from_spec(&loaded.config.llm.to_spec()) {
            Ok(b) => match b.check().await {
                Ok(()) => doctor_push(&mut checks, "llm", true, false, b.name(), None),
                Err(e) => doctor_push(
                    &mut checks,
                    "llm",
                    false,
                    false,
                    format!("{}: {e}; runner works, authoring will not", b.name()),
                    Some("install/configure the selected LLM provider or leave this for runner-only use".into()),
                ),
            },
            Err(e) => doctor_push(
                &mut checks,
                "llm",
                false,
                false,
                e.to_string(),
                Some("fix the llm section in reproit.yaml if you use authoring/analyze commands".into()),
            ),
        },
        None => doctor_push(
            &mut checks,
            "llm",
            false,
            false,
            "no reproit.yaml found, skipping",
            None,
        ),
    }

    let ok = checks.iter().all(|c| c.ok || !c.required);
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "doctor",
            "ok": ok,
            "checks": checks,
        }));
    } else {
        render_doctor(&checks);
        println!(
            "\n{}",
            if ok {
                "doctor: required checks passed"
            } else {
                "doctor: required checks failed"
            }
        );
    }
    if !ok {
        std::process::exit(1);
    }
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
    fn simplify_preserves_the_property_matched_fixture() {
        // A data-dependent repro carries inputs + locale. Simplifying it minimizes
        // the ACTIONS but must keep the data, or the adopted repro stops
        // reproducing the bug that only fires for that fixture.
        let src = serde_json::json!({
            "seed": 7u64,
            "replay": ["tap:a", "tap:b", "tap:c"],
            "inputs": [{ "field": "name", "value": "a-long-unicode-name" }],
            "locale": "tr",
        });
        let out = build_simplified_replay(7, &["tap:c".to_string()], &src);
        assert_eq!(out["replay"], serde_json::json!(["tap:c"]));
        assert_eq!(out["locale"], "tr");
        assert_eq!(out["inputs"], src["inputs"]);

        // A path-only repro (no fixture) stays the bare {seed, replay} shape.
        let bare = build_simplified_replay(
            7,
            &["tap:c".to_string()],
            &serde_json::json!({ "seed": 7u64, "replay": ["tap:a", "tap:c"] }),
        );
        assert!(bare.get("inputs").is_none());
        assert!(bare.get("locale").is_none());
        assert_eq!(bare["replay"], serde_json::json!(["tap:c"]));
    }

    #[test]
    fn target_as_executable_detects_path_and_on_path_commands() {
        // A bare command on PATH (`sh` is always present) is an executable target,
        // with its args preserved.
        assert_eq!(target_as_executable("sh").as_deref(), Some("sh"));
        assert_eq!(
            target_as_executable("sh -c true").as_deref(),
            Some("sh -c true")
        );
        // An absolute path to an existing executable.
        if std::path::Path::new("/bin/sh").exists() {
            assert_eq!(target_as_executable("/bin/sh").as_deref(), Some("/bin/sh"));
        }
        // A non-existent path or a bare token not on PATH is NOT an executable, so
        // it falls through to alias/journey resolution.
        assert_eq!(target_as_executable("/no/such/binary-xyzzy"), None);
        assert_eq!(target_as_executable("checkout-flow-screen-xyzzy"), None);
        assert_eq!(target_as_executable("my-saved-alias-qqq"), None);
        assert_eq!(target_as_executable(""), None);
    }

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
        // A dotted alias whose last label is numeric is NOT a host (no such TLD),
        // so it stays an alias instead of being misread as a deployed app.
        assert_eq!(target_as_url("checkout.2"), None);
        assert_eq!(target_as_url("step.3"), None);
        // ...but a real IPv4 (all-numeric labels) is still a URL.
        assert_eq!(
            target_as_url("10.0.0.5").as_deref(),
            Some("https://10.0.0.5")
        );
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

## confirmed repro (2 actions, shrunk from 7)

```
tap:Login
tap:Submit
```

Replay: write {\"replay\": [...]} to .reproit/tmp/fuzz_config.json ...
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
            id: "abcdef123456".into(),
            seed: 42,
            actions: vec!["tap:Login".into(), "tap:Submit".into()],
            run_dir: std::path::PathBuf::from("/tmp/nonexistent-run"),
        };
        let m = f.pending_meta();
        assert_eq!(m.id, "abcdef123456");
        assert_eq!(m.id, f.id());
        assert_eq!(m.status, repro::Status::Quarantined);
        assert_eq!(m.seed, 42);
        assert!(m.alias.is_none());
        assert!(m.created.is_empty());
        assert!(m.last_checked.is_none());
        assert_eq!(m.trigger_index, Some(2));
    }

    #[test]
    fn public_and_internal_finding_ids_resolve_to_pending_artifact() {
        let root = std::env::temp_dir().join(format!("reproit-fnd-{}", std::process::id()));
        let run = root.join(".reproit/runs/run-1");
        std::fs::create_dir_all(&run).unwrap();
        let md = "\
# fuzz finding (seed 42)

## confirmed repro (2 actions)

```
tap:Login
tap:Submit
```
";
        std::fs::write(run.join("fuzz.md"), md).unwrap();
        let loaded = config::parse_str(
            "app:\n  platform: web\n  webRunnerDir: ./runners/web\n  url: http://localhost:3000\n\
             devices:\n  namePrefix: reproit\n\
             journeys:\n  dir: journeys\n  driver: explore\n  doneMarkers: [DONE]\n\
             evidence:\n  outDir: .reproit/runs\n  video: false\n",
            root.clone(),
        )
        .unwrap();
        let raw = repro::repro_id(42, &["tap:Login", "tap:Submit"]);
        assert!(find_finding_by_id(&loaded, &raw).is_some());
        assert!(find_finding_by_id(&loaded, &repro::display_finding_id(&raw)).is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finding_id_resolves_from_durable_store_after_evidence_moves() {
        let root = std::env::temp_dir().join(format!("reproit-durable-fnd-{}", std::process::id()));
        let raw = repro::repro_id(77, &["tap:key:save"]);
        let durable = root.join(".reproit/findings").join(&raw);
        std::fs::create_dir_all(&durable).unwrap();
        std::fs::write(
            durable.join("fuzz.md"),
            "# fuzz finding (seed 77)\n\n## confirmed repro (1 actions)\n\n```\ntap:key:save\n```\n",
        )
        .unwrap();
        let loaded = config::parse_str(
            "app:\n  platform: web\n  webRunnerDir: ./runners/web\n  url: http://localhost:3000\n\
             devices:\n  namePrefix: reproit\n\
             journeys:\n  dir: journeys\n  driver: explore\n  doneMarkers: [DONE]\n\
             evidence:\n  outDir: moved/evidence\n  video: false\n",
            root.clone(),
        )
        .unwrap();
        let found = find_finding_by_id(&loaded, &repro::display_finding_id(&raw)).unwrap();
        assert_eq!(found.id(), raw);
        assert_eq!(found.run_dir, durable);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parse_fuzz_oracle_reads_occlusion_block() {
        // The `## oracle` block carries the category and violating state sig.
        let md = "\
# fuzz finding (seed 9)

## invariants violated

- **no-occluded-control** (1)

## oracle

- oracle: `occlusion`
- invariant: `no-occluded-control`
- sig: `advanced`

## findings

- `no-occluded-control` **OCCLUSION**: state advanced has an occluded control

## confirmed repro (1 actions)

```
tap:Advanced
```
";
        let (oracle, sig) = parse_fuzz_oracle(md);
        assert_eq!(oracle.as_deref(), Some("occlusion"));
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
    fn state_present_recording_navigates_directly_without_replay() {
        let log = r#"EXPLORE:STATE {"sig":"docs","route":"/docs/search","labels":[]}"#;
        let (url, action) = web_record_metadata(
            Some("https://example.test/start"),
            Some("zoom-reflow"),
            Some("docs"),
            log,
        );
        assert_eq!(url.as_deref(), Some("https://example.test/docs/search"));
        assert_eq!(action, None);
    }

    #[test]
    fn flicker_recording_keeps_only_the_triggering_action() {
        let log = concat!(
            "EXPLORE:STATE {\"sig\":\"header\",\"route\":\"/pricing\",\"labels\":[]}\n",
            "EXPLORE:RERENDER {\"from\":\"header\",\"action\":\"tap:key:menu\",\"churned\":[\"nav\"]}\n"
        );
        let (url, action) = web_record_metadata(
            Some("https://example.test/"),
            Some("flicker"),
            Some("header"),
            log,
        );
        assert_eq!(url.as_deref(), Some("https://example.test/pricing"));
        assert_eq!(action.as_deref(), Some("tap:key:menu"));
    }

    #[test]
    fn legacy_recording_preserves_full_replay() {
        let meta: repro::Meta = serde_json::from_value(serde_json::json!({
            "id": "abc", "status": "quarantined", "seed": 1,
            "created": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        let mut replay = serde_json::json!({"seed": 1, "replay": ["tap:A", "tap:B"]});
        minimize_record_replay(&mut replay, &meta);
        assert_eq!(replay["replay"], serde_json::json!(["tap:A", "tap:B"]));
        assert!(replay.get("gotoUrl").is_none());
    }

    #[test]
    fn direct_recording_replaces_discovery_walk() {
        let mut meta: repro::Meta = serde_json::from_value(serde_json::json!({
            "id": "abc", "status": "quarantined", "seed": 1,
            "created": "2026-01-01T00:00:00Z",
            "record_url": "https://example.test/pricing",
            "record_action": "tap:key:menu"
        }))
        .unwrap();
        let mut replay = serde_json::json!({"seed": 1, "replay": ["tap:A", "tap:B"]});
        minimize_record_replay(&mut replay, &meta);
        assert_eq!(replay["replay"], serde_json::json!(["tap:key:menu"]));
        assert_eq!(replay["gotoUrl"], "https://example.test/pricing");
        meta.record_action = None;
        minimize_record_replay(&mut replay, &meta);
        assert_eq!(replay["replay"], serde_json::json!([]));
    }

    #[test]
    fn parse_fuzz_report_handles_empty_repro_block() {
        let md = "# fuzz finding (seed 5)\n\n## confirmed repro (0 actions)\n\n```\n```\n";
        let (seed, actions) = parse_fuzz_report(md).expect("parse");
        assert_eq!(seed, 5);
        assert!(actions.is_empty());
    }

    #[test]
    fn parse_fuzz_finding_id_accepts_scoped_marker_and_rejects_invalid_ids() {
        assert_eq!(
            parse_fuzz_finding_id("# fuzz finding (seed 0)\n\n<!-- finding-id: abcdef123456 -->"),
            Some("abcdef123456".to_string())
        );
        assert_eq!(
            parse_fuzz_finding_id("<!-- finding-id: not-an-id -->"),
            None
        );
        assert_eq!(parse_fuzz_finding_id("# legacy fuzz report"), None);
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
    fn only_flutter_sim_runs_offer_the_device_picker() {
        // Only FlutterDrive provisions a sim reproit picks, and only with --sim
        // (its default is the headless flutter test tier).
        assert!(run_needs_device_pick("flutter", true));
        assert!(!run_needs_device_pick("flutter", false));
        // Every other backend brings its own target (Appium caps, a browser, the
        // host, a PTY), so no reproit picker, even with --sim.
        for p in [
            "web",
            "react-native",
            "swift-ios",
            "android",
            "winui",
            "electron",
            "tauri",
        ] {
            assert!(!run_needs_device_pick(p, false), "{p} should not prompt");
            assert!(
                !run_needs_device_pick(p, true),
                "{p} should not prompt even with --sim"
            );
        }
        // Unknown platform: no prompt.
        assert!(!run_needs_device_pick("cobol-tui", false));
    }

    #[test]
    fn direct_bug_ids_expand_to_their_existing_execution_paths() {
        let expand = |args: &[&str]| {
            expand_direct_bug_arg(args.iter().map(std::ffi::OsString::from).collect())
                .into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            expand(&["reproit", "bkt_deadbeef0001"]),
            ["reproit", "__replay-bucket", "bkt_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "fnd_deadbeef0001"]),
            ["reproit", "check", "--repro-id", "fnd_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "rep_deadbeef0001"]),
            ["reproit", "check", "--repro-id", "rep_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "--json", "bkt_deadbeef0001"]),
            ["reproit", "--json", "__replay-bucket", "bkt_deadbeef0001"]
        );
        assert_eq!(expand(&["reproit", "scan"]), ["reproit", "scan"]);
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
