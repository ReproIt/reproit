//! Config schema and loader. See reproit.example.yaml for the shape.

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
            "no reproit.yaml found in cwd or ancestors; pass --config or copy reproit.example.yaml",
        )?,
    };
    let raw =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let raw = interpolate_env(&raw)?;
    let config: Config =
        serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", file.display()))?;
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
    let root = file
        .canonicalize()?
        .parent()
        .context("config file has no parent directory")?
        .to_path_buf();
    Ok(Loaded { config, root })
}

fn find_config(from: &Path) -> Option<PathBuf> {
    let mut dir = from.to_path_buf();
    loop {
        let candidate = dir.join("reproit.yaml");
        if candidate.exists() {
            return Some(candidate);
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
    use super::interpolate_env;

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
            interpolate_env("dir: ${RIT_TEST_DEF:-./web-runner}").unwrap(),
            "dir: ./web-runner"
        );
        std::env::set_var("RIT_TEST_DEF", "/explicit");
        assert_eq!(
            interpolate_env("dir: ${RIT_TEST_DEF:-./web-runner}").unwrap(),
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
}
