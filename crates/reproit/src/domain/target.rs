use std::collections::BTreeSet;

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
/// interactive device picker to what the project actually supports. The
/// platform string ("flutter", "web", "react-native", ...) names its target.
/// The current Flutter backend provisions an iOS simulator; React Native can
/// run on either phone target through Appium caps.
/// Desktop/host platforms (winui, electron, ...) yield none, so the picker is
/// not filtered (those run on the host, not a device).
pub fn platform_targets(platform: &str) -> Vec<Target> {
    let p = platform.to_ascii_lowercase();
    if p == "flutter" {
        return vec![Target::Ios];
    }
    if p == "react-native" || p == "rn" {
        return vec![Target::Ios, Target::Android];
    }
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
    // Target-qualified mobile framework ids, if we add them later.
    if out.is_empty() && (p.contains("react") || p.starts_with("rn")) {
        out.push(Target::Ios);
        out.push(Target::Android);
    }
    out
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
    /// The stable per-target label used in progress output AND as the
    /// divergence key (so a finding "only on firefox" reads naturally).
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
///     a list of `Engine` targets: the cross-engine differential.
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
