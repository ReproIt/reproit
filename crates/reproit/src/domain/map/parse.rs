//! Parsing of runner marker streams into normalized map observations.

mod contracts;
mod structure;

use crate::domain::appmap::{OperabilityGap, OperabilityGaps, StateElement, StateText};
use crate::domain::overflow::OverflowCheck;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One leaking metric for a route: `(kind, first, last)` -- the metric name
/// (`listeners` = live event listeners, `nodes` = attached DOM nodes) and its
/// count on the first and last revisit sample. A named alias keeps the
/// `listener_leaks` map under clippy's type-complexity threshold.
pub(crate) type LeakMetric = (String, i64, i64);
pub(crate) type EscapableRoutes = std::sync::Arc<BTreeMap<String, BTreeSet<BTreeSet<String>>>>;

/// One observed violation of an application-declared structural relationship.
/// Geometry is diagnostic only; stable finding identity is the relationship
/// kind, dependent, owner, container, and violation class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelationViolation {
    pub kind: String,
    pub dependent_key: String,
    pub owner_key: String,
    pub container_key: String,
    pub violation: String,
    pub max_gap: i64,
    pub gap_centipx: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelationCheck {
    pub kind: String,
    pub dependent_key: String,
    pub owner_key: String,
    pub container_key: String,
    pub outcome: String,
    pub violation: Option<String>,
}

/// One authoritative native-control versus accessibility-tree state check.
/// `fingerprint` is the stable hash of `(identity, property)` and deliberately
/// excludes observed values so the same subject can be recognized after a fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccessibilityStateCheck {
    pub identity: String,
    pub property: String,
    pub fingerprint: String,
    pub expected: String,
    pub actual: Option<String>,
    pub outcome: String,
    pub reason: Option<String>,
}

/// One run's observations, keyed by semantics signature.
pub(crate) struct RunObs {
    /// sig -> display labels
    pub states: BTreeMap<String, Vec<String>>,
    /// sig -> route/page identity, when the runner reports one.
    /// Framework-neutral: any runner that puts `"route"` in its
    /// EXPLORE:STATE record (the Flutter route anchor, the web URL path,
    /// ...) gets it merged, so the candidate map can reconcile by route
    /// instead of by a name that may not line up.
    pub routes: BTreeMap<String, String>,
    /// sig -> number of tappable elements the runner offered on that state (the
    /// `EXPLORE:STATE` `elements` count). Lets permission-walk tell a proven
    /// trap (no actions, or all actions tried with no forward exit) from a page
    /// the walk simply never finished exploring (it offered tappables it never
    /// tapped). 0 when the runner does not report elements.
    pub tappables: BTreeMap<String, usize>,
    /// sig -> actionable elements the runner offered on that state. Labels are
    /// display-only; `sel` is the replayable structural selector.
    pub elements: BTreeMap<String, Vec<StateElement>>,
    /// sig -> observed screen text regions. These are not replay selectors;
    /// they help importers translate text-based source tests into
    /// structural actions.
    pub texts: BTreeMap<String, Vec<StateText>>,
    /// (from sig, action string e.g. "tap:X"/"back", to sig)
    pub edges: Vec<(String, String, String)>,
    /// First state observed: the app's start state.
    pub start: Option<String>,
    /// route -> the label sets of states on that route that have a forward
    /// (non-back) exit in the AGGREGATE map, folded in by the caller (the
    /// per-seed graph is too sparse on its own). Permission-walk treats a
    /// state on such a route as escapable ONLY when its labels are a subset
    /// of an escapable sibling's (same-or-reduced render of the same page =
    /// animation churn); a structurally DISTINCT screen sharing the URL (a
    /// section toggle with no route change) shows labels the escapable page
    /// lacks, so it remains a trap. Empty unless a caller populates it
    /// (parse_run leaves it empty).
    pub escapable_route_labels: EscapableRoutes,
    /// sig -> operability/accessibility gaps, from `EXPLORE:GROUNDTRUTH`
    /// records (the graph-1-minus-graph-2 diff). Empty for runners that
    /// don't emit it.
    pub gaps: BTreeMap<String, OperabilityGaps>,
    /// (from sig, action) -> the persistent anchors that were torn down and
    /// rebuilt during that transition, from `EXPLORE:RERENDER` records (the
    /// legacy re-render diagnostic). DOM identity churn is not proof of a
    /// visible flicker and is retained only as telemetry. Empty for
    /// runners/transitions that don't emit it.
    pub rerenders: BTreeMap<(String, String), Vec<String>>,
    /// (from sig, action) -> peak transient-divergence magnitude, from the
    /// gated `EXPLORE:FLICKER` records (Tier-2 pixel oracle,
    /// REPROIT_FLICKER_PIXELS). A frame that diverged from both endpoints
    /// mid-transition then settled. Empty unless the pixel oracle is
    /// enabled.
    pub paint_flickers: BTreeMap<(String, String), f64>,
    /// sig -> rendered broken-content artifacts in that state, from
    /// `EXPLORE:CONTENTBUG` records (the content-bug oracle). Each entry is
    /// `(key, reason, text)`: the offending node's stable key, the artifact
    /// class (`object-object`/`undefined`/`null`/`nan`/
    /// `unrendered-template`), and the clipped visible text. Pure DOM/label
    /// scan, so it re-confirms on replay; empty for runners/states that
    /// render no broken content.
    pub content_bugs: BTreeMap<String, Vec<(String, String, String)>>,
    /// sig -> zero-contrast invisible runs in that state, from
    /// `EXPLORE:ZEROCONTRAST` records. Each entry is `(key, text, color)`:
    /// the run's stable `pos:R,C` key, the invisible text, and the shared
    /// resolved RGB both colors collapsed to. Pure attribute equality on the
    /// settled cell grid, so it re-confirms on replay.
    pub zero_contrast: BTreeMap<String, Vec<(String, String, String)>>,
    /// sig -> dead-input probes in that state, from `EXPLORE:DEADINPUT`
    /// records. Each entry is `(key, input, context)`: the probed element's
    /// stable key, the injected input (`key:a` / `wheel:down`), and the
    /// element description. The runner controls the input and observed the
    /// whole event pipeline, so the probe re-confirms on replay.
    pub dead_inputs: BTreeMap<String, Vec<(String, String, String)>>,
    /// sig -> bounded layout containment checks. VIOLATION and SATISFIED are
    /// retained for exact replay; ABSTAIN is retained so unavailable evidence
    /// can never be mistaken for a fix.
    pub overflow_checks: BTreeMap<String, Vec<OverflowCheck>>,
    /// sig -> explicit structural relationship violations. Web currently emits
    /// only `indicator-anchor` records from `data-reproit-*` ownership
    /// contracts. Missing or ambiguous contracts are ABSTAIN upstream and
    /// never appear.
    pub relations: BTreeMap<String, Vec<RelationViolation>>,
    /// sig -> every explicitly resolved relationship evaluation, including
    /// SATISFIED checks. ABSTAIN/absent identities let replay abstain instead
    /// of calling a removed or ambiguous contract fixed.
    pub relation_checks: BTreeMap<String, Vec<RelationCheck>>,
    /// sig -> every accessibility-state parity evaluation, including
    /// SATISFIED checks. ABSTAIN checks are retained without becoming findings,
    /// so replay never mistakes unavailable evidence for a verified fix.
    pub accessibility_state_checks: BTreeMap<String, Vec<AccessibilityStateCheck>>,
    /// sig -> interactive elements whose center is covered by a foreign element
    /// in that state, from `EXPLORE:OCCLUSION` records (the occlusion
    /// oracle). Each entry is `(target, cover)`: the blocked control and
    /// the element on top of it. Pure hit-test (elementFromPoint),
    /// deterministic given a fixed viewport; empty when nothing is
    /// occluded.
    pub occlusions: BTreeMap<String, Vec<(String, String)>>,
    /// sig -> client-side security-hygiene smells in that state, from
    /// `EXPLORE:SECURITY` records. Each entry is `(kind, target)`: the smell
    /// (`tabnabbing`/`insecure-form`/`mixed-content`) and the offending
    /// URL/link. Pure DOM/URL predicates, deterministic and
    /// false-positive-free; empty when the page is clean.
    pub security: BTreeMap<String, Vec<(String, String)>>,
    /// sig -> authoritative blank-screen record for that state. Each item is
    /// `(key, w, h)`: the scanned root plus viewport size. Structural
    /// emptiness without enumerated independent authority is discarded.
    pub blank_screens: BTreeMap<String, Vec<(String, i64, i64)>>,
    /// sig -> dead subresources rendered in that state, from
    /// `EXPLORE:BROKENASSET` records (the broken-asset oracle). Each entry
    /// is `(key, reason, detail)`: the offending node's stable key, the
    /// asset class (`img` for an image that completed with zero natural
    /// width, `font` for a FontFace whose load errored, `tofu` for a
    /// visible U+FFFD replacement character), and the src/family/text
    /// detail. Pure DOM/resource status facts, so they re-confirm on
    /// replay; empty for runners/states with no dead asset.
    pub broken_assets: BTreeMap<String, Vec<(String, String, String)>>,
    /// sig -> zoom-reflow breaks on that route, from `EXPLORE:ZOOMREFLOW`
    /// records (the WCAG 1.4.10 Reflow oracle): the runner re-rendered the
    /// route at half the viewport's CSS size (the reflow-equivalent of 200%
    /// zoom) and the content broke. Each entry is `(key, kind, by)`: the
    /// offending node's stable key, the break class (`hscroll` for a document
    /// that now requires two-dimensional scrolling, `collapsed` for a
    /// previously visible tappable whose hit rect collapsed below 1px), and
    /// the px magnitude. Pure layout measurement at a fixed zoomed viewport,
    /// so it re-confirms on replay; empty for runners/routes that reflow.
    pub zoom_reflows: BTreeMap<String, Vec<(String, String, i64)>>,
    /// sig -> scroll round-trip violations in that state, from
    /// `EXPLORE:SCROLLROUNDTRIP` records (the list-recycling / virtualization
    /// oracle): after scrolling a list away and back, the content at a pinned
    /// offset differed. Each entry is `(pos, before, after)`: the pinned scroll
    /// offset label and the normalized content signature before vs after the
    /// round-trip (value-state normalized out). A structural content
    /// comparison, so it re-confirms on replay; empty for runners/states
    /// whose lists are stable or that cannot scroll.
    pub scroll_round_trips: BTreeMap<String, Vec<(String, String, String)>>,
    /// sig -> `(expected, got)` structural signatures for a ROTATION-stability
    /// violation, from `EXPLORE:ROTATION` records: the explorer rotated the
    /// surface (portrait <-> landscape / split-screen) and then rotated BACK to
    /// the original orientation, but the screen did not rebuild the same
    /// structure (`expected` was the pre-rotation structure, `got` is what
    /// survived the round-trip). Round-trip identity (same orientation in and
    /// out) with value-state excluded, so it is deterministic and
    /// false-positive-free; empty when every screen survives rotation.
    pub rotation_losses: BTreeMap<String, (String, String)>,
    /// sig -> `(expected, got)` structural signatures for a BACKGROUND-RESTORE
    /// violation, from `EXPLORE:BGRESTORE` records: the explorer sent the app
    /// to the background (paused/hidden) and restored it (resumed/visible),
    /// but the app came back to a DIFFERENT structure (`expected` was the
    /// pre-background structure, `got` is what the restore produced). No
    /// size change and value-state excluded, so it is deterministic and
    /// false-positive-free; empty when every screen survives the lifecycle.
    pub background_losses: BTreeMap<String, (String, String)>,
    /// sigs where the soft keyboard was visible while NO text input was
    /// focused, from `EXPLORE:STUCKKEYBOARD` records (the stuck-keyboard
    /// oracle, native mobile explorers only). Ground truth is the platform
    /// IME state plus the focus tree -- keyboard visible <=> an editable
    /// focused -- so it is deterministic and false-positive-free; empty
    /// when every screen is clean.
    pub stuck_keyboards: BTreeSet<String>,
    /// (from sig, action) -> `(bucket, unit)` of a main-thread JANK stall on
    /// that transition, from `EXPLORE:JANK` records. `bucket` is the coarse
    /// magnitude and `unit` names what it measures ("ms" on the web
    /// Long-Tasks tier; a runner without frame timing may report e.g. "pct"
    /// janky frames), so the message never claims milliseconds for a non-ms
    /// metric. Empty unless an action janked.
    pub janks: BTreeMap<(String, String), (i64, String)>,
    /// (from sig, action) -> `(method, url, count)` of a duplicate submit on
    /// that transition, from `EXPLORE:DUPSUBMIT` records: the runner's opt-in
    /// double-dispatch probe (REPROIT_DUPSUBMIT=1) tapped a submit-like control
    /// twice within ~150ms and the SAME first-party non-GET request fired
    /// `count` times -- the handler has no double-activation guard. The probe
    /// skips a control whose first click navigated, so a legit navigation never
    /// lands here. Empty unless the probe ran and a handler double-fired.
    pub duplicate_submits: BTreeMap<(String, String), (String, String, i64)>,
    /// (from sig, action) pairs where a non-navigating tap dropped keyboard
    /// focus to document.body while the tapped control still exists, from
    /// `EXPLORE:FOCUSLOSS` records (the focus-loss oracle): the interaction's
    /// re-render stole focus, so a keyboard user loses their place. The runner
    /// suppresses the false positives upstream (dialog/popover open or close,
    /// a control removed by its own re-render, link/anchor taps). Empty unless
    /// a tap was observed to drop focus.
    pub focus_losses: BTreeSet<(String, String)>,
    /// (from sig, action) -> `(bucket, unit)` of a main-thread HANG/freeze on
    /// that transition, from `EXPLORE:HANG` records (the same watchdog at a
    /// higher floor). `unit` is "ms" on the web tier, but e.g. "keypresses"
    /// on the TUI (a PTY has no frame clock, so the floor is a count of
    /// ignored inputs). Empty unless an action froze the UI past the hang
    /// floor.
    pub hangs: BTreeMap<(String, String), (i64, String)>,
    /// Choice-anomaly findings, from `EXPLORE:CHOICEBUG` records (the
    /// component-choice differential oracle): one entry per multi-choice
    /// component whose options behave UNIFORMLY except one outlier. Each is
    /// `(from sig, role, outlier_label, sel, magnitude_px)`: the state, the
    /// choice role (tab/radio/...), the option that deviated, its selector, and
    /// how many px of global layout it moved. Empty unless a component has an
    /// odd-one-out choice.
    pub choice_bugs: Vec<(String, String, String, String, i64)>,
    /// Broken-route findings, from `EXPLORE:BROKENROUTE` records: a state whose
    /// document responded with a dead-link status (404/410/5xx; the runner
    /// excludes auth-gate 401/403 and rate-limit 429). Each is `(sig, route,
    /// status, from)`, where `from` is the SOURCE state sig that linked to the
    /// dead route (the runner records it at the tap), so the clip attributes
    /// the dead link to the exact page instead of reverse-matching by
    /// destination. `from` is None for a route reached without an in-app
    /// navigation (start URL). Empty unless a visited URL came back broken.
    pub broken_routes: Vec<(String, String, i64, Option<String>)>,
    /// sig -> app-registered invariant violations in that state, from
    /// `EXPLORE:INVARIANT` records. The app declared these predicates via the
    /// SDK (`ReproIt.invariant("id", fn)`); the SDK evaluated them on a
    /// state-settle under the fuzzer and reported the ones that failed. Each
    /// entry is `(id, message)`: the invariant's name and the SDK's failure
    /// detail. The app owns the ground truth, so it re-confirms on replay;
    /// empty when the app registered none or all held.
    pub app_invariants: BTreeMap<String, Vec<(String, String)>>,
    /// route -> a listener/DOM-node leak across REPEATED visits, from
    /// `EXPLORE:LISTENERLEAK` records (the web/electron runners' opt-in revisit
    /// probe, REPROIT_LISTENERLEAK=1). The value is `(visits, items)`: how many
    /// times the route was re-entered, and one `(kind, first, last)` per
    /// leaking metric (`listeners` = live event listeners after
    /// adds-removes; `nodes` = document.getElementsByTagName('*').length),
    /// where `first`/`last` are the counts on the first and last revisit.
    /// The probe only records a metric that climbed MONOTONICALLY across
    /// every revisit above a slope floor, so a stable app (flat after
    /// warmup) is never here. Sequence/repeat-dependent, so it is
    /// a fuzz/soak signal, not a state-present one. Empty unless a route
    /// leaked.
    pub listener_leaks: BTreeMap<String, (i64, Vec<LeakMetric>)>,
    /// sig -> wakelocks that were acquired on that screen and remained HELD
    /// after the user navigated away from it, from `EXPLORE:WAKELOCK`
    /// records (the wakelock-leak oracle, Android/Appium explorer only).
    /// Each entry is `(tag, kind)`: the wakelock tag (or `KEEP_SCREEN_ON`
    /// for a leaked window keep-screen-on flag) and its kind (`wakelock` /
    /// `keep-screen-on`). Ground truth is `dumpsys power` compared before
    /// vs after leaving the screen; the runner excludes app-global/baseline
    /// and released locks and attributes each leak to its origin screen
    /// exactly once, so it is deterministic and false-positive-free. Empty
    /// for runners/screens that release cleanly.
    pub wakelock_leaks: BTreeMap<String, Vec<(String, String)>>,
    /// sig -> safe-area collisions in that state, from `EXPLORE:SAFEAREA`
    /// records (the safe-area oracle, native mobile explorers only): an
    /// interactive control whose hit rect intersects a device inset. Each
    /// entry is `(key, edge, by)`: the control's stable key, which inset it
    /// overlaps (`top`/`bottom`/`left`/`right`), and the overlap depth in
    /// logical px. Pure inset-vs-rect geometry (no pixels, no timing), so
    /// it re-confirms on replay; empty for runners/states with no control
    /// in an inset.
    pub safe_areas: BTreeMap<String, Vec<(String, String, i64)>>,
    /// sig -> the runtime permission whose denial reached that screen, from
    /// `EXPLORE:PERMISSIONWALK` records (the permission-walk oracle, emitted
    /// only under a permission-denial environment sweep). Marks a screen as
    /// reached AFTER a denial; the invariant fires only for the marked
    /// screens that are ALSO trapped, attributing the result to the denied
    /// permission. Empty outside a denial sweep.
    pub permission_screens: BTreeMap<String, String>,
}

/// Compute a state's operability gaps from an `EXPLORE:GROUNDTRUTH` element
/// list. Each element carries `operable` (graph 1) and an `a11y` object with
/// `inTabOrder`/`keyboardActivatable`/`rolePresent`; a gap is a ground-truth-
/// operable element that fails an accessibility dimension. Pure +
/// deterministic.
fn gaps_from_groundtruth(json: &Value) -> OperabilityGaps {
    let mut g = OperabilityGaps {
        focus_trap: json
            .get("focusTrap")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    };
    let Some(els) = json.get("elements").and_then(Value::as_array) else {
        return g;
    };
    for el in els {
        if !el.get("operable").and_then(Value::as_bool).unwrap_or(false) {
            continue; // not ground-truth operable -> not a gap candidate
        }
        let a = el.get("a11y");
        let get = |k: &str| a.and_then(|a| a.get(k)).and_then(Value::as_bool);
        // Default the a11y dims to "true" when unreported, so a missing field is
        // never counted as a gap (conservative: only count confirmed failures).
        let mut kinds: Vec<String> = Vec::new();
        if !get("keyboardActivatable").unwrap_or(true) {
            g.pointer_only += 1;
            kinds.push("pointer_only".into());
        }
        if !get("inTabOrder").unwrap_or(true) {
            g.keyboard_unreachable += 1;
            kinds.push("keyboard_unreachable".into());
        }
        if !get("rolePresent").unwrap_or(true) {
            g.no_role += 1;
            kinds.push("no_role".into());
        }
        // Keep the grounded per-element detail: which selector failed which
        // dimension(s), so the diff is actionable, not just a tally.
        if !kinds.is_empty() {
            let selector = el
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            g.items.push(OperabilityGap { selector, kinds });
        }
    }
    g
}

fn parse_bounds(v: Option<&Value>) -> Option<[i64; 4]> {
    let arr = v?.as_array()?;
    if arr.len() != 4 {
        return None;
    }
    let mut out = [0i64; 4];
    for (i, value) in arr.iter().enumerate() {
        out[i] = value
            .as_i64()
            .or_else(|| value.as_f64().map(|n| n.round() as i64))?;
    }
    if out[2] <= 0 || out[3] <= 0 {
        return None;
    }
    Some(out)
}

pub(crate) fn parse_run(log: &str) -> RunObs {
    let events = crate::domain::runner::parse(log);
    parse_runner_events(&events)
}

pub(crate) fn parse_runner_events(events: &[crate::domain::runner::RunnerEvent<'_>]) -> RunObs {
    let mut obs = RunObs {
        states: BTreeMap::new(),
        routes: BTreeMap::new(),
        tappables: BTreeMap::new(),
        elements: BTreeMap::new(),
        texts: BTreeMap::new(),
        edges: Vec::new(),
        start: None,
        escapable_route_labels: EscapableRoutes::default(),
        gaps: BTreeMap::new(),
        rerenders: BTreeMap::new(),
        paint_flickers: BTreeMap::new(),
        content_bugs: BTreeMap::new(),
        zero_contrast: BTreeMap::new(),
        dead_inputs: BTreeMap::new(),
        overflow_checks: BTreeMap::new(),
        relations: BTreeMap::new(),
        relation_checks: BTreeMap::new(),
        accessibility_state_checks: BTreeMap::new(),
        occlusions: BTreeMap::new(),
        security: BTreeMap::new(),
        blank_screens: BTreeMap::new(),
        broken_assets: BTreeMap::new(),
        zoom_reflows: BTreeMap::new(),
        scroll_round_trips: BTreeMap::new(),
        rotation_losses: BTreeMap::new(),
        background_losses: BTreeMap::new(),
        stuck_keyboards: BTreeSet::new(),
        janks: BTreeMap::new(),
        duplicate_submits: BTreeMap::new(),
        focus_losses: BTreeSet::new(),
        hangs: BTreeMap::new(),
        choice_bugs: Vec::new(),
        broken_routes: Vec::new(),
        app_invariants: BTreeMap::new(),
        listener_leaks: BTreeMap::new(),
        wakelock_leaks: BTreeMap::new(),
        safe_areas: BTreeMap::new(),
        permission_screens: BTreeMap::new(),
    };
    // These records define permission-trap reachability. If one is recognized
    // but malformed, a partial graph could turn missing evidence into a false
    // trap. Abstain from all graph invariants for that segment instead.
    let structural_markers = ["EXPLORE:STATE ", "EXPLORE:EDGE ", "EXPLORE:PERMISSIONWALK "];
    let malformed_structure = events.iter().any(|event| {
        let crate::domain::runner::RunnerEvent::Explore(line) = *event else {
            return false;
        };
        structural_markers.iter().any(|marker| {
            line.strip_prefix(marker)
                .is_some_and(|payload| serde_json::from_str::<Value>(payload).is_err())
        })
    });
    if malformed_structure {
        return obs;
    }
    for event in events {
        let crate::domain::runner::RunnerEvent::Explore(line) = *event else {
            continue;
        };
        if structure::absorb(&mut obs, line) || contracts::absorb(&mut obs, line) {
            continue;
        }
        if let Some(json) = extract(line, "EXPLORE:OVERFLOW ") {
            if let Some(sig) = json.get("sig").and_then(Value::as_str) {
                let checks = crate::domain::overflow::evaluate_marker(&json);
                if !checks.is_empty() {
                    obs.overflow_checks.insert(sig.to_string(), checks);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:CONTENTBUG ") {
            // Broken rendered content for a state: labels carrying a stringify/
            // template artifact ([object Object], undefined/null/NaN, an
            // unrendered {{...}}). Keyed by signature (last write wins); each item
            // is (key, reason, text), the grounded detail.
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let key = it.get("key").and_then(Value::as_str)?.to_string();
                        let reason = it.get("reason").and_then(Value::as_str)?.to_string();
                        let text = it
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        Some((key, reason, text))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.content_bugs.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:ZEROCONTRAST ") {
            // Zero-contrast invisible runs for a state. Keyed by signature
            // (last write wins); each item is (key, text, color).
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let key = it.get("key").and_then(Value::as_str)?.to_string();
                        let text = it.get("text").and_then(Value::as_str)?.to_string();
                        let color = it
                            .get("color")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        Some((key, text, color))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.zero_contrast.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:DEADINPUT ") {
            // Dead-input probes for a state. Keyed by signature (last write
            // wins); each item is (key, input, context).
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let key = it.get("key").and_then(Value::as_str)?.to_string();
                        let input = it.get("input").and_then(Value::as_str)?.to_string();
                        let context = it
                            .get("context")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        Some((key, input, context))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.dead_inputs.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:SECURITY ") {
            // Client-side security-hygiene smells for a state. Keyed by signature;
            // each item is `(kind, target)`.
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let kind = it.get("kind").and_then(Value::as_str)?.to_string();
                        let target = it
                            .get("target")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        Some((kind, target))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.security.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:BLANKSCREEN ") {
            // Structural emptiness is only a candidate: intentional blank
            // fixtures and failed mounts are visually indistinguishable. Accept
            // a reportable marker only when the runner supplies independent,
            // enumerated authority. Legacy and native structural-only markers
            // abstain instead of minting a false-positive finding.
            let authoritative = matches!(
                json.get("authority").and_then(Value::as_str),
                Some(
                    "first-party-exception"
                        | "renderer-crash"
                        | "authored-expectation"
                        | "verified-regression"
                )
            );
            if !authoritative {
                continue;
            }
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, i64, i64)> = items
                    .iter()
                    .filter_map(|it| {
                        let key = it.get("key").and_then(Value::as_str)?.to_string();
                        let w = it.get("w").and_then(Value::as_i64)?;
                        let h = it.get("h").and_then(Value::as_i64)?;
                        Some((key, w, h))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.blank_screens.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:INVARIANT ") {
            // App-registered invariant violations in a state: the SDK evaluated
            // the app's own `ReproIt.invariant("id", fn)` predicates on a
            // state-settle under the fuzzer and reported the ones that failed.
            // Keyed by signature (last write wins); each item is `(id, message)`.
            // Only non-empty item lists are recorded (the SDK is silent when
            // every registered invariant held).
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let id = it.get("id").and_then(Value::as_str)?.to_string();
                        let message = it
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        Some((id, message))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.app_invariants.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:BROKENASSET ") {
            // Dead critical subresources in a state: visibly broken images,
            // rendered tofu, and failed or MIME-blocked same-origin stylesheets
            // and application scripts. Keyed by signature (last write wins); each item is
            // `(key, reason, detail)`. Only non-empty item lists are recorded
            // (the runner is silent when every asset is healthy).
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let key = it.get("key").and_then(Value::as_str)?.to_string();
                        let reason = it.get("reason").and_then(Value::as_str)?.to_string();
                        let detail = it
                            .get("detail")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        Some((key, reason, detail))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.broken_assets.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:ZOOMREFLOW ") {
            // Zoom-reflow breaks on a route: at half the viewport's CSS size
            // (the reflow-equivalent of 200% zoom) the document requires
            // two-dimensional scrolling or a previously visible tappable's hit
            // rect collapsed. Keyed by signature (last write wins); each item
            // is `(key, kind, by)`. Only non-empty item lists are recorded
            // (the runner is silent when the route reflows cleanly).
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String, i64)> = items
                    .iter()
                    .filter_map(|it| {
                        let key = it.get("key").and_then(Value::as_str)?.to_string();
                        let kind = it.get("kind").and_then(Value::as_str)?.to_string();
                        let by = it.get("by").and_then(Value::as_i64).unwrap_or(0);
                        Some((key, kind, by))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.zoom_reflows.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:SCROLLROUNDTRIP ") {
            // Scroll round-trip violations in a state: after scrolling a list
            // away and back, the content at a pinned offset differed. Keyed by
            // signature (last write wins); each item is `(pos, before, after)`.
            // Only non-empty item lists are recorded (the runner is silent when
            // the list's content is stable across the round-trip).
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let pos = it.get("pos").and_then(Value::as_str)?.to_string();
                        let before = it.get("before").and_then(Value::as_str)?.to_string();
                        let after = it.get("after").and_then(Value::as_str)?.to_string();
                        Some((pos, before, after))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.scroll_round_trips.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:ROTATION ") {
            // A state that did not survive a rotation round-trip: the explorer
            // rotated the surface and rotated back, but the pre-rotation
            // structure (`expected`) was not rebuilt (`got`). Keyed by the
            // pre-rotation state sig (last write wins).
            if let (Some(sig), Some(expected), Some(got)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("expected").and_then(Value::as_str),
                json.get("got").and_then(Value::as_str),
            ) {
                obs.rotation_losses
                    .insert(sig.to_string(), (expected.to_string(), got.to_string()));
            }
        } else if let Some(json) = extract(line, "EXPLORE:BGRESTORE ") {
            // A state that did not survive the background -> foreground
            // lifecycle: after pausing and resuming the app, the pre-background
            // structure (`expected`) was replaced by a different one (`got`).
            // Keyed by the pre-background state sig (last write wins).
            if let (Some(sig), Some(expected), Some(got)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("expected").and_then(Value::as_str),
                json.get("got").and_then(Value::as_str),
            ) {
                obs.background_losses
                    .insert(sig.to_string(), (expected.to_string(), got.to_string()));
            }
        } else if let Some(json) = extract(line, "EXPLORE:STUCKKEYBOARD ") {
            // The soft keyboard was visible in this state with no text input
            // focused (stuck-keyboard oracle, native mobile explorers). The
            // explorer only emits on a violation, so presence of the sig is the
            // whole record.
            if let Some(sig) = json.get("sig").and_then(Value::as_str) {
                obs.stuck_keyboards.insert(sig.to_string());
            }
        } else if let Some(json) = extract(line, "EXPLORE:SAFEAREA ") {
            // Interactive controls whose hit rect intersects a device safe-area
            // inset in this state (the safe-area oracle, native mobile explorers).
            // Keyed by signature (last write wins); each item is `(key, edge, by)`,
            // the control, which inset (top/bottom/left/right), and the overlap
            // depth in logical px. Only non-empty item lists are recorded (the
            // runner is silent when no control sits in an inset).
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String, i64)> = items
                    .iter()
                    .filter_map(|it| {
                        let key = it.get("key").and_then(Value::as_str)?.to_string();
                        let edge = it.get("edge").and_then(Value::as_str)?.to_string();
                        let by = it.get("by").and_then(Value::as_i64)?;
                        Some((key, edge, by))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.safe_areas.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:PERMISSIONWALK ") {
            // A screen reached AFTER a runtime-permission denial, from the
            // permission-denial environment sweep. Keyed by signature (last write
            // wins); the value is the denied permission label. The invariant fires
            // only for a marked sig that is ALSO a graph dead end, so a screen with
            // a working exit is recorded here but never flagged.
            if let (Some(sig), Some(perm)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("permission").and_then(Value::as_str),
            ) {
                obs.permission_screens
                    .insert(sig.to_string(), perm.to_string());
            }
        } else if let Some(json) = extract(line, "EXPLORE:OCCLUSION ") {
            // Interactive elements whose center is covered by a foreign element
            // (the occlusion oracle). Keyed by signature; each item is
            // `(target, cover)`.
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let target = it.get("target").and_then(Value::as_str)?.to_string();
                        let cover = it
                            .get("cover")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        Some((target, cover))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.occlusions.insert(sig.to_string(), parsed);
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:DUPSUBMIT ") {
            // A duplicate submit: the opt-in double-dispatch probe tapped a
            // submit-like control twice and the SAME (method, url) first-party
            // non-GET request fired `count` times. Keyed by (from, action), so
            // each control is reported once; a record missing any field is
            // dropped (the runner always emits all five).
            if let (Some(from), Some(action), Some(method), Some(url), Some(count)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
                json.get("method").and_then(Value::as_str),
                json.get("url").and_then(Value::as_str),
                json.get("count").and_then(Value::as_i64),
            ) {
                obs.duplicate_submits.insert(
                    (from.to_string(), action.to_string()),
                    (method.to_string(), url.to_string(), count),
                );
            }
        } else if let Some(json) = extract(line, "EXPLORE:FOCUSLOSS ") {
            // A non-navigating tap that dropped keyboard focus to <body> while
            // the tapped control still exists (focus-loss oracle). Keyed by
            // (from, action), so a re-observed drop dedupes.
            if let (Some(from), Some(action)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
            ) {
                obs.focus_losses
                    .insert((from.to_string(), action.to_string()));
            }
        } else if let Some(json) = extract(line, "EXPLORE:JANK ") {
            // A main-thread JANK stall on a transition (Long Tasks trace). Keyed by
            // (from, action); the value is the coarse blocked-time bucket (ms).
            if let (Some(from), Some(action)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
            ) {
                let bucket = json.get("bucket").and_then(Value::as_i64).unwrap_or(0);
                let unit = parse_metric_unit(&json);
                obs.janks
                    .insert((from.to_string(), action.to_string()), (bucket, unit));
            }
        } else if let Some(json) = extract(line, "EXPLORE:HANG ") {
            // A main-thread HANG/freeze on a transition (Long Tasks trace, higher
            // floor). Keyed by (from, action); value is the blocked-time bucket.
            if let (Some(from), Some(action)) = (
                json.get("from").and_then(Value::as_str),
                json.get("action").and_then(Value::as_str),
            ) {
                let bucket = json.get("bucket").and_then(Value::as_i64).unwrap_or(0);
                let unit = parse_metric_unit(&json);
                obs.hangs
                    .insert((from.to_string(), action.to_string()), (bucket, unit));
            }
        } else if let Some(json) = extract(line, "EXPLORE:CHOICEBUG ") {
            // A component-choice outlier: a multi-choice component whose options
            // behave uniformly except one that deviates on the global layout.
            if let (Some(from), Some(role), Some(outlier), Some(sel)) = (
                json.get("from").and_then(Value::as_str),
                json.get("role").and_then(Value::as_str),
                json.get("outlier").and_then(Value::as_str),
                json.get("sel").and_then(Value::as_str),
            ) {
                let mag = json.get("magnitude").and_then(Value::as_i64).unwrap_or(0);
                obs.choice_bugs.push((
                    from.to_string(),
                    role.to_string(),
                    outlier.to_string(),
                    sel.to_string(),
                    mag,
                ));
            }
        } else if let Some(json) = extract(line, "EXPLORE:BROKENROUTE ") {
            // A dead route: the document for this state's URL responded 4xx/5xx.
            if let Some(sig) = json.get("sig").and_then(Value::as_str) {
                let route = json
                    .get("route")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let status = json.get("status").and_then(Value::as_i64).unwrap_or(0);
                let from = json.get("from").and_then(Value::as_str).map(str::to_string);
                obs.broken_routes
                    .push((sig.to_string(), route, status, from));
            }
        } else if let Some(json) = extract(line, "EXPLORE:LISTENERLEAK ") {
            // A listener/DOM-node leak across repeated visits to a route: keyed by
            // route (last write wins). `visits` is the repeat count; each item is
            // `(kind, first, last)` for a metric that climbed monotonically. Only
            // records with a non-empty item list are kept (the runner is silent
            // when a route is stable).
            if let (Some(route), Some(items)) = (
                json.get("route").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let visits = json.get("visits").and_then(Value::as_i64).unwrap_or(0);
                let parsed: Vec<(String, i64, i64)> = items
                    .iter()
                    .filter_map(|it| {
                        let kind = it.get("kind").and_then(Value::as_str)?.to_string();
                        let first = it.get("first").and_then(Value::as_i64)?;
                        let last = it.get("last").and_then(Value::as_i64)?;
                        Some((kind, first, last))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.listener_leaks
                        .insert(route.to_string(), (visits, parsed));
                }
            }
        } else if let Some(json) = extract(line, "EXPLORE:WAKELOCK ") {
            // A wakelock leak: one or more wakelocks acquired on state `sig` were
            // still held after the user navigated away (the Android explorer's
            // dumpsys-power before/after comparison). Keyed by signature (last
            // write wins); each item is `(tag, kind)`. Only emitted on a violation,
            // so a clean release produces no marker.
            if let (Some(sig), Some(items)) = (
                json.get("sig").and_then(Value::as_str),
                json.get("items").and_then(Value::as_array),
            ) {
                let parsed: Vec<(String, String)> = items
                    .iter()
                    .filter_map(|it| {
                        let tag = it.get("tag").and_then(Value::as_str)?.to_string();
                        let kind = it
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or("wakelock")
                            .to_string();
                        Some((tag, kind))
                    })
                    .collect();
                if !parsed.is_empty() {
                    obs.wakelock_leaks.insert(sig.to_string(), parsed);
                }
            }
        }
    }
    obs
}

fn extract(line: &str, marker: &str) -> Option<Value> {
    let payload = line.strip_prefix(marker)?;
    serde_json::from_str(payload.trim()).ok()
}

/// The metric unit on a JANK/HANG marker -- what `bucket` measures. Defaults to
/// "ms" (the web Long-Tasks tier), so a marker without an explicit `unit` keeps
/// the historical millisecond meaning. A runner whose floor is NOT milliseconds
/// (the TUI's ignored-keypress count; an RSS-only tier's janky-frame percent)
/// sets `unit` so the rendered message doesn't claim "ms" for a count/percent.
fn parse_metric_unit(json: &Value) -> String {
    json.get("unit")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("ms")
        .to_string()
}
