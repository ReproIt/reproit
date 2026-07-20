//! Config schema and loader. See examples/reproit.example.yaml for the shape.

use regex::Regex;
use serde::{Deserialize, Serialize};

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
    /// Portable temporal properties evaluated over normalized runner events.
    #[serde(default)]
    pub contracts: Vec<crate::model::contracts::ContractSpec>,
    /// Experimental backend structural analysis. This is intentionally absent
    /// from public documentation until its cross-language validation gate is
    /// complete.
    #[serde(default)]
    pub backend: crate::model::backend::BackendConfig,
}

/// Login credentials for journeys, resolved at run time from the encrypted
/// vault and injected as env (never stored in config or repo). See auth.rs.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthCfg {
    /// Encrypted vault path, relative to the config file.
    /// Default: .reproit/secrets.vault
    pub vault: Option<String>,
    #[serde(default)]
    pub accounts: Vec<Account>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStrategy {
    Password,
    PasswordOtp,
    PhoneOtp,
    EmailLink,
    OauthTest,
    Session,
    Api,
}

impl AuthStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthStrategy::Password => "password",
            AuthStrategy::PasswordOtp => "password-otp",
            AuthStrategy::PhoneOtp => "phone-otp",
            AuthStrategy::EmailLink => "email-link",
            AuthStrategy::OauthTest => "oauth-test",
            AuthStrategy::Session => "session",
            AuthStrategy::Api => "api",
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthValidate {
    pub text: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Account {
    /// Account handle, e.g. "alice" or "admin"; becomes the env namespace
    /// REPROIT_SECRET_<NAME>_*.
    pub name: String,
    /// Login mechanism. `login(<account>)` drives the app's login journey with
    /// these fields; `auth(<account>)` restores a saved session when present.
    pub strategy: Option<AuthStrategy>,
    /// Non-secret backend user id for this account. Lets reset steps clear this
    /// account's data by reference (`${account.<name>.userId}`) instead of a
    /// hardcoded UUID, so reset stays in sync with the accounts a scenario
    /// uses.
    pub user_id: Option<String>,
    /// Non-secret username/email. Use ${ENV} interpolation or put it here.
    pub username: Option<String>,
    /// Vault keys for account identifiers. Prefer these over plaintext config
    /// when emails/phones are private.
    pub username_ref: Option<String>,
    pub email_ref: Option<String>,
    pub phone_ref: Option<String>,
    /// Vault key holding this account's password.
    pub password_ref: Option<String>,
    /// Vault key holding a base32 TOTP secret (2FA / one-time codes).
    pub totp_ref: Option<String>,
    /// Vault key holding a fixed/manual one-time code. Useful for deterministic
    /// test-mode phone/email OTP before provider adapters are configured.
    pub otp_ref: Option<String>,
    /// Vault key holding a JSON session blob for the `auth(<account>)` login
    /// bypass: a map the runner restores (e.g. localStorage entries) so the app
    /// boots authenticated without driving the login UI.
    pub storage_ref: Option<String>,
    /// Optional success signal for auth doctor and generated journey guidance.
    pub validate: Option<AuthValidate>,
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

/// The INVARIANTS oracle config. Built-ins (no-exception, no-jank,
/// no-occluded-control, no-leak) ship ON by default; flip any off here. Custom
/// invariants are declared under `custom`. See model/invariants.rs.
///
/// reproit.yaml shape:
/// ```yaml
/// invariants:
///   noException: true        # edge: any uncaught app exception (default on)
///   noJank: true             # state: per-state frame budget (SIM ONLY)
///   jankPctMax: 25.0         # the budget no-jank / custom jank checks use
///   noOccludedControl: true  # a foreign element blocks a control's hit target
///   noDetachedIndicator: true # an explicit indicator relationship broke
///   noAccessibilityStateMismatch: true # native state contradicts the AX tree
///   noLeak: true             # graph: leaked-resource signal (sim-authoritative)
///   terminalStates: [order_confirmed, advanced]  # intended end screens, exempt
///   custom:
///     - id: settings-has-save
///       scope: state
///       labelsMatch: "(?i)save"          # every state must have a matching label
///     - id: no-raw-error-text
///       scope: state
///       labelsAbsent: "(?i)null|exception"  # no state may show a matching label
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
    /// A visible control's center is covered by a foreign element, so pointer
    /// activation hits the covering element instead of the control.
    #[serde(default = "default_true")]
    pub no_occluded_control: bool,
    /// Detached indicator: an explicitly declared indicator is no longer near
    /// its explicitly declared owner or has escaped their declared container.
    /// Web evaluates `data-reproit-indicator-*`; mobile SDKs evaluate
    /// equivalent framework-native global geometry. Both emit
    /// `EXPLORE:RELATION` after two stable samples. Missing, ambiguous,
    /// hidden, transformed, or animating relationships abstain and remain
    /// silent.
    #[serde(default = "default_true")]
    pub no_detached_indicator: bool,
    /// A native control's live authoritative state contradicts Chromium's
    /// computed accessibility state for the exact same DOM node. Both channels
    /// must agree in two settled samples; missing or ambiguous evidence abstains.
    #[serde(default = "default_true")]
    pub no_accessibility_state_mismatch: bool,
    #[serde(default = "default_true")]
    pub no_leak: bool,
    /// Presented-frame flicker detection. The legacy name is retained for
    /// config compatibility; DOM node replacement alone is diagnostic, not
    /// a finding.
    #[serde(default = "default_true")]
    pub rerender_flicker: bool,
    /// Broken rendered content: a label showing a stringify/template artifact
    /// (`[object Object]`, a bare `undefined`/`null`/`NaN`, or an unrendered
    /// `{{...}}`/`${...}` placeholder). Deterministic DOM/label scan from the
    /// web runner (`EXPLORE:CONTENTBUG`).
    #[serde(default = "default_true")]
    pub no_broken_render: bool,
    /// Blank screen: a settled empty state corroborated by independent
    /// authority such as a first-party exception or renderer crash. Structural
    /// visual emptiness alone is diagnostic and always abstains.
    #[serde(default = "default_true")]
    pub no_blank_screen: bool,
    /// Broken asset: a dead or browser-rejected critical subresource in a
    /// state. Includes visible dead images/tofu and same-origin stylesheet
    /// or application script failures. Emitted by the web runner as
    /// `EXPLORE:BROKENASSET`.
    #[serde(default = "default_true")]
    pub no_broken_asset: bool,
    /// Zoom reflow: a route that breaks at 200% zoom (WCAG 1.4.10 Reflow). The
    /// web runner re-renders each newly-reached route at half the viewport's
    /// CSS size and flags a document that then requires two-dimensional
    /// scrolling or a previously visible tappable whose hit rect collapses
    /// below 1px (`EXPLORE:ZOOMREFLOW`).
    #[serde(default = "default_true")]
    pub no_zoom_reflow: bool,
    /// Main-thread freeze (no-progress): an action whose synchronous handler
    /// blocked the main thread past the hang floor (the app stopped making
    /// progress). Deterministic, keyed off the browser's Long Tasks trace from
    /// the web runner (`EXPLORE:HANG`).
    #[serde(default = "default_true")]
    pub no_hang: bool,
    /// Component-choice anomaly: a multi-choice component (language tabs, a
    /// radio group) where one option behaves differently from its siblings,
    /// shifting the global layout. Differential (outlier vs siblings), not
    /// an absolute threshold. From the web runner's `EXPLORE:CHOICEBUG`;
    /// Chromium-tier.
    #[serde(default = "default_true")]
    pub no_choice_anomaly: bool,
    /// Broken route: the app links to a URL whose document responds 4xx/5xx (a
    /// dead route / 404). Keyed off the navigation HTTP status from the web
    /// runner (`EXPLORE:BROKENROUTE`); structural and false-positive-free.
    /// Web only.
    #[serde(default = "default_true")]
    pub no_broken_route: bool,
    /// Stuck soft keyboard: the on-screen keyboard is visible while no text
    /// input is focused (the app navigated away from a field and never
    /// dismissed the IME). From the native mobile explorers'
    /// `EXPLORE:STUCKKEYBOARD`; platform ground truth, false-positive-free.
    /// Native mobile only.
    #[serde(default = "default_true")]
    pub no_stuck_keyboard: bool,
    /// Rotation-stability: a state must survive a device rotation. The explorer
    /// rotates the surface (portrait <-> landscape / split-screen), reflows,
    /// then rotates BACK to the original orientation; a screen that does
    /// not rebuild the same structure lost content/state across the
    /// orientation lifecycle. From the native (Flutter, Appium) and
    /// Chromium (web, electron, tauri) explorers' `EXPLORE:ROTATION`;
    /// round-trip identity with value-state excluded, so it is
    /// deterministic and false-positive-free.
    #[serde(default = "default_true")]
    pub no_rotation_loss: bool,
    /// Background-restore-stability: a state must survive the app background ->
    /// foreground lifecycle. The explorer backgrounds the app (paused/hidden)
    /// and restores it (resumed/visible); an app that returns to a
    /// different screen or loses state violates it. From the native
    /// (Flutter, Appium) and Chromium (web, electron, tauri) explorers'
    /// `EXPLORE:BGRESTORE`; no size change and value-state excluded, so it
    /// is deterministic and false-positive-free.
    #[serde(default = "default_true")]
    pub no_background_loss: bool,
    /// Duplicate submit: a submit-like control that fires the SAME first-party
    /// non-GET request twice when tapped twice in rapid succession (no
    /// double-activation guard, so a double click submits an order/payment
    /// twice). Evaluation is on by default; the RUNNER probe that
    /// double-dispatches taps is opt-in per run via REPROIT_DUPSUBMIT=1,
    /// because double-firing real submits changes exploration semantics. From
    /// the web runner (`EXPLORE:DUPSUBMIT`).
    #[serde(default = "default_true")]
    pub no_duplicate_submit: bool,
    /// Focus loss: a non-navigating tap that leaves document.activeElement on
    /// <body> while the tapped control still exists (the interaction's
    /// re-render dropped keyboard focus, so a keyboard user loses their
    /// place). The runner suppresses dialog open/close, removed controls, and
    /// link taps upstream. From the web runner (`EXPLORE:FOCUSLOSS`).
    #[serde(default = "default_true")]
    pub no_focus_loss: bool,
    /// App-registered invariant: a predicate the app declared via the SDK
    /// (`ReproIt.invariant("id", fn)`) that must hold in every visited state.
    /// The SDK evaluates its registered predicates on each state-settle under
    /// the fuzzer and reports the failures; any reported violation is a finding
    /// (`EXPLORE:INVARIANT`). The app owns the ground truth, so it is
    /// false-positive-free. Distinct from the declarative `custom` regex rules
    /// below. Ports to every backend whose SDK has a state hook.
    #[serde(default = "default_true")]
    pub no_invariant_violation: bool,
    /// Scroll round-trip: in a scrollable list, the content at a pinned offset
    /// is not identical after scrolling away and back (a list-recycling /
    /// virtualization bug rebinds a different row to the same position). Driven
    /// by the explorers that can scroll (Flutter, web, Appium)
    /// (`EXPLORE:SCROLLROUNDTRIP`); structural content comparison, value-state
    /// normalized out.
    #[serde(default = "default_true")]
    pub no_scroll_round_trip: bool,
    /// Listener leak: event listeners and/or DOM nodes that grow monotonically
    /// across REPEATED visits to the same route (enter route, leave, re-enter).
    /// A route whose components add listeners/nodes on every mount without
    /// releasing them on unmount climbs unbounded -- the classic SPA leak.
    /// Evaluation is on by default; the RUNNER probe that drives the revisit
    /// loop is opt-in per run via REPROIT_LISTENERLEAK=1 (repeated
    /// navigation changes exploration semantics, like the duplicate-submit
    /// probe). From the web/electron runners (`EXPLORE:LISTENERLEAK`);
    /// sequence/repeat-dependent, so it belongs to fuzz/soak, not the
    /// state-present scan crawl.
    #[serde(default = "default_true")]
    pub no_listener_leak: bool,
    /// Wakelock leak: a wakelock (or a window FLAG_KEEP_SCREEN_ON) acquired on
    /// a screen is still held after the user navigates away, keeping the
    /// CPU/screen awake off the video/map/call screen that needed it (a
    /// battery drain). From the Android/Appium explorer's
    /// `EXPLORE:WAKELOCK`, off `dumpsys power` ground truth compared before
    /// vs after leaving the screen; app-global, released, and short-lived
    /// locks are excluded, so it is deterministic and false-positive-free.
    /// Android only (iOS has no public wakelock introspection;
    /// web/desktop/TUI have no wakelock concept).
    #[serde(default = "default_true")]
    pub no_wakelock_leak: bool,
    /// Safe-area collision: an interactive control whose hit rect intersects a
    /// device safe-area inset (status bar / notch / Dynamic Island / home
    /// indicator / rounded corner), so it is obscured or hard to hit. Pure
    /// inset-vs-rect geometry from the native mobile explorers
    /// (`EXPLORE:SAFEAREA`); deterministic and false-positive-free. Native
    /// mobile only (desktop has no insets; headless web reports every inset
    /// as 0).
    #[serde(default = "default_true")]
    pub no_safe_area: bool,
    /// Permission dead-end: under a runtime-permission denial sweep, a screen
    /// the app reached after the denial is a genuine graph dead end (a
    /// stuck "please enable X" screen with no way forward). Uses the
    /// internal sink predicate but attributes the trap to the denied
    /// permission. From the native explorers' `EXPLORE:PERMISSIONWALK`
    /// under the denial sweep; silent outside it. Native mobile only (a
    /// backend that cannot deny a permission never emits the marker).
    #[serde(default = "default_true")]
    pub no_permission_dead_end: bool,
    /// State sigs OR label-substrings that mark intended end screens, exempt
    /// from permission-walk. A bare entry matches a state sig exactly OR (case-
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
            no_occluded_control: true,
            no_detached_indicator: true,
            no_accessibility_state_mismatch: true,
            no_leak: true,
            rerender_flicker: true,
            no_broken_render: true,
            no_blank_screen: true,
            no_broken_asset: true,
            no_zoom_reflow: true,
            no_hang: true,
            no_choice_anomaly: true,
            no_broken_route: true,
            no_stuck_keyboard: true,
            no_rotation_loss: true,
            no_background_loss: true,
            no_duplicate_submit: true,
            no_focus_loss: true,
            no_invariant_violation: true,
            no_scroll_round_trip: true,
            no_listener_leak: true,
            no_wakelock_leak: true,
            no_safe_area: true,
            no_permission_dead_end: true,
            terminal_states: vec![],
            custom: vec![],
        }
    }
}

impl InvariantsCfg {
    /// Does `sig` (with its observed `labels`) match a terminal-state allowlist
    /// entry? Matches either the exact sig or, case-insensitively, any label.
    pub fn terminal_states_match(&self, sig: &str, labels: &[String]) -> bool {
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
/// over labels/actions and a reachability requirement.
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
    /// edge: no edge may take an action matching this regex (e.g.
    /// "tap:Delete").
    #[serde(default, deserialize_with = "de_opt_regex")]
    pub action_absent: Option<Regex>,
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
    /// "flutter" or "web".
    pub platform: String,
    /// Flutter project directory, relative to the config file (flutter only).
    #[serde(default)]
    pub project_dir: String,
    #[serde(default)]
    pub bundle_id: String,
    /// Defines passed to every run (dart-define for flutter, env for web).
    #[serde(default)]
    pub defines: std::collections::BTreeMap<String, String>,
    /// web: directory containing runner.mjs, and the app URL.
    pub web_runner_dir: Option<String>,
    pub url: Option<String>,
    /// react-native: directory containing the RN runner, the Appium server URL,
    /// and the platform/app for the Appium session.
    pub rn_runner_dir: Option<String>,
    pub appium_url: Option<String>,
    pub appium_caps: Option<std::collections::BTreeMap<String, String>>,
    /// Desktop / Electron / Tauri / instrumented: the app to drive. For the
    /// macOS AX backend this is a bundle id (falls back to bundleId); for the
    /// others it's a path to the built executable.
    pub executable: Option<String>,
    /// Override where the per-backend runner scripts live (macos-ax.swift,
    /// electron.mjs, ...). The Windows (UIA) and Linux (AT-SPI) desktop runners
    /// are in-process Rust subcommands (`reproit __uia` / `__atspi`) and need
    /// no script here. Default: REPROIT_RUNNERS env, else a `runners/` dir
    /// beside the config.
    pub runner_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Devices {
    #[serde(default = "default_device_type")]
    pub device_type: String,
    /// Sims are named <prefix>-A, <prefix>-B, ... and are the only sims
    /// touched.
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
/// landed in a Fastlane-style layout. Modeled on `Visual`; the SHOOT landing
/// path is the same machinery, organized per locale/device here.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Screenshots {
    /// The journey ("tour") to drive. Its SHOOT markers name the shots.
    pub tour: String,
    /// Durable output root. A journey-led layout (collapsing the axes that do
    /// not vary) lands under it; see modes/screenshots.rs. Default
    /// "screenshots".
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
    /// Explicit per-shot directory template, overriding the auto layout.
    /// Supports the placeholders {journey} {platform} {locale} {device} (an
    /// absent locale/ device renders as "default"), joined under `out`.
    /// None = the auto layout (<out>/<journey> then locale/device/platform
    /// levels only when they vary).
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
    crate::layout::default_runs_dir_rel().into()
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

mod loader;
mod synthesis;
mod web_runner;

pub use loader::{load, parse_str, Loaded};
pub use synthesis::{synthesize_tui, synthesize_web};
pub use web_runner::ensure_web_runner_dir;
// Retain the pre-refactor façade even though the current crate has no caller.
#[allow(unused_imports)]
pub use web_runner::web_runner_data_dir;

#[cfg(test)]
use loader::interpolate_env;

#[cfg(test)]
mod tests {
    use super::{interpolate_env, load, synthesize_tui, synthesize_web};
    use std::path::PathBuf;

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
        assert_eq!(l.config.app.platform, "web");
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
        // the PROJECT dir (not `.reproit/`), so a follow-up `reproit <id>` resolves
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
        let dir = std::env::temp_dir().join(format!("rit_cfg_e2e_wrd_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reproit.yaml");
        std::fs::write(
            &path,
            "app:\n  platform: web\n  webRunnerDir: ${RIT_E2E_WRD:-./runners/web}\ndevices:\n  \
             namePrefix: x\njourneys:\n  driver: noop\n  doneMarkers: [done]\n",
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
