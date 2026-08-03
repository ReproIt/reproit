//! Clap schema for the Reproit command-line interface.

use super::context::Ctx;
use super::rewrite;
use crate::VERSION;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod actions;

pub(crate) use actions::*;

/// The listed commands are the complete public product loop.
const AFTER_HELP: &str = concat!(
    "Known failure:\n",
    "  reproit occ_<id>      reproduce a production occurrence locally\n",
    "  reproit capture       preserve a UI or command failure\n",
    "\nCapture or discover failures:\n",
    "  reproit init          configure the current application\n",
    "  reproit find          run staged surface and deep discovery\n",
    "\nProve and retain:\n",
    "  reproit <id>          reproduce one exact failure\n",
    "  reproit keep <id>     preserve it as a regression guard\n",
    "  reproit check         prove saved failures remain fixed\n",
    "  reproit list          show local guards\n",
    "\nUtilities: doctor and login.",
);

#[derive(Parser)]
#[command(
    name = "reproit",
    version = VERSION,
    about = "Make software failures executable, prove fixes, and keep them from returning",
    after_help = AFTER_HELP
)]
pub(crate) struct Cli {
    /// Path to reproit.yaml (default: search cwd and ancestors)
    #[arg(long, global = true)]
    pub(crate) config: Option<PathBuf>,
    /// Machine-readable output (CI, scripts, the MCP bridge)
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Suppress human-readable output
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

/// One outcome-oriented discovery command over the existing scan and fuzz
/// engines. The explicit modes keep work bounded while the default exercises
/// both layers.
#[derive(Args)]
pub(crate) struct FindArgs {
    /// URL, schema, terminal executable, journey, or configured target.
    #[arg(value_name = "TARGET")]
    pub(crate) target: Option<String>,
    /// Run only the fast surface pass.
    #[arg(long, conflicts_with_all = ["deep", "exhaustive"])]
    pub(crate) quick: bool,
    /// Run only deep interaction exploration.
    #[arg(long, conflicts_with_all = ["quick", "exhaustive"])]
    pub(crate) deep: bool,
    /// Run the staged pass with the larger bounded campaign budget.
    #[arg(long, conflicts_with_all = ["quick", "deep"])]
    pub(crate) exhaustive: bool,
    /// Override the number of deep exploration seeds.
    #[arg(long)]
    pub(crate) runs: Option<u32>,
    /// Override the per-pass action budget.
    #[arg(long)]
    pub(crate) budget: Option<u32>,
    /// Disposable backend service URL.
    #[arg(long, hide = true)]
    pub(crate) service: Option<String>,
    /// Force URL routing through `web` or `backend`.
    #[arg(long, hide = true)]
    pub(crate) platform: Option<String>,
    /// Same-origin reset endpoint for exact backend replay.
    #[arg(long, hide = true)]
    pub(crate) reset: Option<String>,
    /// Record surface findings as short clips.
    #[arg(long, hide = true)]
    pub(crate) record_video: bool,
    /// Extra browser header, repeatable as `"Name: value"`.
    #[arg(long = "header", value_name = "NAME: VALUE", hide = true)]
    pub(crate) headers: Vec<String>,
    /// Restrict deep exploration to these detector categories (the default is
    /// the stable set).
    #[arg(long)]
    pub(crate) only: Option<String>,
    /// Exclude these detector categories after applying --only.
    #[arg(long = "no")]
    pub(crate) no_oracles: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ListState {
    Guards,
    Candidates,
    Bugs,
}

// A clap subcommand enum: variants carry their flags by value and are
// instantiated once at startup, so the size spread between variants is
// irrelevant (and unavoidable for a rich CLI).
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Detect the current app and create the smallest working reproit setup.
    /// After initialization, use `reproit find`.
    Init {
        /// Running web app to initialize. A URL always selects the web UI
        /// workflow.
        #[arg(value_name = "URL")]
        target: Option<String>,
        /// Platform override: flutter | web | rn | android | backend.
        #[arg(long)]
        platform: Option<String>,
        /// Running service base URL. When a draft is derived from source,
        /// reproit sends one bounded GET per parameterless GET route and
        /// records the observed response.
        #[arg(long = "target", value_name = "SERVICE_URL")]
        learn_target: Option<String>,
        /// Replace existing generated scaffold files.
        #[arg(long)]
        force: bool,
    },
    /// Find unknown failures through a fast surface pass followed by bounded
    /// deep exploration and exact confirmation.
    Find(FindArgs),
    /// Run the saved regression suite and classify each: pass (0) / fail (1) /
    /// flaky (2) / stale (3). To reproduce one bug, run `reproit <id>` or
    /// `reproit @saved-name`.
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
        /// Optional sub-variant, passed as --dart-define=PROMPT_KIND=<kind>
        #[arg(long, hide = true)]
        kind: Option<String>,
        /// Write JUnit XML results to this path (for CI)
        #[arg(long)]
        junit: Option<PathBuf>,
        /// Gate several services in one command: repeat for each service's
        /// reproit.yaml. Exits non-zero if ANY of them fails, so a repo with
        /// more than one service needs one CI step instead of N chained ones.
        #[arg(long, value_name = "CONFIG")]
        service: Vec<PathBuf>,
        /// Treat a quarantined (reported, non-blocking) repro's failure as
        /// blocking too, so it gates the exit code like a required repro.
        #[arg(long)]
        strict: bool,
        /// Contract override for config-less suite gates (the cloud repo's
        /// guard corpus has no reproit.yaml to hold gate.runs). Projects use
        /// the `gate:` config section instead.
        #[arg(long, hide = true)]
        runs: Option<u32>,
        /// Headless reproduction: report the verdict and exit without holding
        /// the replayed app for inspection. This is automatic for CI, agents,
        /// and scripts (non-TTY, --json, --yes); the flag forces it on a TTY.
        #[arg(long)]
        auto: bool,
        /// Hermetic re-execution for a capture file: boot this command with
        /// REPROIT_REPLAY pointed at the capture, fire the recorded request,
        /// and verdict from the live response (reproduced / fixed / diverged /
        /// inconclusive). The app must mount the reproit SDK with
        /// instrument.install() and listen on $PORT.
        #[arg(long, value_name = "COMMAND")]
        exec: Option<String>,
        /// Device target: ios|android|web|all. Interactive picker when omitted
        /// on a TTY and not --yes.
        #[arg(long, hide = true)]
        target: Option<String>,
        /// Save screen video as supporting evidence for each executed repro.
        #[arg(long, hide = true)]
        record_video: bool,
        /// Scan recorded video for transient render glitches.
        #[arg(long, requires = "record_video", hide = true)]
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
        /// Backend CI gate: record the current findings as the accepted baseline
        /// and exit 0, so later `check` runs block only on new or regressed
        /// findings.
        #[arg(long)]
        update_baseline: bool,
    },
    /// Keep a finding or occurrence in the committed regression suite. The
    /// store dir is the repro's CONTENT HASH (.reproit/repros/<id>/), stable
    /// across machines and self-deduping. `--as` assigns a human alias.
    Keep {
        /// Finding id from the latest fuzz run, or a local/Cloud occurrence id.
        /// Uses the sole finding if omitted, else lists choices.
        id: Option<String>,
        /// Optional human label for the kept repro.
        #[arg(long = "as", name = "name")]
        as_name: Option<String>,
        /// Land the repro `required` (blocking) immediately instead of
        /// quarantined-until-first-green.
        #[arg(long)]
        strict: bool,
        /// For a capture file: the boot command for hermetic re-execution
        /// (stored as the guard's hermetic.json exec recipe). The app must
        /// mount the reproit SDK with instrument.install() and honor $PORT.
        /// Defaults to backend.exec from reproit.yaml when set.
        #[arg(long, value_name = "COMMAND")]
        exec: Option<String>,
        /// Re-record a DRIFTED hermetic guard against the current code: boots
        /// the guard's own recipe, fires its recorded trigger, and prints the
        /// old-versus-new exchange diff. Nothing is rewritten without --yes,
        /// and the inbound trigger and oracle are always preserved.
        #[arg(long)]
        refresh: bool,
    },
    /// Diagnose local setup: config, runner deps, app URL, and cloud
    /// credentials.
    Doctor,
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
    /// The implementation surface, flattened in as top-level commands. Only
    /// `capture` and `list` are visible; everything else carries `hide = true`
    /// on its own variant. There is no `internal` word to type: a command is
    /// either part of the vocabulary or it is unlisted, and nothing in between
    /// needs a name.
    #[command(flatten)]
    Internal(crate::interface::cli::internal::InternalCmd),
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
