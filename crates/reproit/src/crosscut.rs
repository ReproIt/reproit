//! Cross-cutting CLI features shared by `fuzz` and `check`: locale handling,
//! oracle tagging + filtering, target/device selection, and cloud-token
//! persistence. Everything pure lives here so it is unit-testable without
//! touching a device or the network.
//!
//! The contracts these helpers establish:
//!   - LOCALE: `--locale de,ar,ja` runs the flow once per locale, tagging every
//!     finding with the locale. The locale reaches the runner as
//!     `REPROIT_LOCALE=<loc>` (a dart-define for Flutter, an env var for the
//!     other backends; both keyed `REPROIT_LOCALE`). A separate agent makes the
//!     explorers honor it; this side only emits it + tags findings.
//!   - ORACLE: every finding maps to an oracle category (crash/jank/leak/...).
//!     `--only`/`--no` filter categories; the default is all-on.
//!   - TARGET/DEVICE: `--target` + `--device` select a platform/device; an
//!     interactive numbered picker (parsed from the platform's own device list)
//!     fills them when a TTY is present and they are unset.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Locale
// ---------------------------------------------------------------------------

/// Parse a `--locale de,ar,ja` list into a deduped, order-preserving vector of
/// trimmed, non-empty locale tags. An empty / all-blank input yields an empty
/// vector, which the caller treats as "app default, behavior unchanged".
pub fn parse_locales(raw: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for tok in raw.split(',') {
        let loc = tok.trim();
        if loc.is_empty() {
            continue;
        }
        if seen.insert(loc.to_string()) {
            out.push(loc.to_string());
        }
    }
    out
}

/// The dart-define / env var name that carries the locale to every runner.
pub const LOCALE_ENV: &str = "REPROIT_LOCALE";

/// Tag a finding with the locale it was found under (in place). Pure given the
/// value; the caller owns the locale-loop.
pub fn tag_finding_locale(finding: &mut Value, locale: &str) {
    if let Some(obj) = finding.as_object_mut() {
        obj.insert("locale".to_string(), Value::String(locale.to_string()));
    }
}

/// Given per-locale finding signatures (locale -> set of finding signatures),
/// return the signatures that appear in SOME locale but not ALL of them. These
/// are locale-specific i18n findings (e.g. an overflow only in `de`). When
/// fewer than two locales ran, there is nothing to compare, so the result is
/// empty.
pub fn locale_specific_findings(
    per_locale: &[(String, BTreeSet<String>)],
) -> Vec<(String, Vec<String>)> {
    if per_locale.len() < 2 {
        return Vec::new();
    }
    // Union of all signatures across locales.
    let mut all: BTreeSet<String> = BTreeSet::new();
    for (_, sigs) in per_locale {
        all.extend(sigs.iter().cloned());
    }
    let mut out = Vec::new();
    for sig in all {
        let present: Vec<String> = per_locale
            .iter()
            .filter(|(_, sigs)| sigs.contains(&sig))
            .map(|(loc, _)| loc.clone())
            .collect();
        if present.len() < per_locale.len() {
            out.push((sig, present));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Oracle categories
// ---------------------------------------------------------------------------

/// The oracle categories a finding can belong to (docs/cli.md "Oracles").
/// `--only`/`--no` filter on these. `as_str` is the canonical lowercase tag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Oracle {
    Crash,
    Jank,
    Leak,
    Visual,
    /// Transient render glitch within a single run: a frame that diverges sharply
    /// from both neighbors then resolves (a flash/flicker), detected frame-to-frame
    /// in the repro video. Distinct from `Visual` (cross-run baseline regression).
    Flicker,
    Divergence,
    A11y,
    I18n,
    /// DOM/layout overflow: content clipped or overflowing its container/viewport
    /// (text truncated by `text-overflow`, a child wider than its parent, or a
    /// horizontal scroll appearing). The i18n/long-string/RTL failure class,
    /// detected by the web runner from deterministic structural measurements.
    Overflow,
    /// Broken rendered content: a label showing a stringify/template artifact
    /// ([object Object], a bare undefined/null/NaN, an unrendered {{...}}). A
    /// deterministic DOM/label finding from the web runner, built-in (no custom
    /// invariant needed).
    ContentBug,
    /// Main-thread freeze / no-progress hang: an action that blocked the main
    /// thread past the hang floor. Deterministic, keyed off the Long Tasks trace.
    Hang,
    /// Graph/structural findings (dead-end). Not a docs-listed oracle but a real
    /// finding class, so it gets a stable tag rather than being silently dropped.
    Graph,
}

impl Oracle {
    /// Every oracle category, the single list to iterate. Used by the drift
    /// tests (skills coverage, default-filter) as the source of truth; add a
    /// variant here when you add one above. Test-only today, so gated to avoid a
    /// dead-code warning in the binary build.
    #[cfg(test)]
    pub const ALL: &'static [Oracle] = &[
        Oracle::Crash,
        Oracle::Jank,
        Oracle::Leak,
        Oracle::Visual,
        Oracle::Flicker,
        Oracle::Divergence,
        Oracle::A11y,
        Oracle::I18n,
        Oracle::Overflow,
        Oracle::ContentBug,
        Oracle::Hang,
        Oracle::Graph,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Oracle::Crash => "crash",
            Oracle::Jank => "jank",
            Oracle::Leak => "leak",
            Oracle::Visual => "visual",
            Oracle::Flicker => "flicker",
            Oracle::Divergence => "divergence",
            Oracle::A11y => "a11y",
            Oracle::I18n => "i18n",
            Oracle::Overflow => "overflow",
            Oracle::ContentBug => "content-bug",
            Oracle::Hang => "hang",
            Oracle::Graph => "graph",
        }
    }

    /// Parse a category name (case-insensitive, with a few aliases) into an
    /// `Oracle`. Unknown names return None so the caller can warn.
    pub fn parse(name: &str) -> Option<Oracle> {
        match name.trim().to_ascii_lowercase().as_str() {
            "crash" | "exception" | "exceptions" => Some(Oracle::Crash),
            "jank" | "perf" | "performance" => Some(Oracle::Jank),
            "leak" | "memory" => Some(Oracle::Leak),
            "visual" => Some(Oracle::Visual),
            "flicker" | "flash" => Some(Oracle::Flicker),
            "divergence" | "diverge" | "diff" => Some(Oracle::Divergence),
            "a11y" | "accessibility" => Some(Oracle::A11y),
            "i18n" | "intl" | "locale" => Some(Oracle::I18n),
            "overflow" | "clip" | "clipped" => Some(Oracle::Overflow),
            "content-bug" | "content" | "contentbug" | "broken-render" => Some(Oracle::ContentBug),
            "hang" | "freeze" | "frozen" | "no-progress" => Some(Oracle::Hang),
            "graph" | "dead-end" | "deadend" => Some(Oracle::Graph),
            _ => None,
        }
    }
}

/// Classify a finding Value into an oracle category, mapping from the finding's
/// `invariant` id (preferred) or its `kind`. The invariant/kind taxonomy
/// already exists (modes/fuzz.rs, model/invariants.rs); this is the single
/// mapping from that taxonomy to the user-facing oracle categories.
pub fn classify(finding: &Value) -> Oracle {
    let invariant = finding
        .get("invariant")
        .and_then(Value::as_str)
        .unwrap_or("");
    match invariant {
        "no-exception" => return Oracle::Crash,
        "no-jank" => return Oracle::Jank,
        "no-leak" => return Oracle::Leak,
        "all-labeled" => return Oracle::A11y,
        "rerender-flicker" | "paint-flicker" => return Oracle::Flicker,
        "no-overflow" => return Oracle::Overflow,
        "no-broken-render" => return Oracle::ContentBug,
        "no-hang" => return Oracle::Hang,
        "no-dead-end" => return Oracle::Graph,
        _ => {}
    }
    let kind = finding.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind.to_ascii_uppercase().as_str() {
        "PERF" => Oracle::Jank,
        "LEAK" => Oracle::Leak,
        "SEMANTICS" => Oracle::A11y,
        "GRAPH" => Oracle::Graph,
        "VISUAL" => Oracle::Visual,
        "FLICKER" => Oracle::Flicker,
        "DIVERGENCE" => Oracle::Divergence,
        "I18N" => Oracle::I18n,
        "OVERFLOW" => Oracle::Overflow,
        "CONTENTBUG" => Oracle::ContentBug,
        "HANG" => Oracle::Hang,
        // An uncaught exception block ("EXCEPTION CAUGHT BY ...") and anything
        // unrecognized fall back to crash, the broadest class.
        _ => Oracle::Crash,
    }
}

/// An oracle include/exclude filter built from `--only` / `--no`. Default
/// (neither set) is "all on". `--only` restricts to the listed set; `--no`
/// removes the listed set. When both are given, `--only` is applied first then
/// `--no` subtracts, so `--only crash,jank --no jank` == `crash`.
#[derive(Clone, Debug)]
pub struct OracleFilter {
    only: Option<BTreeSet<&'static str>>,
    no: BTreeSet<&'static str>,
}

impl OracleFilter {
    /// Build from the raw comma-separated `--only`/`--no` strings. Unknown
    /// category names are returned (second tuple element) so the caller can warn
    /// without failing the run.
    pub fn build(only: Option<&str>, no: Option<&str>) -> (OracleFilter, Vec<String>) {
        let mut unknown = Vec::new();
        let parse_set = |raw: &str, unknown: &mut Vec<String>| -> BTreeSet<&'static str> {
            let mut set = BTreeSet::new();
            for tok in raw.split(',') {
                let t = tok.trim();
                if t.is_empty() {
                    continue;
                }
                match Oracle::parse(t) {
                    Some(o) => {
                        set.insert(o.as_str());
                    }
                    None => unknown.push(t.to_string()),
                }
            }
            set
        };
        let only_set = only.map(|s| parse_set(s, &mut unknown));
        let no_set = no.map(|s| parse_set(s, &mut unknown)).unwrap_or_default();
        (
            OracleFilter {
                only: only_set,
                no: no_set,
            },
            unknown,
        )
    }

    /// All categories on, nothing filtered (the default when no flags are set).
    pub fn all() -> Self {
        OracleFilter {
            only: None,
            no: BTreeSet::new(),
        }
    }

    /// Whether a given oracle category passes the filter.
    pub fn allows(&self, oracle: Oracle) -> bool {
        let tag = oracle.as_str();
        if let Some(only) = &self.only {
            if !only.contains(tag) {
                return false;
            }
        }
        !self.no.contains(tag)
    }

    /// Partition findings into (kept, dropped) by the filter, tagging every KEPT
    /// finding with its `oracle` category (in place). Dropped findings are
    /// returned untagged so the caller can count/report them.
    pub fn apply(&self, findings: Vec<Value>) -> (Vec<Value>, Vec<Value>) {
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for mut f in findings {
            let oracle = classify(&f);
            if self.allows(oracle) {
                if let Some(obj) = f.as_object_mut() {
                    obj.insert(
                        "oracle".to_string(),
                        Value::String(oracle.as_str().to_string()),
                    );
                }
                kept.push(f);
            } else {
                dropped.push(f);
            }
        }
        (kept, dropped)
    }
}

// ---------------------------------------------------------------------------
// Target / device selection
// ---------------------------------------------------------------------------

/// A normalized target platform from `--target`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Ios,
    Android,
    Web,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Ios => "ios",
            Target::Android => "android",
            Target::Web => "web",
        }
    }

    pub fn parse(name: &str) -> Option<Target> {
        match name.trim().to_ascii_lowercase().as_str() {
            "ios" | "iphone" | "ipad" => Some(Target::Ios),
            "android" => Some(Target::Android),
            "web" | "chromium" | "chrome" | "firefox" | "webkit" | "safari" => Some(Target::Web),
            _ => None,
        }
    }
}

/// Expand a `--target a,b,all` string into a concrete list of platform targets.
/// `all` expands to ios+android+web. Unknown tokens are returned for a warning.
/// Duplicates are removed, order preserved. (Platform-only; the unified
/// dispatch uses `parse_run_targets`, which also handles web engines.)
#[allow(dead_code)]
/// The device target(s) a project platform can run on, used to filter the
/// interactive device picker to what the project actually supports. The platform
/// string ("flutter-ios-sim", "web-playwright", "rn-android", ...) names its
/// target; a bare mobile framework with no target word covers both phones.
/// Desktop/host platforms (winui, electron, ...) yield none, so the picker is
/// not filtered (those run on the host, not a device).
pub fn platform_targets(platform: &str) -> Vec<Target> {
    let p = platform.to_ascii_lowercase();
    let mut out = Vec::new();
    if p.contains("ios") || p.contains("iphone") || p.contains("ipad") {
        out.push(Target::Ios);
    }
    if p.contains("android") {
        out.push(Target::Android);
    }
    if p.contains("web") || p.contains("chromium") || p.contains("playwright") {
        out.push(Target::Web);
    }
    // A mobile framework named with no explicit target word runs on both phones.
    if out.is_empty() && (p.contains("flutter") || p.contains("react") || p.starts_with("rn")) {
        out.push(Target::Ios);
        out.push(Target::Android);
    }
    out
}

pub fn parse_targets(raw: &str) -> (Vec<Target>, Vec<String>) {
    let mut out = Vec::new();
    let mut unknown = Vec::new();
    let push = |t: Target, out: &mut Vec<Target>| {
        if !out.contains(&t) {
            out.push(t);
        }
    };
    for tok in raw.split(',') {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        if t.eq_ignore_ascii_case("all") {
            push(Target::Ios, &mut out);
            push(Target::Android, &mut out);
            push(Target::Web, &mut out);
            continue;
        }
        match Target::parse(t) {
            Some(target) => push(target, &mut out),
            None => unknown.push(t.to_string()),
        }
    }
    (out, unknown)
}

/// Whether a `--target` token (or whole list) names web browser engines, so the
/// dispatcher routes to the cross-engine path rather than the platform path. A
/// list is web-engine iff EVERY non-empty token is an engine alias.
pub fn is_web_engine_token(tok: &str) -> bool {
    matches!(
        tok.trim().to_ascii_lowercase().as_str(),
        "chromium" | "chrome" | "blink" | "firefox" | "gecko" | "webkit" | "safari"
    )
}

/// Normalize an engine alias to its canonical Playwright engine name
/// (chromium/firefox/webkit), the value the web runner reads from
/// `REPROIT_ENGINE`. Returns None for non-engine tokens.
pub fn canonical_engine(tok: &str) -> Option<&'static str> {
    match tok.trim().to_ascii_lowercase().as_str() {
        "chromium" | "chrome" | "blink" => Some("chromium"),
        "firefox" | "gecko" => Some("firefox"),
        "webkit" | "safari" => Some("webkit"),
        _ => None,
    }
}

/// One concrete thing a `--target` run dispatches to: either a platform
/// (ios/android/web, resolved to a device) or a specific web browser engine
/// (chromium/firefox/webkit, run through the web backend with REPROIT_ENGINE
/// set). Unifying these means ONE routing+divergence path handles both web
/// engines and platforms.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RunTarget {
    /// A platform target resolved to a device per simctl/adb/flutter.
    Platform(Target),
    /// A specific web browser engine for the cross-engine differential. The
    /// String is the canonical engine name (chromium/firefox/webkit).
    Engine(String),
}

impl RunTarget {
    /// The stable per-target label used in progress output AND as the divergence
    /// key (so a finding "only on firefox" reads naturally).
    pub fn label(&self) -> String {
        match self {
            RunTarget::Platform(t) => t.as_str().to_string(),
            RunTarget::Engine(e) => e.clone(),
        }
    }
}

/// Resolve a `--target` string into the concrete run targets the dispatcher
/// loops over, plus any unknown tokens (for a warning). The list routes ONE of
/// two ways, never mixed:
///   - If every token is a web ENGINE (chromium/firefox/webkit), the result is
///     a list of `Engine` targets: the cross-engine differential. This is the
///     validated runtime case.
///   - Otherwise it is a PLATFORM list (ios/android/web/all), each resolved to
///     a device. A bare `web` here means "the web platform" (one run), distinct
///     from naming specific engines.
/// Duplicates are removed, order preserved.
pub fn parse_run_targets(raw: &str) -> (Vec<RunTarget>, Vec<String>) {
    let tokens: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    // Web-engine routing iff EVERY token is an engine name (bare `web` is a
    // platform token, so `--target web` stays the platform path). At least one
    // token, all engines -> cross-engine.
    let all_engines = !tokens.is_empty() && tokens.iter().all(|t| is_web_engine_token(t));
    let mut out: Vec<RunTarget> = Vec::new();
    let mut unknown = Vec::new();
    let push = |rt: RunTarget, out: &mut Vec<RunTarget>| {
        if !out.contains(&rt) {
            out.push(rt);
        }
    };
    if all_engines {
        for t in tokens {
            match canonical_engine(t) {
                Some(e) => push(RunTarget::Engine(e.to_string()), &mut out),
                None => unknown.push(t.to_string()),
            }
        }
        return (out, unknown);
    }
    // Platform routing. `all` expands to ios+android+web.
    for t in tokens {
        if t.eq_ignore_ascii_case("all") {
            push(RunTarget::Platform(Target::Ios), &mut out);
            push(RunTarget::Platform(Target::Android), &mut out);
            push(RunTarget::Platform(Target::Web), &mut out);
            continue;
        }
        match Target::parse(t) {
            Some(target) => push(RunTarget::Platform(target), &mut out),
            None => unknown.push(t.to_string()),
        }
    }
    (out, unknown)
}

/// Cross-target divergence: given per-target finding signatures, return the
/// signatures that reproduce on a SUBSET of the run targets (some but not all).
/// A finding on EVERY target is consistent behavior, not divergence; a finding
/// on a strict subset is a divergence (a crash on firefox but not chromium, an
/// overflow on android but not ios). With fewer than two targets there is
/// nothing to compare, so the result is empty.
///
/// The returned vec is `(signature, targets_it_reproduced_on)`, sorted by
/// signature for stable output. This is the same "present in some but not all"
/// shape as the locale i18n diff, but kept as its own named function so the
/// cross-TARGET semantics (and its tests) are explicit rather than borrowed.
pub fn cross_target_divergence(
    per_target: &[(String, BTreeSet<String>)],
) -> Vec<(String, Vec<String>)> {
    if per_target.len() < 2 {
        return Vec::new();
    }
    let mut all: BTreeSet<String> = BTreeSet::new();
    for (_, sigs) in per_target {
        all.extend(sigs.iter().cloned());
    }
    let mut out = Vec::new();
    for sig in all {
        let present: Vec<String> = per_target
            .iter()
            .filter(|(_, sigs)| sigs.contains(&sig))
            .map(|(label, _)| label.clone())
            .collect();
        if present.len() < per_target.len() {
            out.push((sig, present));
        }
    }
    out
}

/// One device entry shown in the interactive picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub id: String,
    pub target: Target,
    pub booted: bool,
}

/// Parse `flutter devices --machine` JSON into device entries. The shape is a
/// JSON array of objects with `name`, `id`, and `targetPlatform` (e.g.
/// `ios`, `android-arm64`, `web-javascript`). Best-effort: malformed input
/// yields an empty list.
pub fn parse_flutter_devices(json: &str) -> Vec<Device> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|d| {
            let name = d.get("name").and_then(Value::as_str)?.to_string();
            let id = d.get("id").and_then(Value::as_str)?.to_string();
            let plat = d
                .get("targetPlatform")
                .and_then(Value::as_str)
                .unwrap_or("");
            let target = if plat.starts_with("ios") {
                Target::Ios
            } else if plat.starts_with("android") {
                Target::Android
            } else if plat.contains("web") {
                Target::Web
            } else {
                return None;
            };
            Some(Device {
                name,
                id,
                target,
                booted: true,
            })
        })
        .collect()
}

/// Parse `xcrun simctl list devices` plain-text output into iOS device entries.
/// Lines under a runtime header look like `    iPhone 16 (UDID) (Booted)`;
/// unavailable devices (`(unavailable, ...)`) are skipped.
pub fn parse_simctl_devices(text: &str) -> Vec<Device> {
    let re = regex::Regex::new(
        r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}",
    )
    .unwrap();
    let mut out = Vec::new();
    // simctl groups devices under runtime headers ("-- iOS 18.0 --", "-- tvOS
    // ... --", "-- watchOS ... --", "-- visionOS ... --"). Only iOS-runtime
    // devices (iPhone/iPad) are valid app-fuzzing targets; skip the TV / Watch /
    // Vision form factors so the picker is not a wall of irrelevant sims.
    let mut in_ios = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("--") {
            in_ios = trimmed.starts_with("-- iOS");
            continue;
        }
        // Skip the "== Devices ==" banner, blanks, and any non-iOS runtime.
        if trimmed.is_empty() || trimmed.starts_with("==") || !in_ios {
            continue;
        }
        if trimmed.contains("(unavailable") {
            continue;
        }
        let Some(m) = re.find(trimmed) else {
            continue;
        };
        let udid = m.as_str().to_string();
        let name = trimmed[..trimmed.find(" (").unwrap_or(trimmed.len())]
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        out.push(Device {
            name,
            id: udid,
            target: Target::Ios,
            booted: trimmed.contains("(Booted)"),
        });
    }
    out
}

/// Parse `adb devices` plain-text output into Android device entries. The
/// format is a header line `List of devices attached` then `serial\tstate`
/// lines; only `device`-state entries are returned (offline/unauthorized are
/// skipped).
pub fn parse_adb_devices(text: &str) -> Vec<Device> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("List of devices") || line.starts_with('*') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(serial), Some(state)) = (parts.next(), parts.next()) else {
            continue;
        };
        if state != "device" {
            continue;
        }
        out.push(Device {
            name: serial.to_string(),
            id: serial.to_string(),
            target: Target::Android,
            booted: true,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Cloud token persistence
// ---------------------------------------------------------------------------

/// The path the cloud service token is persisted to: `~/.reproit/token`.
/// Falls back to `.reproit/token` under cwd when there is no home directory.
pub fn token_path() -> PathBuf {
    if let Some(home) = home_dir() {
        home.join(".reproit").join("token")
    } else {
        PathBuf::from(".reproit").join("token")
    }
}

/// Best-effort home directory from `$HOME` (unix) / `$USERPROFILE` (windows).
/// Avoids a new dependency for one lookup.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Persist a cloud service token (+ base URL) to `path`. Written as JSON so the
/// URL travels with the token. Creates parent dirs as needed. On unix the file
/// is written 0600 (it holds a credential).
pub fn save_token(path: &std::path::Path, token: &str, url: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "token": token,
        "url": url,
    }))?;
    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Load a persisted token (token, url) from `path`. Returns None when the file
/// is absent or unparseable.
pub fn load_token(path: &std::path::Path) -> Option<(String, Option<String>)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let token = v.get("token").and_then(Value::as_str)?.to_string();
    let url = v
        .get("url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Some((token, url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_locales_dedupes_trims_and_drops_blanks() {
        assert_eq!(parse_locales("de, ar ,ja"), vec!["de", "ar", "ja"]);
        assert_eq!(parse_locales("de,de,ar"), vec!["de", "ar"]);
        assert!(parse_locales("").is_empty());
        assert!(parse_locales("  , ,").is_empty());
    }

    #[test]
    fn tag_finding_locale_sets_the_field() {
        let mut f = json!({ "kind": "PERF" });
        tag_finding_locale(&mut f, "ar");
        assert_eq!(f["locale"], "ar");
    }

    #[test]
    fn locale_specific_findings_flags_partial_presence() {
        let de: BTreeSet<String> = ["overflow", "shared"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ar: BTreeSet<String> = ["shared"].iter().map(|s| s.to_string()).collect();
        let out = locale_specific_findings(&[("de".into(), de), ("ar".into(), ar)]);
        // "overflow" appears in de only -> locale-specific; "shared" is in both.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "overflow");
        assert_eq!(out[0].1, vec!["de".to_string()]);
    }

    #[test]
    fn locale_specific_findings_needs_two_locales() {
        let de: BTreeSet<String> = ["a"].iter().map(|s| s.to_string()).collect();
        assert!(locale_specific_findings(&[("de".into(), de)]).is_empty());
    }

    #[test]
    fn classify_maps_invariants_and_kinds_to_oracles() {
        assert_eq!(
            classify(&json!({ "invariant": "no-exception" })),
            Oracle::Crash
        );
        assert_eq!(classify(&json!({ "invariant": "no-jank" })), Oracle::Jank);
        assert_eq!(classify(&json!({ "invariant": "no-leak" })), Oracle::Leak);
        assert_eq!(
            classify(&json!({ "invariant": "all-labeled" })),
            Oracle::A11y
        );
        assert_eq!(
            classify(&json!({ "invariant": "no-dead-end" })),
            Oracle::Graph
        );
        assert_eq!(
            classify(&json!({ "invariant": "no-overflow" })),
            Oracle::Overflow
        );
        assert_eq!(classify(&json!({ "kind": "OVERFLOW" })), Oracle::Overflow);
        assert_eq!(
            classify(&json!({ "invariant": "no-broken-render" })),
            Oracle::ContentBug
        );
        assert_eq!(
            classify(&json!({ "kind": "CONTENTBUG" })),
            Oracle::ContentBug
        );
        assert_eq!(classify(&json!({ "invariant": "no-hang" })), Oracle::Hang);
        assert_eq!(classify(&json!({ "kind": "HANG" })), Oracle::Hang);
        // The web jank path reuses the no-jank invariant -> jank category.
        assert_eq!(classify(&json!({ "invariant": "no-jank" })), Oracle::Jank);
        // The new categories parse from their --only/--no names + aliases.
        assert_eq!(Oracle::parse("content-bug"), Some(Oracle::ContentBug));
        assert_eq!(Oracle::parse("content"), Some(Oracle::ContentBug));
        assert_eq!(Oracle::parse("hang"), Some(Oracle::Hang));
        assert_eq!(Oracle::parse("freeze"), Some(Oracle::Hang));
        assert_eq!(classify(&json!({ "kind": "PERF" })), Oracle::Jank);
        // Raw exception block: falls back to crash.
        assert_eq!(
            classify(&json!({ "kind": "EXCEPTION CAUGHT BY WIDGETS LIBRARY" })),
            Oracle::Crash
        );
    }

    #[test]
    fn oracle_filter_default_allows_everything() {
        let f = OracleFilter::all();
        for &o in Oracle::ALL {
            assert!(f.allows(o));
        }
    }

    #[test]
    fn oracle_filter_only_restricts() {
        let (f, unknown) = OracleFilter::build(Some("crash,jank"), None);
        assert!(unknown.is_empty());
        assert!(f.allows(Oracle::Crash));
        assert!(f.allows(Oracle::Jank));
        assert!(!f.allows(Oracle::Leak));
        assert!(!f.allows(Oracle::A11y));
    }

    #[test]
    fn oracle_filter_only_and_no_overflow() {
        // `--only overflow` restricts to overflow; `--no overflow` removes it.
        let (only, unknown) = OracleFilter::build(Some("overflow"), None);
        assert!(unknown.is_empty());
        assert!(only.allows(Oracle::Overflow));
        assert!(!only.allows(Oracle::Crash));
        let (no, _) = OracleFilter::build(None, Some("overflow"));
        assert!(!no.allows(Oracle::Overflow));
        assert!(no.allows(Oracle::Crash));
    }

    #[test]
    fn oracle_filter_no_excludes() {
        let (f, _) = OracleFilter::build(None, Some("jank,leak"));
        assert!(f.allows(Oracle::Crash));
        assert!(!f.allows(Oracle::Jank));
        assert!(!f.allows(Oracle::Leak));
    }

    #[test]
    fn oracle_filter_only_then_no_subtracts() {
        let (f, _) = OracleFilter::build(Some("crash,jank"), Some("jank"));
        assert!(f.allows(Oracle::Crash));
        assert!(!f.allows(Oracle::Jank));
    }

    #[test]
    fn oracle_filter_reports_unknown_categories() {
        let (_f, unknown) = OracleFilter::build(Some("crash,bogus"), None);
        assert_eq!(unknown, vec!["bogus".to_string()]);
    }

    #[test]
    fn oracle_filter_apply_tags_kept_and_splits_dropped() {
        let (f, _) = OracleFilter::build(Some("crash"), None);
        let findings = vec![
            json!({ "invariant": "no-exception", "message": "boom" }),
            json!({ "invariant": "no-jank", "message": "janky" }),
        ];
        let (kept, dropped) = f.apply(findings);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["oracle"], "crash");
        assert_eq!(dropped.len(), 1);
        // Dropped findings are NOT tagged.
        assert!(dropped[0].get("oracle").is_none());
    }

    #[test]
    fn parse_targets_expands_all_and_dedupes() {
        let (ts, unknown) = parse_targets("ios,all,web");
        assert_eq!(ts, vec![Target::Ios, Target::Android, Target::Web]);
        assert!(unknown.is_empty());
        let (_, unknown2) = parse_targets("ios,bogus");
        assert_eq!(unknown2, vec!["bogus".to_string()]);
    }

    #[test]
    fn parse_run_targets_routes_engines_vs_platforms() {
        // All-engine list -> the cross-engine differential, canonicalized.
        let (rts, unknown) = parse_run_targets("chromium,firefox,webkit");
        assert!(unknown.is_empty());
        assert_eq!(
            rts,
            vec![
                RunTarget::Engine("chromium".into()),
                RunTarget::Engine("firefox".into()),
                RunTarget::Engine("webkit".into()),
            ]
        );
        // Engine aliases canonicalize (chrome->chromium, safari->webkit).
        let (rts2, _) = parse_run_targets("chrome,safari");
        assert_eq!(
            rts2,
            vec![
                RunTarget::Engine("chromium".into()),
                RunTarget::Engine("webkit".into()),
            ]
        );
    }

    #[test]
    fn parse_run_targets_treats_bare_web_as_platform() {
        // A bare `web` is the WEB PLATFORM (one platform run), not the
        // cross-engine differential. Mixed platform list expands `all`.
        let (rts, _) = parse_run_targets("web");
        assert_eq!(rts, vec![RunTarget::Platform(Target::Web)]);
        let (rts2, _) = parse_run_targets("ios,all,web");
        assert_eq!(
            rts2,
            vec![
                RunTarget::Platform(Target::Ios),
                RunTarget::Platform(Target::Android),
                RunTarget::Platform(Target::Web),
            ]
        );
    }

    #[test]
    fn parse_run_targets_reports_unknown_tokens() {
        // A platform list with a bogus token surfaces it; the rest still parse.
        let (rts, unknown) = parse_run_targets("ios,bogus");
        assert_eq!(rts, vec![RunTarget::Platform(Target::Ios)]);
        assert_eq!(unknown, vec!["bogus".to_string()]);
    }

    #[test]
    fn cross_target_divergence_flags_subset_findings() {
        // crash:X reproduces on chromium+firefox but NOT webkit -> divergence.
        // crash:Y reproduces on all three -> consistent, NOT divergence.
        let chromium: BTreeSet<String> = ["crash:X", "crash:Y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let firefox: BTreeSet<String> = ["crash:X", "crash:Y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let webkit: BTreeSet<String> = ["crash:Y"].iter().map(|s| s.to_string()).collect();
        let div = cross_target_divergence(&[
            ("chromium".into(), chromium),
            ("firefox".into(), firefox),
            ("webkit".into(), webkit),
        ]);
        assert_eq!(div.len(), 1);
        assert_eq!(div[0].0, "crash:X");
        assert_eq!(
            div[0].1,
            vec!["chromium".to_string(), "firefox".to_string()]
        );
    }

    #[test]
    fn cross_target_divergence_same_on_all_is_not_divergence() {
        let a: BTreeSet<String> = ["crash:X"].iter().map(|s| s.to_string()).collect();
        let b: BTreeSet<String> = ["crash:X"].iter().map(|s| s.to_string()).collect();
        assert!(cross_target_divergence(&[("ios".into(), a), ("android".into(), b)]).is_empty());
    }

    #[test]
    fn cross_target_divergence_needs_two_targets() {
        let a: BTreeSet<String> = ["crash:X"].iter().map(|s| s.to_string()).collect();
        assert!(cross_target_divergence(&[("ios".into(), a)]).is_empty());
    }

    #[test]
    fn parse_flutter_devices_reads_machine_json() {
        let json = r#"[
            {"name":"iPhone 16","id":"ABC-123","targetPlatform":"ios"},
            {"name":"Pixel 7","id":"emulator-5554","targetPlatform":"android-arm64"},
            {"name":"Chrome","id":"chrome","targetPlatform":"web-javascript"},
            {"name":"macOS","id":"macos","targetPlatform":"darwin"}
        ]"#;
        let devs = parse_flutter_devices(json);
        // macOS (darwin) is not one of our three targets -> dropped.
        assert_eq!(devs.len(), 3);
        assert_eq!(devs[0].target, Target::Ios);
        assert_eq!(devs[1].target, Target::Android);
        assert_eq!(devs[1].id, "emulator-5554");
        assert_eq!(devs[2].target, Target::Web);
    }

    #[test]
    fn parse_simctl_devices_picks_booted_and_skips_unavailable() {
        let text = "\
== Devices ==
-- iOS 17.0 --
    iPhone 16 (11111111-2222-3333-4444-555555555555) (Booted)
    iPhone SE (66666666-7777-8888-9999-000000000000) (Shutdown)
    Old Phone (AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE) (Shutdown) (unavailable, runtime profile not found)
";
        let devs = parse_simctl_devices(text);
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[0].name, "iPhone 16");
        assert!(devs[0].booted);
        assert_eq!(devs[1].name, "iPhone SE");
        assert!(!devs[1].booted);
        assert!(devs.iter().all(|d| d.target == Target::Ios));
    }

    #[test]
    fn parse_simctl_devices_skips_tv_watch_vision_runtimes() {
        let text = "\
== Devices ==
-- iOS 18.0 --
    iPhone 16 Pro (11111111-2222-3333-4444-555555555555) (Booted)
    iPad Air 11-inch (22222222-3333-4444-5555-666666666666) (Shutdown)
-- tvOS 18.0 --
    Apple TV 4K (33333333-4444-5555-6666-777777777777) (Shutdown)
-- watchOS 11.0 --
    Apple Watch Series 10 (44444444-5555-6666-7777-888888888888) (Shutdown)
-- visionOS 2.0 --
    Apple Vision Pro (55555555-6666-7777-8888-999999999999) (Shutdown)
";
        let devs = parse_simctl_devices(text);
        // Only the iOS-runtime iPhone + iPad survive; TV / Watch / Vision are out.
        assert_eq!(devs.len(), 2);
        assert!(devs.iter().any(|d| d.name == "iPhone 16 Pro"));
        assert!(devs.iter().any(|d| d.name == "iPad Air 11-inch"));
        assert!(devs.iter().all(|d| !d.name.contains("Apple")));
    }

    #[test]
    fn platform_targets_cover_every_framework() {
        assert_eq!(platform_targets("flutter-ios-sim"), vec![Target::Ios]);
        assert_eq!(platform_targets("web-playwright"), vec![Target::Web]);
        assert_eq!(platform_targets("swift-ios"), vec![Target::Ios]);
        assert_eq!(platform_targets("android"), vec![Target::Android]);
        assert_eq!(
            platform_targets("rn-appium"),
            vec![Target::Ios, Target::Android]
        );
        // Desktop / host frameworks have no device target (they run on the host).
        for p in ["winui", "electron", "tauri", "swift-macos"] {
            assert!(platform_targets(p).is_empty(), "{p} has no device target");
        }
    }

    #[test]
    fn parse_adb_devices_keeps_only_ready_devices() {
        let text = "\
List of devices attached
emulator-5554\tdevice
ZX1G\toffline
RF8N\tunauthorized
ZY2H\tdevice
";
        let devs = parse_adb_devices(text);
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[0].id, "emulator-5554");
        assert_eq!(devs[1].id, "ZY2H");
        assert!(devs.iter().all(|d| d.target == Target::Android));
    }

    #[test]
    fn token_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("reproit-tok-{}", std::process::id()));
        let path = dir.join("token");
        save_token(&path, "sk-test-123", "http://cloud.example").unwrap();
        let (tok, url) = load_token(&path).expect("loads");
        assert_eq!(tok, "sk-test-123");
        assert_eq!(url.as_deref(), Some("http://cloud.example"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_token_missing_file_is_none() {
        let path = std::env::temp_dir().join("reproit-nonexistent-token-xyz");
        let _ = std::fs::remove_file(&path);
        assert!(load_token(&path).is_none());
    }
}
