use serde_json::Value;
use std::collections::BTreeSet;

/// The oracle categories a finding can belong to (docs/cli.md "Oracles").
/// `--only`/`--no` filter on these. `as_str` is the canonical lowercase tag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Oracle {
    /// A finding whose taxonomy is not recognized by this CLI build. Unknown
    /// findings must never inherit the crash oracle: doing so would turn a
    /// parser/registry drift into a confirmed product bug.
    Unknown,
    Crash,
    Jank,
    Leak,
    Visual,
    /// Transient render glitch within a single run: a frame that diverges
    /// sharply from both neighbors then resolves (a flash/flicker),
    /// detected frame-to-frame in the repro video. Distinct from `Visual`
    /// (cross-run baseline regression).
    Flicker,
    Divergence,
    /// Broken rendered content: a label showing a stringify/template artifact
    /// ([object Object], a bare undefined/null/NaN, an unrendered {{...}}). A
    /// deterministic DOM/label finding from the web runner, built-in (no custom
    /// invariant needed).
    ContentBug,
    /// Main-thread freeze / no-progress hang: an action that blocked the main
    /// thread past the hang floor. Deterministic, keyed off the Long Tasks
    /// trace.
    Hang,
    /// A visible control whose hit target is covered by a foreign element.
    Occlusion,
    /// Explicitly declared indicator no longer stays attached to its declared
    /// owner/container. Missing or ambiguous relationships abstain.
    DetachedIndicator,
    /// Choice-anomaly: one option of a multi-choice component
    /// (tab/radio/select/ button-cluster) shifts the global layout when its
    /// siblings do not. A differential specialist signal from the web
    /// runner. An outlier is not proof that sibling choices were intended
    /// to have identical effects.
    ChoiceAnomaly,
    /// Broken route: the app links to a URL whose document responds 404/410.
    /// Objective HTTP evidence, but not proof that the response was unintended,
    /// so it remains a specialist signal unless backed by a route contract.
    BrokenRoute,
    /// Security hygiene: a pure DOM/URL predicate for a client-side security
    /// smell -- a cross-origin `target=_blank` link without `rel=noopener`
    /// (reverse tabnabbing), an HTTPS page with an `http:` form action or an
    /// `http:` subresource (mixed content). Deterministic specialist hygiene,
    /// not application-intent proof.
    Security,
    /// Stuck soft keyboard: the on-screen keyboard is visible while no text
    /// input is focused, so it covers content the user never asked it to cover
    /// (navigated away from a field and the IME never dismissed). Ground-truth
    /// state observation. Some apps intentionally retain an IME across focus
    /// handoff, so it remains an environment specialist without an app
    /// contract. Native mobile only
    /// (Flutter and Appium explorers); desktop and web have no soft keyboard.
    StuckKeyboard,
    /// Duplicate submit: a submit-like control that, tapped twice in rapid
    /// succession, fires the SAME first-party non-GET request twice -- the
    /// handler has no double-activation guard, so an impatient double click
    /// places the order/payment/post twice. From the web runner's opt-in
    /// double-dispatch probe (REPROIT_DUPSUBMIT=1: double-firing real submits
    /// changes exploration semantics, so a normal walk never does it).
    DuplicateSubmit,
    /// Focus loss: a tap that does not navigate yet leaves
    /// document.activeElement on <body> while the tapped control still
    /// exists -- the interaction's re-render dropped keyboard focus, so a
    /// keyboard user loses their place. The runner excludes common
    /// intentional cases, but cannot prove the app's desired post-action
    /// focus target. Specialist candidate (web only).
    FocusLoss,
    /// Blank screen: a reached state renders ZERO visible text nodes and ZERO
    /// tappable controls in a non-empty viewport -- the white-screen-of-death
    /// (a failed SPA mount, a render error swallowed before paint). Structural
    /// DOM emptiness, not a pixel check, so it is deterministic (web only).
    BlankScreen,
    /// Broken asset: a dead or browser-rejected critical subresource in the
    /// state. Includes visible dead images/tofu and same-origin stylesheet
    /// or application script failures. Deterministic browser/DOM facts (web
    /// only).
    BrokenAsset,
    /// Zoom reflow: a route that breaks at 200% zoom (WCAG 1.4.10 Reflow,
    /// EAA-mandatory). The runner re-renders the route at half the viewport's
    /// CSS size (the reflow-equivalent of 200% zoom) and flags content that
    /// then requires TWO-DIMENSIONAL scrolling (a horizontal scrollbar on a
    /// vertically-scrolling document) or a previously visible tappable whose
    /// hit rect collapses below 1px. Pure layout measurement at a fixed
    /// zoomed viewport, deterministic (web only).
    ZoomReflow,
    /// App-registered invariant: a predicate the app itself declared via the
    /// SDK (`ReproIt.invariant("name", () => bool)`) that must hold in every
    /// visited state. The SDK evaluates its registered predicates on each
    /// state-settle and, only when it detects it is running under the fuzzer,
    /// emits a violation the runner surfaces; the CLI turns it into a finding.
    /// This is the app's own domain rule (a cart total never negative, a
    /// selected tab always highlighted), so it ports to every backend whose SDK
    /// has a state hook. Distinct from the CLI-config `custom` regex rules,
    /// which are declarative predicates written in a reproit config file.
    Invariant,
    /// User-declared structural state or temporal property evaluated over the
    /// normalized cross-platform observation trace.
    Contract,
    /// Rotation-stability: a metamorphic relation across a device-orientation
    /// transform. The explorer rotates the surface (portrait <-> landscape /
    /// split-screen), lets it reflow, then rotates BACK to the original
    /// orientation and re-snapshots. A correct screen reflows but rebuilds the
    /// SAME structure once the original orientation is restored; an app that
    /// mishandles the orientation/resize lifecycle -- dropping content or state
    /// that never comes back -- regresses the structural signature. Round-trip
    /// identity makes this high signal, but an app may intentionally react to
    /// an orientation lifecycle event. It remains an environment candidate
    /// unless the app declares round-trip identity. Value-state is excluded
    /// from the compared signature. Native (Flutter, Appium) + Chromium
    /// (web, electron, tauri) explorers that can drive the transform.
    Rotation,
    /// Background-restore-stability: a metamorphic relation across the app
    /// background -> foreground lifecycle. The explorer sends the app to the
    /// background (paused/hidden) then restores it (resumed/visible) and
    /// re-snapshots. A correct app returns to the SAME screen with its state
    /// intact; one that drops you on a different screen or loses state across
    /// the lifecycle regresses the structural signature. No size change, so a
    /// direct before/after comparison is high signal, but resume may
    /// intentionally lock or redirect. It remains a candidate without an
    /// app contract; value-state is excluded from the compared signature.
    /// Native (Flutter, Appium) + Chromium (web, electron, tauri) explorers
    /// that expose a lifecycle hook.
    BackgroundRestore,
    /// Scroll round-trip: in a scrollable list the content at a pinned offset
    /// is NOT identical after scrolling away and back -- a list-recycling /
    /// virtualization bug rebinds a different row to the same position. A
    /// metamorphic relation across a scroll transform (scroll down then back is
    /// an identity for content at a fixed offset), asserted by the explorers
    /// that can scroll (Flutter, Appium, web). Structural content comparison at
    /// pinned offsets that ignores dynamic value-state. Live/reordered lists
    /// can still change intentionally, so this is an environment candidate.
    ScrollRoundTrip,
    /// Wakelock leak: a wakelock (or a window FLAG_KEEP_SCREEN_ON) acquired on
    /// a screen is STILL held after the user navigates away from that
    /// screen -- a battery drain that keeps the CPU/screen awake off the
    /// video/map/call screen that needed it. Ground truth is `dumpsys
    /// power` (the app-owned held wake locks) plus the focused window's
    /// keep-screen-on flag, sampled before vs after leaving the screen. It
    /// is an environment candidate because an app can intentionally
    /// transfer a lock across screens; a declared ownership contract is
    /// required for confirmation. Android / Appium only: iOS exposes no
    /// public wakelock introspection and web / desktop / TUI have no
    /// wakelock concept.
    WakeLock,
    /// Safe-area collision: an interactive control whose hit rect intersects a
    /// device safe-area inset -- the status bar / notch / Dynamic Island (top),
    /// the home indicator (bottom), or a landscape notch / rounded corner (left
    /// or right) -- so the control is partly obscured or hard to hit. Ground
    /// truth is the platform inset geometry (Flutter MediaQuery.viewPadding /
    /// Appium safe-area insets) versus the control's hit rect. Edge-to-edge
    /// apps can intentionally place controls there, so it remains a
    /// candidate without an authored exclusion/containment contract. Native
    /// mobile only: desktop has no device insets, and the headless web the
    /// runner drives reports every env(safe-area-inset-*) as 0 (no
    /// display-cutout ground truth).
    SafeArea,
    /// Permission dead-end: under a runtime-permission DENIAL sweep, a screen
    /// the app reached after the denial is a genuine graph sink -- a stuck
    /// "please enable X" screen with no working way forward. Uses the
    /// internal sink predicate (the reached screen must itself be trapped)
    /// and attributes the trap to the denied permission, so a team sees the
    /// exact permission that strands the user. Sequence/environment
    /// dependent (it only exists under a denied permission), so it belongs
    /// to the environment sweep, not the single-screen scan crawl. Native
    /// mobile only: Appium denies permissions and Flutter mocks the
    /// permission platform channel; a backend that cannot deny a permission
    /// (web / desktop / TUI) is excluded.
    PermissionWalk,
}

impl Oracle {
    /// Every oracle category, the single list to iterate. Used by the drift
    /// tests (skills coverage, default-filter) as the source of truth; add a
    /// variant here when you add one above. Test-only today, so gated to avoid
    /// a dead-code warning in the binary build.
    #[cfg(test)]
    pub const ALL: &'static [Oracle] = &[
        Oracle::Unknown,
        Oracle::Crash,
        Oracle::Jank,
        Oracle::Leak,
        Oracle::Visual,
        Oracle::Flicker,
        Oracle::Divergence,
        Oracle::ContentBug,
        Oracle::Hang,
        Oracle::Occlusion,
        Oracle::DetachedIndicator,
        Oracle::ChoiceAnomaly,
        Oracle::BrokenRoute,
        Oracle::Security,
        Oracle::StuckKeyboard,
        Oracle::DuplicateSubmit,
        Oracle::FocusLoss,
        Oracle::BlankScreen,
        Oracle::BrokenAsset,
        Oracle::ZoomReflow,
        Oracle::Invariant,
        Oracle::Contract,
        Oracle::Rotation,
        Oracle::BackgroundRestore,
        Oracle::ScrollRoundTrip,
        Oracle::WakeLock,
        Oracle::SafeArea,
        Oracle::PermissionWalk,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Oracle::Unknown => "unknown",
            Oracle::Crash => "crash",
            Oracle::Jank => "jank",
            Oracle::Leak => "leak",
            Oracle::Visual => "visual",
            Oracle::Flicker => "flicker",
            Oracle::Divergence => "divergence",
            Oracle::ContentBug => "content-bug",
            Oracle::Hang => "hang",
            Oracle::Occlusion => "occlusion",
            Oracle::DetachedIndicator => "detached-indicator",
            Oracle::ChoiceAnomaly => "choice-anomaly",
            Oracle::BrokenRoute => "broken-route",
            Oracle::Security => "security",
            Oracle::StuckKeyboard => "stuck-keyboard",
            Oracle::DuplicateSubmit => "duplicate-submit",
            Oracle::FocusLoss => "focus-loss",
            Oracle::BlankScreen => "blank-screen",
            Oracle::BrokenAsset => "broken-asset",
            Oracle::ZoomReflow => "zoom-reflow",
            Oracle::Invariant => "invariant",
            Oracle::Contract => "contract",
            Oracle::Rotation => "rotation",
            Oracle::BackgroundRestore => "background-restore",
            Oracle::ScrollRoundTrip => "scroll-round-trip",
            Oracle::WakeLock => "wakelock",
            Oracle::SafeArea => "safe-area",
            Oracle::PermissionWalk => "permission-walk",
        }
    }

    /// Parse a category name (case-insensitive, with a few aliases) into an
    /// `Oracle`. Unknown names return None so the caller can warn.
    pub fn parse(name: &str) -> Option<Oracle> {
        match name.trim().to_ascii_lowercase().as_str() {
            "unknown" => Some(Oracle::Unknown),
            "crash" | "exception" | "exceptions" => Some(Oracle::Crash),
            "jank" | "perf" | "performance" => Some(Oracle::Jank),
            "leak" | "memory" => Some(Oracle::Leak),
            "visual" => Some(Oracle::Visual),
            "flicker" | "flash" => Some(Oracle::Flicker),
            "divergence" | "diverge" | "diff" => Some(Oracle::Divergence),
            "content-bug" | "content" | "contentbug" | "broken-render" => Some(Oracle::ContentBug),
            "hang" | "freeze" | "frozen" | "no-progress" => Some(Oracle::Hang),
            "occlusion" | "occluded" | "blocked-control" => Some(Oracle::Occlusion),
            "detached-indicator" | "detachedindicator" | "indicator" | "badge" => {
                Some(Oracle::DetachedIndicator)
            }
            "choice-anomaly" | "choice" | "choicebug" | "anomaly" => Some(Oracle::ChoiceAnomaly),
            "broken-route" | "broken-link" | "not-found" | "404" | "deadlink" => {
                Some(Oracle::BrokenRoute)
            }
            "security" | "sec" | "mixed-content" | "tabnabbing" => Some(Oracle::Security),
            "stuck-keyboard" | "keyboard" | "ime" | "soft-keyboard" => Some(Oracle::StuckKeyboard),
            "duplicate-submit" | "dupsubmit" | "double-submit" => Some(Oracle::DuplicateSubmit),
            "focus-loss" | "focusloss" => Some(Oracle::FocusLoss),
            "blank-screen" | "blankscreen" | "white-screen" => Some(Oracle::BlankScreen),
            "broken-asset" | "brokenasset" | "dead-asset" | "tofu" => Some(Oracle::BrokenAsset),
            "zoom-reflow" | "zoomreflow" | "reflow" | "zoom" => Some(Oracle::ZoomReflow),
            "invariant" | "assertion" | "app-invariant" | "custom-invariant" => {
                Some(Oracle::Invariant)
            }
            "contract" | "temporal-contract" | "property" => Some(Oracle::Contract),
            "rotation" | "rotate" | "orientation" | "split-screen" => Some(Oracle::Rotation),
            "background-restore" | "background" | "bg-restore" | "lifecycle" | "backgrounded" => {
                Some(Oracle::BackgroundRestore)
            }
            "scroll-round-trip" | "scrollroundtrip" | "scroll-recycle" | "list-recycle"
            | "recycle" => Some(Oracle::ScrollRoundTrip),
            "wakelock" | "wake-lock" | "wakelocks" | "keep-screen-on" | "battery" => {
                Some(Oracle::WakeLock)
            }
            "safe-area" | "safearea" | "safe-area-inset" | "notch" => Some(Oracle::SafeArea),
            "permission-walk" | "permissionwalk" | "permission-dead-end" | "permission" => {
                Some(Oracle::PermissionWalk)
            }
            _ => None,
        }
    }
}

/// Classify a finding Value into an oracle category, mapping from the finding's
/// `invariant` id (preferred) or its `kind`. The invariant/kind taxonomy
/// already exists (modes/fuzz.rs, model/invariants.rs); this is the single
/// mapping from that taxonomy to the user-facing oracle categories.
pub fn classify(finding: &Value) -> Oracle {
    if matches!(
        finding.get("oracle").and_then(Value::as_str),
        Some("contract" | "backend-contract")
    ) {
        return Oracle::Contract;
    }
    let invariant = finding
        .get("invariant")
        .and_then(Value::as_str)
        .unwrap_or("");
    match invariant {
        "no-exception" => return Oracle::Crash,
        "no-jank" => return Oracle::Jank,
        "no-leak" => return Oracle::Leak,
        // Listener/DOM-node leak across repeated route visits: the same Leak
        // oracle class as the soak/memory signal, just detected structurally.
        "no-listener-leak" => return Oracle::Leak,
        "rerender-flicker" | "paint-flicker" => return Oracle::Flicker,
        "no-broken-render" => return Oracle::ContentBug,
        "no-hang" => return Oracle::Hang,
        "no-occluded-control" => return Oracle::Occlusion,
        "no-detached-indicator" => return Oracle::DetachedIndicator,
        "no-choice-anomaly" => return Oracle::ChoiceAnomaly,
        "no-broken-route" => return Oracle::BrokenRoute,
        "no-stuck-keyboard" => return Oracle::StuckKeyboard,
        "no-duplicate-submit" => return Oracle::DuplicateSubmit,
        "no-focus-loss" => return Oracle::FocusLoss,
        "no-blank-screen" => return Oracle::BlankScreen,
        "no-broken-asset" => return Oracle::BrokenAsset,
        "no-reflow-break" => return Oracle::ZoomReflow,
        "app-invariant" => return Oracle::Invariant,
        "no-rotation-loss" => return Oracle::Rotation,
        "no-background-loss" => return Oracle::BackgroundRestore,
        "no-scroll-recycle" => return Oracle::ScrollRoundTrip,
        "no-wakelock-leak" => return Oracle::WakeLock,
        "no-safe-area-collision" => return Oracle::SafeArea,
        "no-permission-dead-end" => return Oracle::PermissionWalk,
        _ => {}
    }
    let kind = finding.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind.to_ascii_uppercase().as_str() {
        "PERF" => Oracle::Jank,
        "LEAK" => Oracle::Leak,
        "LISTENERLEAK" => Oracle::Leak,
        "OCCLUSION" => Oracle::Occlusion,
        "DETACHEDINDICATOR" => Oracle::DetachedIndicator,
        "VISUAL" => Oracle::Visual,
        "FLICKER" => Oracle::Flicker,
        "DIVERGENCE" => Oracle::Divergence,
        "CONTENTBUG" => Oracle::ContentBug,
        "SECURITY" => Oracle::Security,
        "STUCKKEYBOARD" => Oracle::StuckKeyboard,
        "DUPSUBMIT" => Oracle::DuplicateSubmit,
        "FOCUSLOSS" => Oracle::FocusLoss,
        "BLANKSCREEN" => Oracle::BlankScreen,
        "BROKENASSET" => Oracle::BrokenAsset,
        "ZOOMREFLOW" => Oracle::ZoomReflow,
        // Both the app-registered invariant path and the CLI-config `custom`
        // regex rules emit kind INVARIANT; bucket both here (previously they
        // fell through to Crash).
        "INVARIANT" => Oracle::Invariant,
        "TEMPORAL-CONTRACT" => Oracle::Contract,
        "ROTATION" => Oracle::Rotation,
        "BGRESTORE" => Oracle::BackgroundRestore,
        "SCROLLROUNDTRIP" => Oracle::ScrollRoundTrip,
        "WAKELOCK" => Oracle::WakeLock,
        "SAFEAREA" => Oracle::SafeArea,
        "PERMISSIONWALK" => Oracle::PermissionWalk,
        "HANG" => Oracle::Hang,
        // Raw framework exception blocks predate the named no-exception
        // invariant. They are still objective crashes. Everything else stays
        // UNKNOWN so registry drift cannot masquerade as a confirmed crash.
        value
            if value == "EXCEPTION"
                || value == "CRASH"
                || value == "SIGNAL"
                || value.starts_with("EXCEPTION CAUGHT BY") =>
        {
            Oracle::Crash
        }
        _ => Oracle::Unknown,
    }
}

/// An oracle include/exclude filter built from `--only` / `--no`. Default
/// (neither set) is the stable, high-confidence product set. `--only` can opt
/// into any preview/experimental detector; `--no`
/// removes the listed set. When both are given, `--only` is applied first then
/// `--no` subtracts, so `--only crash,jank --no jank` == `crash`.
#[derive(Clone, Debug)]
pub struct OracleFilter {
    only: Option<BTreeSet<&'static str>>,
    no: BTreeSet<&'static str>,
}

impl OracleFilter {
    /// Build from the raw comma-separated `--only`/`--no` strings. Unknown
    /// category names are returned (second tuple element) so the caller can
    /// warn without failing the run.
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
        let only_set = match only {
            Some(s) => Some(parse_set(s, &mut unknown)),
            None => Some(Self::stable_set()),
        };
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

    /// Default product surface. These detectors describe an objective failure
    /// and have a direct replay predicate. Specialist, timing-sensitive, or
    /// heuristic detectors remain available through `--only` without being
    /// allowed to create noisy default findings.
    pub fn stable() -> Self {
        OracleFilter {
            only: Some(Self::stable_set()),
            no: BTreeSet::new(),
        }
    }

    pub(super) fn stable_set() -> BTreeSet<&'static str> {
        [Oracle::Crash, Oracle::DetachedIndicator, Oracle::Contract]
            .into_iter()
            .map(Oracle::as_str)
            .collect()
    }

    /// Whether a given oracle category passes the filter.
    pub fn allows(&self, oracle: Oracle) -> bool {
        // Unknown taxonomy is telemetry about registry drift, never a bug class.
        // It cannot be opted into, including through the internal all() filter.
        if oracle == Oracle::Unknown {
            return false;
        }
        let tag = oracle.as_str();
        if let Some(only) = &self.only {
            if !only.contains(tag) {
                return false;
            }
        }
        !self.no.contains(tag)
    }

    /// Partition findings into (kept, dropped) by the filter, tagging every
    /// KEPT finding with its `oracle` category (in place). Dropped findings
    /// are returned untagged so the caller can count/report them.
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
                    // Preserve specialist detectors as useful output while
                    // enforcing the confirmation boundary before IDs, shrink,
                    // persistence, or Cloud upload. Repeatability alone cannot
                    // prove application intent.
                    if !Self::stable_set().contains(oracle.as_str()) {
                        obj.insert("advisory".to_string(), Value::Bool(true));
                        obj.insert(
                            "confidence".to_string(),
                            Value::String("candidate".to_string()),
                        );
                    }
                }
                kept.push(f);
            } else {
                dropped.push(f);
            }
        }
        (kept, dropped)
    }
}
