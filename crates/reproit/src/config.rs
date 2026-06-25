//! Config schema and loader. See examples/reproit.example.yaml for the shape.

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub app: App,
    pub devices: Devices,
    #[serde(default)]
    pub reset: Reset,
    pub journeys: Journeys,
    #[serde(default)]
    pub evidence: Evidence,
    pub visual: Option<Visual>,
    /// Store/marketing screenshot tours (see modes/screenshots.rs). Optional;
    /// only needed when running `reproit screenshots`.
    #[serde(default)]
    pub screenshots: Option<Screenshots>,
    #[serde(default)]
    pub gate: Gate,
    #[serde(default)]
    pub llm: LlmCfg,
    #[serde(default)]
    pub auth: AuthCfg,
    /// Named invariants/properties checked over a run's state graph (see
    /// model/invariants.rs). Built-ins are on by default; custom ones are
    /// declared here.
    #[serde(default)]
    pub invariants: InvariantsCfg,
}

/// Login credentials for journeys, resolved at run time from the encrypted
/// vault and injected as env (never stored in config or repo). See auth.rs.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthCfg {
    /// Encrypted vault path, relative to the config file.
    /// Default: .reproit/secrets.vault
    pub vault: Option<String>,
    #[serde(default)]
    pub accounts: Vec<Account>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Account {
    /// Account handle, e.g. "alice" or "admin"; becomes the env namespace
    /// REPROIT_SECRET_<NAME>_*.
    pub name: String,
    /// Non-secret backend user id for this account. Lets reset steps clear this
    /// account's data by reference (`${account.<name>.userId}`) instead of a
    /// hardcoded UUID, so reset stays in sync with the accounts a scenario uses.
    pub user_id: Option<String>,
    /// Non-secret username/email. Use ${ENV} interpolation or put it here.
    pub username: Option<String>,
    /// Vault key holding this account's password.
    pub password_ref: Option<String>,
    /// Vault key holding a base32 TOTP secret (2FA / one-time codes).
    pub totp_ref: Option<String>,
    /// Vault key holding a JSON session blob for the `auth(<account>)` login
    /// bypass: a map the runner restores (e.g. localStorage entries) so the app
    /// boots authenticated without driving the login UI.
    pub storage_ref: Option<String>,
}

/// Which LLM powers the authoring agent / failure analyzer. Hot-swappable:
/// CLI agents bill against subscriptions (dev), APIs bill per token (prod).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmCfg {
    /// codex-cli (default) | claude-cli | claude-api
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Override the CLI binary path.
    pub bin: Option<String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl LlmCfg {
    pub fn to_spec(&self) -> llm::Spec {
        llm::Spec {
            provider: self.provider.clone(),
            model: self.model.clone(),
            bin: self.bin.clone(),
            extra_args: self.extra_args.clone(),
        }
    }
}

/// The INVARIANTS oracle config. Built-ins (no-exception, no-jank, all-labeled,
/// no-dead-end, no-leak) ship ON by default; flip any off here. Custom
/// invariants are declared under `custom`. See model/invariants.rs.
///
/// reproit.yaml shape:
/// ```yaml
/// invariants:
///   noException: true        # edge: any uncaught app exception (default on)
///   noJank: true             # state: per-state frame budget (SIM ONLY)
///   jankPctMax: 25.0         # the budget no-jank / custom jank checks use
///   allLabeled: true         # state: every tappable must have a semantics label
///   noDeadEnd: true          # graph: no non-terminal sink node
///   noLeak: true             # graph: leaked-resource signal (sim-authoritative)
///   terminalStates: [order_confirmed, advanced]  # intended end screens, exempt
///   custom:
///     - id: settings-has-save
///       scope: state
///       labelsMatch: "(?i)save"          # every state must have a matching label
///     - id: no-raw-error-text
///       scope: state
///       labelsAbsent: "(?i)null|exception"  # no state may show a matching label
///     - id: feed-tidy
///       scope: state
///       maxUnlabeled: 0                  # unlabeled tappables <= N
///     - id: no-delete-reachable
///       scope: edge
///       actionAbsent: "tap:Delete"       # no edge may take a matching action
///     - id: checkout-reachable
///       scope: graph
///       mustReach: "(?i)checkout"        # some state must expose a matching label
/// ```
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvariantsCfg {
    #[serde(default = "default_true")]
    pub no_exception: bool,
    #[serde(default = "default_true")]
    pub no_jank: bool,
    #[serde(default = "default_jank_pct_max")]
    pub jank_pct_max: f64,
    #[serde(default = "default_true")]
    pub all_labeled: bool,
    #[serde(default = "default_true")]
    pub no_dead_end: bool,
    #[serde(default = "default_true")]
    pub no_leak: bool,
    /// Per-transition re-render flicker: a transition that tears down and
    /// rebuilds persistent chrome which did not change (web runner only).
    #[serde(default = "default_true")]
    pub rerender_flicker: bool,
    /// DOM/layout overflow: content clipped or overflowing its container/viewport
    /// (the i18n/long-string/RTL failure class). Deterministic structural
    /// measurement from the web runner (`EXPLORE:OVERFLOW`).
    #[serde(default = "default_true")]
    pub no_overflow: bool,
    /// Minimum overflow magnitude (px) for a `no-overflow` finding to count. The
    /// web runner already drops sub-pixel noise (OVERFLOW_TOL); this is a USER
    /// floor on top of it, so a team can ignore minor clips/spills and report only
    /// overflows they care about (set higher for "dramatic only"). Default 2 keeps
    /// every measured overflow, matching the prior behavior.
    #[serde(default = "default_overflow_min_px")]
    pub overflow_min_px: i64,
    /// Broken rendered content: a label showing a stringify/template artifact
    /// (`[object Object]`, a bare `undefined`/`null`/`NaN`, or an unrendered
    /// `{{...}}`/`${...}` placeholder). Deterministic DOM/label scan from the web
    /// runner (`EXPLORE:CONTENTBUG`).
    #[serde(default = "default_true")]
    pub no_broken_render: bool,
    /// Main-thread freeze (no-progress): an action whose synchronous handler
    /// blocked the main thread past the hang floor (the app stopped making
    /// progress). Deterministic, keyed off the browser's Long Tasks trace from the
    /// web runner (`EXPLORE:HANG`).
    #[serde(default = "default_true")]
    pub no_hang: bool,
    /// Component-choice anomaly: a multi-choice component (language tabs, a radio
    /// group) where one option behaves differently from its siblings, shifting the
    /// global layout. Differential (outlier vs siblings), not an absolute
    /// threshold. From the web runner's `EXPLORE:CHOICEBUG`; Chromium-tier.
    #[serde(default = "default_true")]
    pub no_choice_anomaly: bool,
    /// Broken route: the app links to a URL whose document responds 4xx/5xx (a
    /// dead route / 404). Keyed off the navigation HTTP status from the web runner
    /// (`EXPLORE:BROKENROUTE`); structural and false-positive-free. Web only.
    #[serde(default = "default_true")]
    pub no_broken_route: bool,
    /// State sigs OR label-substrings that mark intended end screens, exempt
    /// from no-dead-end. A bare entry matches a state sig exactly OR (case-
    /// insensitively) any of that state's labels, so you can list either the
    /// signature or a human screen name.
    #[serde(default)]
    pub terminal_states: Vec<String>,
    #[serde(default)]
    pub custom: Vec<CustomInvariant>,
}

impl Default for InvariantsCfg {
    fn default() -> Self {
        InvariantsCfg {
            no_exception: true,
            no_jank: true,
            jank_pct_max: default_jank_pct_max(),
            all_labeled: true,
            no_dead_end: true,
            no_leak: true,
            rerender_flicker: true,
            no_overflow: true,
            overflow_min_px: default_overflow_min_px(),
            no_broken_render: true,
            no_hang: true,
            no_choice_anomaly: true,
            no_broken_route: true,
            terminal_states: vec![],
            custom: vec![],
        }
    }
}

impl InvariantsCfg {
    /// Does `sig` (with its observed `labels`) match a terminal-state allowlist
    /// entry? Matches either the exact sig or, case-insensitively, any label.
    pub fn terminal_states_match(&self, sig: &str, labels: Vec<String>) -> bool {
        self.terminal_states.iter().any(|t| {
            t == sig
                || labels.iter().any(|l| {
                    l.eq_ignore_ascii_case(t) || l.to_lowercase().contains(&t.to_lowercase())
                })
        })
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InvariantScope {
    #[default]
    State,
    Edge,
    Graph,
}

/// A user-declared invariant. The predicate fields are optional and combine by
/// scope; an empty predicate is a no-op (never fires). Start simple: regexes
/// over labels/actions, an unlabeled cap, a dead-end toggle, a reachability
/// requirement.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomInvariant {
    pub id: String,
    #[serde(default)]
    pub scope: InvariantScope,
    /// state: every state must have SOME label matching this regex.
    #[serde(default, deserialize_with = "de_opt_regex")]
    pub labels_match: Option<Regex>,
    /// state: NO label may match this regex.
    #[serde(default, deserialize_with = "de_opt_regex")]
    pub labels_absent: Option<Regex>,
    /// state: unlabeled tappables must be <= this.
    pub max_unlabeled: Option<u32>,
    /// edge: no edge may take an action matching this regex (e.g. "tap:Delete").
    #[serde(default, deserialize_with = "de_opt_regex")]
    pub action_absent: Option<Regex>,
    /// graph: this id also enforces no-dead-end.
    #[serde(default)]
    pub no_dead_end: bool,
    /// graph: some observed state must expose a label matching this regex.
    #[serde(default, deserialize_with = "de_opt_regex")]
    pub must_reach: Option<Regex>,
}

fn de_opt_regex<'de, D>(d: D) -> std::result::Result<Option<Regex>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(d)?;
    match s {
        Some(pat) => Regex::new(&pat).map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

fn default_overflow_min_px() -> i64 {
    2
}
fn default_jank_pct_max() -> f64 {
    25.0
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct App {
    /// "flutter-ios-sim" or "web-playwright".
    pub platform: String,
    /// Flutter project directory, relative to the config file (flutter only).
    #[serde(default)]
    pub project_dir: String,
    #[serde(default)]
    pub bundle_id: String,
    /// Defines passed to every run (dart-define for flutter, env for web).
    #[serde(default)]
    pub defines: std::collections::BTreeMap<String, String>,
    /// web-playwright: directory containing runner.mjs, and the app URL.
    pub web_runner_dir: Option<String>,
    pub url: Option<String>,
    /// rn-appium: directory containing the RN runner, the Appium server URL,
    /// and the platform/app for the Appium session.
    pub rn_runner_dir: Option<String>,
    pub appium_url: Option<String>,
    pub appium_caps: Option<std::collections::BTreeMap<String, String>>,
    /// Desktop / Electron / Tauri / instrumented: the app to drive. For the
    /// macOS AX backend this is a bundle id (falls back to bundleId); for the
    /// others it's a path to the built executable.
    pub executable: Option<String>,
    /// Override where the per-backend runner scripts live (macos-ax.swift,
    /// windows-uia.py, electron.mjs, ...). Default: REPROIT_RUNNERS env, else
    /// a `runners/` dir beside the config.
    pub runner_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Devices {
    #[serde(default = "default_device_type")]
    pub device_type: String,
    /// Sims are named <prefix>-A, <prefix>-B, ... and are the only sims touched.
    pub name_prefix: String,
    #[serde(default)]
    pub determinism: Determinism,
    #[serde(default)]
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Determinism {
    #[serde(default = "default_status_bar_time")]
    pub status_bar_time: String,
    /// [lat, lon]
    pub location: Option<[f64; 2]>,
    #[serde(default = "default_true")]
    pub disable_keyboard_intro: bool,
}

impl Default for Determinism {
    fn default() -> Self {
        Determinism {
            status_bar_time: default_status_bar_time(),
            location: None,
            disable_keyboard_intro: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Permission {
    pub service: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Reset {
    #[serde(default)]
    pub steps: Vec<ResetStep>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ResetStep {
    Command {
        run: String,
        #[serde(default)]
        required: bool,
    },
    Http {
        #[serde(default = "default_method")]
        method: String,
        url: String,
        body: Option<String>,
        #[serde(default)]
        required: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Journeys {
    #[serde(default = "default_journeys_dir")]
    pub dir: String,
    pub driver: String,
    /// Substring of a drive log line meaning "app is live"; gates launch of
    /// further devices and the start of recording.
    pub ready_marker: Option<String>,
    /// Substrings meaning the test reported its result. We key off these,
    /// never off process exit: flutter drive can linger (app timers keep the
    /// isolate alive).
    pub done_markers: Vec<String>,
    /// Prefix of structured action-log lines, parsed into actions.jsonl.
    #[serde(default = "default_action_prefix")]
    pub action_prefix: String,
    /// Optional journey-declared completion marker (e.g. "JOURNEY DONE").
    /// A device whose log prints it counts as done-and-passed without
    /// waiting for the runner verdict, so observer roles whose drive
    /// lingers never pay the linger grace. Print it as the LAST statement
    /// of a role's branch; an explicit runner verdict still overrides it.
    pub device_done_marker: Option<String>,
    #[serde(default = "default_timeout_sec")]
    pub timeout_sec: u64,
    /// Once the FIRST device reports done, how long to keep waiting for the
    /// rest before judging by observed markers. flutter drive can linger
    /// without ever flushing its runner verdict (app timers keep the isolate
    /// alive), so a finished multi-device run must not ride out the full
    /// timeout for one lingerer.
    #[serde(default = "default_linger_grace_sec")]
    pub linger_grace_sec: u64,
    /// Host-interaction hooks: when a device's log prints `marker`, run the
    /// command on the HOST (sh -c, cwd = config root) with {udid} and
    /// {device} substituted. For things unreachable from inside the app,
    /// e.g. clicking the native iOS location permission dialog.
    #[serde(default)]
    pub hooks: Vec<MarkerHook>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkerHook {
    pub marker: String,
    pub run: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Evidence {
    #[serde(default = "default_out_dir")]
    pub out_dir: String,
    #[serde(default = "default_true")]
    pub video: bool,
    #[serde(default = "default_true")]
    pub composite: bool,
    #[serde(default = "default_shoot_marker")]
    pub screenshot_marker: String,
}

impl Default for Evidence {
    fn default() -> Self {
        Evidence {
            out_dir: default_out_dir(),
            video: true,
            composite: true,
            screenshot_marker: default_shoot_marker(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Visual {
    pub shots_dir: String,
    #[serde(default = "default_pixel_tol")]
    pub pixel_tol: u8,
    #[serde(default = "default_fail_pct")]
    pub fail_pct: f64,
    /// Screens whose content is intentionally non-deterministic: diffed and
    /// reported, never failed.
    #[serde(default)]
    pub advisory: Vec<String>,
}

/// A store/marketing screenshot tour: a journey whose named SHOOT markers are
/// driven across locales and devices, verified against the expected screen, and
/// landed in a fastlane-compatible layout. Modeled on `Visual`; the SHOOT landing
/// path is the same machinery, organized per locale/device here.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Screenshots {
    /// The journey ("tour") to drive. Its SHOOT markers name the shots.
    pub tour: String,
    /// Durable output root. A journey-led layout (collapsing the axes that do not
    /// vary) lands under it; see modes/screenshots.rs. Default "screenshots".
    #[serde(default = "default_shots_out")]
    pub out: String,
    /// Locales to fan out across (e.g. de, ar, ja). Empty = app default only.
    /// Overridden by --locale on the command line.
    #[serde(default)]
    pub locales: Vec<String>,
    /// Device names/classes to fan out across. Empty = the configured device.
    /// Overridden by --device on the command line.
    #[serde(default)]
    pub devices: Vec<String>,
    /// Verify each shot landed on its expected screen via the state signature,
    /// failing loudly on navigation drift (the correctness gate). Default on.
    #[serde(default = "default_verify_signature")]
    pub verify_signature: bool,
    /// Explicit per-shot directory template, overriding the auto layout. Supports
    /// the placeholders {journey} {platform} {locale} {device} (an absent locale/
    /// device renders as "default"), joined under `out`. None = the auto layout
    /// (<out>/<journey> then locale/device/platform levels only when they vary).
    #[serde(default)]
    pub path_template: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gate {
    #[serde(default = "default_gate_runs")]
    pub runs: u32,
}

impl Default for Gate {
    fn default() -> Self {
        Gate {
            runs: default_gate_runs(),
        }
    }
}

fn default_device_type() -> String {
    "iPhone 16 Plus".into()
}
fn default_status_bar_time() -> String {
    "9:41".into()
}
fn default_method() -> String {
    "POST".into()
}
fn default_journeys_dir() -> String {
    "integration_test".into()
}
fn default_action_prefix() -> String {
    "JOURNEY".into()
}
fn default_timeout_sec() -> u64 {
    300
}
fn default_linger_grace_sec() -> u64 {
    90
}
fn default_out_dir() -> String {
    ".reproit/runs".into()
}
fn default_shoot_marker() -> String {
    "SHOOT:".into()
}
fn default_shots_out() -> String {
    "screenshots".into()
}
fn default_verify_signature() -> bool {
    true
}
fn default_pixel_tol() -> u8 {
    16
}
fn default_fail_pct() -> f64 {
    1.0
}
fn default_gate_runs() -> u32 {
    5
}
fn default_true() -> bool {
    true
}

pub struct Loaded {
    pub config: Config,
    /// Directory of the config file; relative paths resolve from here.
    pub root: PathBuf,
}

pub fn load(explicit: Option<&Path>) -> Result<Loaded> {
    let file = match explicit {
        Some(p) => p.to_path_buf(),
        None => find_config(&std::env::current_dir()?).context(
            "no reproit.yaml found in cwd or ancestors; pass --config or copy examples/reproit.example.yaml",
        )?,
    };
    let raw =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let canon = file.canonicalize()?;
    let parent = canon
        .parent()
        .context("config file has no parent directory")?;
    // A persisted zero-config run lives at `<cwd>/.reproit/reproit.yaml`; root it
    // at the cwd (the `.reproit` parent) so `.reproit/runs` and other relative
    // paths resolve from the project dir, not from inside `.reproit/`.
    let root = if parent.file_name().map(|n| n == ".reproit").unwrap_or(false) {
        parent
            .parent()
            .context("`.reproit` config has no parent directory")?
            .to_path_buf()
    } else {
        parent.to_path_buf()
    };
    parse_str(&raw, root).with_context(|| format!("parsing {}", file.display()))
}

/// Parse a config YAML string (env interpolation + validation), rooted at
/// `root` (where relative paths and `.reproit/` output resolve). Shared by
/// `load` (from a file) and the zero-config `--url` synthesizer.
pub fn parse_str(raw: &str, root: PathBuf) -> Result<Loaded> {
    let raw = interpolate_env(raw)?;
    let config: Config = serde_yaml::from_str(&raw)?;
    if crate::platform::resolve(&config.app.platform).is_none() {
        bail!(
            "unsupported platform {:?}; known: {}",
            config.app.platform,
            crate::platform::known_ids()
        );
    }
    if config.journeys.done_markers.is_empty() {
        bail!("journeys.doneMarkers must not be empty");
    }
    Ok(Loaded { config, root })
}

/// GitHub repo whose releases carry the prebuilt `reproit-web-runner.tar.gz`.
const RELEASE_REPO: &str = "ReproIt/reproit";

/// Where the self-healed web runner lives: the OS data dir, `<data>/reproit/web`.
/// Linux: `$XDG_DATA_HOME` or `~/.local/share`; macOS: `~/Library/Application
/// Support`; Windows: `%LOCALAPPDATA%`. `install.sh` provisions into this same
/// path, so a scripted install and a runtime self-heal converge on one location
/// and never need `REPROIT_WEB_RUNNER_DIR`.
pub fn web_runner_data_dir() -> PathBuf {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("reproit/web")
}

/// The web runner's JS logic, EMBEDDED in the binary at compile time. This is the
/// fix for runner/binary skew: the heavy `node_modules` (Playwright) is downloaded
/// once, but the runner SCRIPTS always come from the binary, so the runner logic
/// is in lock-step with the binary no matter how it was installed (`cargo install`,
/// brew, install.sh) -- no stale cache, no `REPROIT_WEB_RUNNER_DIR` needed.
const WEB_RUNNER_FILES: &[(&str, &str)] = &[
    (
        "runner.mjs",
        include_str!("../../../runners/web/runner.mjs"),
    ),
    (
        "flicker-oracle.mjs",
        include_str!("../../../runners/web/flicker-oracle.mjs"),
    ),
    ("probe.mjs", include_str!("../../../runners/web/probe.mjs")),
    (
        "annotate.mjs",
        include_str!("../../../runners/web/annotate.mjs"),
    ),
    (
        "jank-oracle.mjs",
        include_str!("../../../runners/web/jank-oracle.mjs"),
    ),
    ("jank.mjs", include_str!("../../../runners/web/jank.mjs")),
    (
        "differential.mjs",
        include_str!("../../../runners/web/differential.mjs"),
    ),
    (
        "package.json",
        include_str!("../../../runners/web/package.json"),
    ),
];

/// Write the binary's embedded runner scripts into `dir`, overwriting any stale
/// copies, so the runner logic matches this binary exactly.
fn write_embedded_runner(dir: &Path) -> Result<()> {
    for (name, contents) in WEB_RUNNER_FILES {
        std::fs::write(dir.join(name), contents)
            .with_context(|| format!("writing embedded runner {name}"))?;
    }
    Ok(())
}

/// A DEV runner used verbatim (live edits, no embed sync): an explicit
/// `$REPROIT_WEB_RUNNER_DIR`, a source checkout at `./runners/web`, or the
/// binary's sibling. Must already carry `node_modules`. `None` falls through to
/// the self-provisioned data-dir runner.
fn find_dev_runner_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("REPROIT_WEB_RUNNER_DIR") {
        if !d.trim().is_empty() {
            candidates.push(PathBuf::from(d));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("runners/web"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            candidates.push(p.join("runners/web"));
        }
    }
    candidates
        .into_iter()
        .find(|c| c.join("node_modules").is_dir())
}

/// Return a ready-to-use web runner dir. A dev/source runner is used verbatim;
/// otherwise the self-provisioned data-dir runner is used, downloading the heavy
/// `node_modules` (Playwright) once and then ALWAYS writing the binary's embedded
/// runner scripts over it. So a fresh `cargo install` / `brew install` / scripted
/// install all reach a working `reproit fuzz <url>` with the runner logic in
/// lock-step with the binary -- no `REPROIT_WEB_RUNNER_DIR`, no stale-cache skew.
/// `version` pins the matching release asset for the one-time `node_modules`
/// download; `log` receives human progress lines.
pub fn ensure_web_runner_dir(version: &str, log: &dyn Fn(&str)) -> Result<PathBuf> {
    // Dev/source checkout: used as-is so edits are live without a reinstall.
    if let Some(d) = find_dev_runner_dir() {
        return Ok(d);
    }
    let dir = web_runner_data_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating web runner dir {}", dir.display()))?;
    // Provision the heavy deps (Playwright + node_modules + browser) ONCE.
    if !dir.join("node_modules").is_dir() {
        // Node is the one external prerequisite for the web fuzzer (it drives
        // Playwright). Check up front so the failure is actionable, not a cryptic
        // spawn error deep in the drive loop.
        let node_ok = std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !node_ok {
            bail!(
                "reproit's web fuzzer needs Node.js (18+), which was not found. Install it \
                 (https://nodejs.org or `brew install node`), then re-run."
            );
        }
        log("web runner not found; provisioning it (one-time)...");
        download_and_extract_runner(version, &dir, log)?;
        ensure_web_browser(&dir, log)?;
    }
    // ALWAYS sync the runner scripts to THIS binary, so a binary update is never
    // paired with a stale runner (the cause of clips silently failing).
    write_embedded_runner(&dir)?;
    Ok(dir)
}

/// Release asset URL for `asset`. A clean release version (e.g. "0.1.2") pins the
/// matching tag; a dev build (e.g. "0.1.2-3-gabc-dirty") has no asset of its own,
/// so it falls back to the latest release.
fn release_asset_url(version: &str, asset: &str) -> String {
    let is_release =
        version.split('.').count() == 3 && version.chars().all(|c| c.is_ascii_digit() || c == '.');
    if is_release {
        format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}/{asset}")
    } else {
        format!("https://github.com/{RELEASE_REPO}/releases/latest/download/{asset}")
    }
}

/// Download the prebuilt runner bundle and extract it flat into `dir` (so
/// `dir/runner.mjs` and `dir/node_modules` land in place). Shells out to `curl`
/// and `tar`, which ship on macOS, Linux, and Windows 10+, to avoid pulling a
/// TLS/archive stack into the binary just for this one-time provisioning.
fn download_and_extract_runner(version: &str, dir: &Path, log: &dyn Fn(&str)) -> Result<()> {
    let url = release_asset_url(version, "reproit-web-runner.tar.gz");
    let tmp = std::env::temp_dir().join("reproit-web-runner.tar.gz");
    log(&format!("  downloading {url}"));
    let st = std::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&tmp)
        .arg(&url)
        .status()
        .context("running curl to download the web runner (is curl installed?)")?;
    if !st.success() {
        bail!("failed to download the web runner bundle from {url}");
    }

    // Integrity check: the binary downloads and executes this bundle, so verify
    // it against the SHA-256 the release publishes alongside it before trusting
    // it. We fetch the sibling `.sha256` asset and compare.
    //
    // Transition safety: older releases predate the checksum asset, so an ABSENT
    // checksum logs a warning and proceeds (a hard failure would brick installs
    // of already-published releases). Whenever the checksum IS present, a
    // mismatch is fatal (fail closed) -- we delete the temp file and bail.
    //
    // TODO: once every live release publishes `reproit-web-runner.tar.gz.sha256`
    // (the release workflow now does), make a MISSING checksum fatal too.
    let sum_url = release_asset_url(version, "reproit-web-runner.tar.gz.sha256");
    match fetch_text(&sum_url) {
        Some(sum_body) => match parse_sha256_hex(&sum_body) {
            Some(expected) => {
                let actual = sha256_file_hex(&tmp)
                    .with_context(|| format!("hashing downloaded bundle {}", tmp.display()))?;
                if !actual.eq_ignore_ascii_case(&expected) {
                    let _ = std::fs::remove_file(&tmp);
                    bail!(
                        "web runner checksum mismatch (expected {expected}, got {actual}); \
                         refusing to use a tampered or corrupt bundle"
                    );
                }
                log("  checksum verified.");
            }
            None => {
                let _ = std::fs::remove_file(&tmp);
                bail!("web runner checksum asset {sum_url} is malformed");
            }
        },
        None => {
            log("  WARNING: no checksum asset published for this release; \
                 skipping integrity verification (older release).");
        }
    }

    log("  extracting...");
    // Harden extraction: refuse absolute paths and `..` traversal, and don't
    // restore archived ownership. Flags are chosen to work on GNU tar (Linux),
    // bsdtar (macOS, Windows 10+). `-P` is OFF by default on all three, so a
    // leading `/` is already stripped; we additionally validate every entry path
    // ourselves below so a malicious `..` can never escape `dir` even if a tar
    // build ignored a flag.
    validate_tar_entries(&tmp)?;
    let st = std::process::Command::new("tar")
        .arg("--no-same-owner")
        .arg("-xzf")
        .arg(&tmp)
        .arg("-C")
        .arg(dir)
        .status()
        .context("running tar to extract the web runner (is tar installed?)")?;
    let _ = std::fs::remove_file(&tmp);
    if !st.success() {
        bail!("failed to extract the web runner bundle");
    }
    if !dir.join("node_modules").is_dir() {
        bail!(
            "web runner bundle extracted but node_modules is missing under {}",
            dir.display()
        );
    }
    Ok(())
}

/// Fetch a small text asset (the checksum file) over curl. Returns None when the
/// asset is absent (HTTP 404 -> curl `-f` exits non-zero), so callers can treat
/// a missing checksum as "older release, skip verification".
fn fetch_text(url: &str) -> Option<String> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Pull the 64-hex-char SHA-256 out of a `shasum`/`sha256sum`-style line, which
/// is `<hex>  <filename>` (the hex may also stand alone). Returns None if no
/// well-formed 64-char hex token is present.
fn parse_sha256_hex(body: &str) -> Option<String> {
    let tok = body.split_whitespace().next()?;
    if tok.len() == 64 && tok.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(tok.to_ascii_lowercase())
    } else {
        None
    }
}

/// SHA-256 of a file, lowercase hex.
fn sha256_file_hex(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex_lower(&h.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Defense-in-depth path validation: list the tarball entries and reject the
/// whole archive if any entry is absolute or contains a `..` component, so a
/// crafted bundle can never write outside the target dir regardless of how the
/// platform `tar` treats traversal. Cross-platform: `tar -tzf` lists on GNU tar
/// and bsdtar alike.
fn validate_tar_entries(tarball: &Path) -> Result<()> {
    let out = std::process::Command::new("tar")
        .arg("-tzf")
        .arg(tarball)
        .output()
        .context("listing the web runner tarball entries (is tar installed?)")?;
    if !out.status.success() {
        bail!("failed to list the web runner bundle (corrupt download?)");
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    for entry in listing.lines() {
        let e = entry.trim();
        if e.is_empty() {
            continue;
        }
        // Absolute paths (unix `/...`, Windows `C:\...` or `\...`).
        let abs =
            e.starts_with('/') || e.starts_with('\\') || (e.len() >= 2 && e.as_bytes()[1] == b':');
        if abs {
            bail!("web runner bundle contains an absolute path entry ({e}); refusing to extract");
        }
        // `..` traversal in any path component (handle both separators).
        if e.split(['/', '\\']).any(|c| c == "..") {
            bail!("web runner bundle contains a `..` traversal entry ({e}); refusing to extract");
        }
    }
    Ok(())
}

/// Ensure the headless browser the runner drives is installed (Playwright caches
/// it outside the runner dir, so this is a no-op when already present).
fn ensure_web_browser(dir: &Path, log: &dyn Fn(&str)) -> Result<()> {
    let cli = dir.join("node_modules/playwright/cli.js");
    if !cli.exists() {
        return Ok(());
    }
    log("  ensuring the headless browser (chromium)...");
    let st = std::process::Command::new("node")
        .arg(&cli)
        .args(["install", "chromium"])
        .status()
        .context("running `playwright install chromium`")?;
    if !st.success() {
        bail!("failed to install the chromium browser for the web runner");
    }
    Ok(())
}

/// Build a web `Loaded` for the zero-config `reproit fuzz <url>` run, with
/// `.reproit/` output under `root` (the cwd). The synthesized config is also
/// persisted to `<root>/.reproit/reproit.yaml` so a follow-up `check <id>` /
/// `keep` / `repros` can replay the run without a hand-written reproit.yaml.
pub fn synthesize_web(url: &str, web_runner_dir: &Path, root: PathBuf) -> Result<Loaded> {
    // Serialize the URL and path as JSON strings: YAML is a JSON superset, so a
    // JSON-quoted scalar is a valid, fully-escaped YAML scalar. Raw interpolation
    // would let a `"`, backslash, or newline in the URL/path break the document.
    let url = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_string());
    let wrd = serde_json::to_string(&web_runner_dir.display().to_string())
        .unwrap_or_else(|_| "\"\"".to_string());
    let yaml = format!(
        "app:\n  platform: web-playwright\n  webRunnerDir: {wrd}\n  url: {url}\n  \
         defines: {{}}\ndevices:\n  namePrefix: web\nreset:\n  steps: []\njourneys:\n  \
         dir: integration_test\n  driver: web\n  readyMarker: \"claimed role\"\n  \
         doneMarkers:\n    - All tests passed\n    - Some tests failed\n  \
         deviceDoneMarker: \"JOURNEY DONE\"\n  actionPrefix: \"JOURNEY\"\n  timeoutSec: 300\n\
         evidence:\n  outDir: .reproit/runs\n  video: false\n",
    );
    let loaded = parse_str(&yaml, root)?;
    // Persist so the zero-config flow is replayable: `find_config` picks this up
    // as a fallback and `load` roots it back at the cwd (the `.reproit` parent).
    // Best-effort: a write failure leaves the run working but non-replayable,
    // exactly as before this was persisted.
    let dir = loaded.root.join(".reproit");
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join("reproit.yaml"), &yaml);
    }
    Ok(loaded)
}

/// Zero-config TUI run: synthesize a `platform: tui` config that drives the given
/// terminal executable in a PTY (the built-in `reproit __tui` runner). The analogue
/// of [`synthesize_web`] for `reproit sweep <executable>` (e.g. `lazygit`, `htop`).
/// `executable` is the command line run via `sh -c`, so args and PATH resolution
/// work. Persisted to `.reproit/reproit.yaml` so a follow-up check/keep replays.
pub fn synthesize_tui(executable: &str, root: PathBuf) -> Result<Loaded> {
    // JSON-quote into the YAML (a JSON scalar is a valid, fully-escaped YAML
    // scalar), so a quote/backslash/space in the command can't break the document.
    let exe = serde_json::to_string(executable).unwrap_or_else(|_| "\"\"".to_string());
    // Same ready/done/action markers the `__tui` runner emits (tui.rs: "JOURNEY
    // claimed role=a", "JOURNEY DONE", "All tests passed"), so the orchestrator
    // contract matches without a hand-written reproit.yaml.
    let yaml = format!(
        "app:\n  platform: tui\n  executable: {exe}\n  defines: {{}}\ndevices:\n  \
         namePrefix: tui\nreset:\n  steps: []\njourneys:\n  driver: \"\"\n  \
         readyMarker: \"claimed role\"\n  doneMarkers:\n    - All tests passed\n    \
         - Some tests failed\n  deviceDoneMarker: \"JOURNEY DONE\"\n  \
         actionPrefix: \"JOURNEY\"\n  timeoutSec: 300\nevidence:\n  \
         outDir: .reproit/runs\n  video: false\n",
    );
    let loaded = parse_str(&yaml, root)?;
    let dir = loaded.root.join(".reproit");
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join("reproit.yaml"), &yaml);
    }
    Ok(loaded)
}

fn find_config(from: &Path) -> Option<PathBuf> {
    let mut dir = from.to_path_buf();
    loop {
        let candidate = dir.join("reproit.yaml");
        if candidate.exists() {
            return Some(candidate);
        }
        // Fallback: a persisted zero-config `fuzz <url>` run writes its
        // synthesized config here, so a later check/keep/repros finds it.
        // `load` re-roots this at `dir` (not `.reproit/`) so relative paths hold.
        let synth = dir.join(".reproit").join("reproit.yaml");
        if synth.exists() {
            return Some(synth);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Interpolate environment variables across the whole config (every field, not
/// just `app.defines`), using the familiar shell parameter-expansion subset:
///   - `${VAR}`              substitute VAR; empty if unset (back-compat default)
///   - `${VAR:-default}`     substitute VAR, or `default` if unset/empty
///   - `${VAR:?message}`     substitute VAR, or fail the load with `message`
/// `${VAR:?}` forms that resolve to nothing are collected and reported together
/// so one run surfaces every missing required var, not just the first.
fn interpolate_env(raw: &str) -> Result<String> {
    let re = Regex::new(r"\$\{(\w+)(?::(-|\?)([^}]*))?\}").unwrap();
    let mut missing: Vec<String> = Vec::new();
    let out = re
        .replace_all(raw, |caps: &regex::Captures| {
            let name = &caps[1];
            let val = std::env::var(name).ok().filter(|v| !v.is_empty());
            match caps.get(2).map(|m| m.as_str()) {
                // ${VAR:-default}
                Some("-") => val.unwrap_or_else(|| caps[3].to_string()),
                // ${VAR:?message} -> required; record and fail after the pass.
                Some("?") => val.unwrap_or_else(|| {
                    let msg = caps[3].trim();
                    let msg = if msg.is_empty() {
                        format!("required config variable {name} is not set")
                    } else {
                        format!("{name}: {msg}")
                    };
                    missing.push(msg);
                    String::new()
                }),
                // ${VAR} (back-compat: empty when unset)
                _ => val.unwrap_or_default(),
            }
        })
        .into_owned();
    if !missing.is_empty() {
        bail!("unresolved config variables:\n  {}", missing.join("\n  "));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        interpolate_env, load, parse_sha256_hex, sha256_file_hex, synthesize_tui, synthesize_web,
    };
    use std::path::PathBuf;

    #[test]
    fn parse_sha256_hex_accepts_shasum_lines_and_rejects_junk() {
        let hex = "e".repeat(64);
        // bare hex
        assert_eq!(parse_sha256_hex(&hex).as_deref(), Some(hex.as_str()));
        // `shasum`/`sha256sum` "<hex>  <file>" form, plus trailing newline
        let line = format!("{hex}  reproit-web-runner.tar.gz\n");
        assert_eq!(parse_sha256_hex(&line).as_deref(), Some(hex.as_str()));
        // uppercase is normalized to lowercase
        assert_eq!(
            parse_sha256_hex(&"A".repeat(64)).as_deref(),
            Some("a".repeat(64).as_str())
        );
        // wrong length / non-hex / empty -> None
        assert!(parse_sha256_hex("deadbeef").is_none());
        assert!(parse_sha256_hex(&"z".repeat(64)).is_none());
        assert!(parse_sha256_hex("").is_none());
    }

    #[test]
    fn sha256_file_hex_matches_known_vector() {
        // SHA-256("abc") is a published NIST test vector.
        let dir = std::env::temp_dir().join(format!("reproit_sha_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("data");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(
            sha256_file_hex(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // The compare in download_and_extract_runner is case-insensitive; confirm.
        let upper = sha256_file_hex(&f).unwrap().to_ascii_uppercase();
        assert!(upper.eq_ignore_ascii_case(&sha256_file_hex(&f).unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn synthesize_web_parses_to_a_valid_web_config() {
        let proj = std::env::temp_dir().join(format!("reproit_synth_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&proj);
        std::fs::create_dir_all(&proj).unwrap();
        let l = synthesize_web(
            "https://app.example.com/x:y",
            &PathBuf::from("/tmp/wr"),
            proj.clone(),
        )
        .expect("synthesized web config parses + validates");
        assert_eq!(l.config.app.platform, "web-playwright");
        assert_eq!(
            l.config.app.url.as_deref(),
            Some("https://app.example.com/x:y")
        );
        assert_eq!(l.config.app.web_runner_dir.as_deref(), Some("/tmp/wr"));
        assert_eq!(l.root, proj);
        // The journeys.doneMarkers validation (load's hard gate) must pass.
        assert!(!l.config.journeys.done_markers.is_empty());
        // The synthesized config is persisted so a later check/keep can replay.
        assert!(proj.join(".reproit").join("reproit.yaml").exists());
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn synthesize_tui_parses_to_a_valid_tui_config() {
        let proj = std::env::temp_dir().join(format!("reproit_tui_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&proj);
        std::fs::create_dir_all(&proj).unwrap();
        // A command with args + a quote, to exercise the JSON/YAML escaping.
        let l = synthesize_tui("lazygit --use-config \"x y\"", proj.clone())
            .expect("synthesized tui config parses + validates");
        assert_eq!(l.config.app.platform, "tui");
        assert_eq!(
            l.config.app.executable.as_deref(),
            Some("lazygit --use-config \"x y\"")
        );
        assert!(!l.config.journeys.done_markers.is_empty());
        assert_eq!(
            l.config.journeys.device_done_marker.as_deref(),
            Some("JOURNEY DONE")
        );
        assert!(proj.join(".reproit").join("reproit.yaml").exists());
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn zero_config_persists_and_reloads_rooted_at_cwd() {
        // The zero-config `fuzz <url>` papercut fix: synthesize_web persists its
        // config, and loading that persisted `.reproit/reproit.yaml` re-roots at
        // the PROJECT dir (not `.reproit/`), so a follow-up `check <id>` resolves
        // `.reproit/runs` and friends from the cwd and replays correctly.
        let proj = std::env::temp_dir().join(format!("reproit_reload_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&proj);
        std::fs::create_dir_all(&proj).unwrap();
        synthesize_web(
            "https://app.example.com",
            &PathBuf::from("/tmp/wr"),
            proj.clone(),
        )
        .expect("synthesize");
        let synth = proj.join(".reproit").join("reproit.yaml");
        assert!(synth.exists(), "config persisted under .reproit/");
        let reloaded = load(Some(&synth)).expect("reload persisted config");
        assert_eq!(
            reloaded.root,
            proj.canonicalize().unwrap(),
            "root is the project dir, not .reproit/"
        );
        assert_eq!(
            reloaded.config.app.url.as_deref(),
            Some("https://app.example.com")
        );
        let _ = std::fs::remove_dir_all(&proj);
    }

    // Each test uses a unique var name so parallel tests don't race on env state.
    #[test]
    fn bare_var_substitutes_or_empties() {
        std::env::set_var("RIT_TEST_BARE", "/runner");
        assert_eq!(
            interpolate_env("dir: ${RIT_TEST_BARE}").unwrap(),
            "dir: /runner"
        );
        std::env::remove_var("RIT_TEST_BARE_UNSET");
        assert_eq!(
            interpolate_env("dir: ${RIT_TEST_BARE_UNSET}").unwrap(),
            "dir: "
        );
    }

    #[test]
    fn default_form_falls_back_when_unset() {
        std::env::remove_var("RIT_TEST_DEF");
        assert_eq!(
            interpolate_env("dir: ${RIT_TEST_DEF:-./runners/web}").unwrap(),
            "dir: ./runners/web"
        );
        std::env::set_var("RIT_TEST_DEF", "/explicit");
        assert_eq!(
            interpolate_env("dir: ${RIT_TEST_DEF:-./runners/web}").unwrap(),
            "dir: /explicit"
        );
    }

    #[test]
    fn required_form_errors_when_unset() {
        std::env::remove_var("RIT_TEST_REQ");
        let err = interpolate_env("dir: ${RIT_TEST_REQ:?must be set}").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("RIT_TEST_REQ"), "got: {msg}");
        assert!(msg.contains("must be set"), "got: {msg}");
    }

    #[test]
    fn required_form_passes_when_set() {
        std::env::set_var("RIT_TEST_REQ_OK", "x");
        assert_eq!(
            interpolate_env("v: ${RIT_TEST_REQ_OK:?nope}").unwrap(),
            "v: x"
        );
    }

    // End-to-end: app.webRunnerDir (the field from issue #1) resolves through the
    // real loader, both the :-default fallback and an explicit override.
    #[test]
    fn loader_resolves_app_web_runner_dir() {
        let dir = std::env::temp_dir().join("rit_cfg_e2e_wrd");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reproit.yaml");
        std::fs::write(
            &path,
            "app:\n  platform: web-playwright\n  webRunnerDir: ${RIT_E2E_WRD:-./runners/web}\n\
             devices:\n  namePrefix: x\njourneys:\n  driver: noop\n  doneMarkers: [done]\n",
        )
        .unwrap();

        std::env::remove_var("RIT_E2E_WRD");
        let loaded = super::load(Some(&path)).unwrap();
        assert_eq!(
            loaded.config.app.web_runner_dir.as_deref(),
            Some("./runners/web")
        );

        std::env::set_var("RIT_E2E_WRD", "/custom/runner");
        let loaded = super::load(Some(&path)).unwrap();
        assert_eq!(
            loaded.config.app.web_runner_dir.as_deref(),
            Some("/custom/runner")
        );

        std::env::remove_var("RIT_E2E_WRD");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn examples_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/configs")
    }

    // Every shipped per-platform example must parse + resolve its platform +
    // satisfy the schema. This is what would have caught the issue-#1 mistake (a
    // top-level / misplaced field), and it guards every framework's example, so
    // they can't silently rot as the schema evolves.
    #[test]
    fn all_example_configs_load() {
        let dir = examples_dir();
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("examples/configs") {
            let p = entry.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            super::load(Some(&p))
                .unwrap_or_else(|e| panic!("{} failed to load: {e:#}", p.display()));
            count += 1;
        }
        assert!(count >= 13, "expected >= 13 example configs, found {count}");
    }

    // The desktop-toolkit example covers four platform ids in one file; verify
    // each id actually loads (swap it into the example, load, assert ok).
    #[test]
    fn desktop_toolkit_ids_all_load() {
        let src = std::fs::read_to_string(examples_dir().join("reproit.desktop-toolkit.yaml"))
            .expect("toolkit example");
        let dir = std::env::temp_dir().join("rit_toolkit_ids");
        std::fs::create_dir_all(&dir).unwrap();
        for id in ["qt", "gtk", "avalonia", "wxwidgets"] {
            let yaml = src.replace("platform: qt", &format!("platform: {id}"));
            let path = dir.join("reproit.yaml");
            std::fs::write(&path, yaml).unwrap();
            super::load(Some(&path)).unwrap_or_else(|e| panic!("toolkit {id} failed: {e:#}"));
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
