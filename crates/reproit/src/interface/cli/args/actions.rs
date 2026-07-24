use super::SkillFormat;
use crate::adapters::config;
use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Subcommand)]
pub(crate) enum DebugAction {
    /// Inspect or force maintenance of reproit's internal app model.
    Map {
        #[command(subcommand)]
        action: Option<MapAction>,
    },
    /// Re-evaluate a production backend capture payload (the
    /// `context.reproitCapture` object on `/v1/errors/:app`) and report
    /// whether the captured violation still reproduces deterministically.
    ReplayCapture {
        /// Path to the capture JSON file.
        file: PathBuf,
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
        format: SkillFormat,
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
    /// Print the bounded observed state machine and its still-unknown actions.
    /// The model is explicitly incomplete and cannot produce a finding.
    Model,
    /// Estimate useful exploration work from current state/transition coverage.
    Budget {
        /// Normal campaign action budget used as the recommendation baseline.
        #[arg(long, default_value_t = 100)]
        base: u32,
    },
    /// Propose draft temporal contracts from verified reversible transitions.
    /// Drafts are non-authoritative until an application owner accepts them.
    SuggestContracts,
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
        /// Local name (alias) for the saved repro, run with `reproit @name`.
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
    /// @name` runs the standard local, network-free verification and
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
        /// Local name (alias) for the saved repro, run with `reproit @name`.
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
