//! The repro identity model and check classification (Phase B core).
//!
//! A *repro* is the single object the CLI revolves around (docs/cli.md): a seed
//! plus an action sequence, addressed by a CONTENT HASH so the same case on two
//! machines lands on the same id (self-deduping), with an optional human alias.
//!
//! This module owns three things:
//!   1. the content-hash id (`repro_id`) over (seed + normalized actions),
//!   2. the on-disk store (`.reproit/repros/<id>/` with `meta.json`),
//!   3. the four-outcome classification (`classify`) pass/fail/flaky/stale and
//!      its mapping to the CI exit-code contract.

use crate::layout;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const FINDING_PREFIX: &str = "fnd_";
pub const REPRO_PREFIX: &str = "rep_";

pub fn display_finding_id(id: &str) -> String {
    prefixed_id(FINDING_PREFIX, id)
}

pub fn display_repro_id(id: &str) -> String {
    prefixed_id(REPRO_PREFIX, id)
}

fn prefixed_id(prefix: &str, id: &str) -> String {
    if id.starts_with(prefix) {
        id.to_string()
    } else {
        format!("{prefix}{id}")
    }
}

pub fn raw_finding_id(id: &str) -> Option<&str> {
    id.strip_prefix(FINDING_PREFIX)
}

pub fn raw_repro_id(id: &str) -> Option<&str> {
    id.strip_prefix(REPRO_PREFIX)
}

/// A saved repro lands quarantined (reported, non-blocking) and auto-promotes
/// to required on its first green, unless `keep --strict`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Reported but non-blocking (a fresh keep, not yet proven green once).
    Quarantined,
    /// Blocks CI on regression (promoted after a first green, or `--strict`).
    Required,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Quarantined => "quarantined",
            Status::Required => "required",
        }
    }
}

/// The persisted `meta.json` for a saved repro.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    /// Content-hash id (12 hex of sha256 over seed + normalized actions).
    pub id: String,
    /// Optional human alias (the friendly name used in `check <alias>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// quarantined | required.
    pub status: Status,
    /// The seed that produced the action sequence.
    pub seed: u64,
    /// RFC3339 creation timestamp.
    pub created: String,
    /// RFC3339 of the last `check`, or None if never checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<String>,
    /// The last check outcome as a string (pass/fail/flaky/stale), or None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
    /// The finding's TRIGGER POINT: the count of actions that must replay
    /// before the original finding fired (i.e. the position of the last
    /// action in the saved, minimized sequence). A replay that performs
    /// this many actions without an earlier miss has REACHED the trigger
    /// context, so a clean run is a real PASS (the fix held) and any miss
    /// AFTER this point is just the fix's downstream effect, not a stale
    /// path. A miss BEFORE this point means the path to the trigger no
    /// longer exists -> STALE. None for older repros kept before this field
    /// existed (handled by the fallback heuristic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_index: Option<usize>,
    /// The state signature that was active when the original finding fired, if
    /// it was recoverable at keep time. Optional companion to
    /// `trigger_index`: when present, reaching this sig in the replay log
    /// also counts as reaching the trigger context. None when the report
    /// carried no sig.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_sig: Option<String>,
    /// Stable structural selector for the exact offending relationship member.
    /// Optional for older and non-relational repros.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_selector: Option<String>,
    /// The ORACLE category the finding belongs to (crash/jank/leak/occlusion/
    /// divergence/i18n), recorded at `keep` so `check` re-confirms the SAME
    /// finding by its oracle rather than only scanning for exceptions. A
    /// crash-class finding (or None, for repros kept before this field existed)
    /// uses the existing exception/process-death logic; a graph-class finding
    /// re-evaluates its invariant over the replay's EXPLORE markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<String>,
    /// Direct web URL used to record the finding without filming discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_url: Option<String>,
    /// Single transition action to replay from `record_url`, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_action: Option<String>,
}

/// The normalized action sequence: trim each action, drop blanks. This is the
/// SAME normalization the id hashes over, so two captures of the same case (one
/// with stray whitespace) produce one id.
pub fn normalize_actions<S: AsRef<str>>(actions: &[S]) -> Vec<String> {
    actions
        .iter()
        .map(|a| a.as_ref().trim().to_string())
        .filter(|a| !a.is_empty())
        .collect()
}

/// The content-hash repro id: 12 hex chars of sha256 over the seed and the
/// normalized action sequence. Stable across machines (no timestamps, no paths,
/// no run dir names enter it) and self-deduping (same case -> same id).
///
/// The hashed preimage is `seed\n` followed by one normalized action per line,
/// so the id is insensitive to surrounding whitespace but sensitive to the
/// action ORDER (reordering is a different case, hence a different repro).
pub fn repro_id<S: AsRef<str>>(seed: u64, actions: &[S]) -> String {
    let norm = normalize_actions(actions);
    let mut hasher = Sha256::new();
    hasher.update(seed.to_string().as_bytes());
    hasher.update(b"\n");
    for a in &norm {
        hasher.update(a.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    let mut s = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Content identity for a discovered finding. A finding includes the target
/// and bug signature because action-only ids collide for state-present bugs
/// that require no actions.
pub fn finding_id<S: AsRef<str>>(
    target: &str,
    signature: &str,
    seed: u64,
    actions: &[S],
) -> String {
    let norm = normalize_actions(actions);
    let mut hasher = Sha256::new();
    hasher.update(b"finding-v2\n");
    hasher.update(target.trim().as_bytes());
    hasher.update(b"\n");
    hasher.update(signature.trim().as_bytes());
    hasher.update(b"\n");
    hasher.update(seed.to_string().as_bytes());
    hasher.update(b"\n");
    for action in &norm {
        hasher.update(action.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The committed repro store directory (`.reproit/repros/`).
pub fn repros_dir(root: &Path) -> PathBuf {
    layout::repros_dir(root)
}

/// One saved repro's store directory (`.reproit/repros/<id>/`).
pub fn repro_dir(root: &Path, id: &str) -> PathBuf {
    layout::repro_dir(root, id)
}

/// Load a repro's meta.json by id (the store dir name).
pub fn load_meta(root: &Path, id: &str) -> Option<Meta> {
    let p = repro_dir(root, id).join("meta.json");
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// Persist a repro's meta.json (creating the store dir if needed).
pub fn save_meta(root: &Path, meta: &Meta) -> Result<()> {
    let dir = repro_dir(root, &meta.id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(meta).context("serializing meta.json")?,
    )
    .context("writing meta.json")
}

/// All saved repros, sorted by id. Reads each store dir's meta.json; dirs
/// without a parseable meta are skipped.
pub fn list(root: &Path) -> Vec<Meta> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(repros_dir(root)) {
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            if let Some(name) = e.file_name().to_str() {
                if let Some(m) = load_meta(root, name) {
                    out.push(m);
                }
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Resolve a repro reference (`rep_...` OR an alias) to its meta.
pub fn resolve(root: &Path, name: &str) -> Option<Meta> {
    if let Some(id) = raw_repro_id(name) {
        if let Some(m) = load_meta(root, id) {
            return Some(m);
        }
    }
    list(root)
        .into_iter()
        .find(|m| m.alias.as_deref() == Some(name))
}

/// The four check outcomes (docs/cli.md). Ordered by SEVERITY so a suite can
/// take the worst with `max`: Pass < Stale < Flaky < Fail.
///
/// Severity rationale: a Fail is a confirmed regression (the hard CI stop), a
/// Flaky is a real non-determinism bug, and a Stale is "couldn't replay, go
/// re-record" (the softest non-clean state). Pass is clean.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Pass,
    Stale,
    Flaky,
    Fail,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Stale => "stale",
            Outcome::Flaky => "flaky",
            Outcome::Fail => "fail",
        }
    }

    /// The CI exit-code contract: 0 clean, 1 regression, 2 flaky, 3 stale.
    pub fn exit_code(self) -> u8 {
        match self {
            Outcome::Pass => 0,
            Outcome::Fail => 1,
            Outcome::Flaky => 2,
            Outcome::Stale => 3,
        }
    }
}

/// The verdict of ONE replay run, before aggregation across the N runs.
///
/// This is the crux of distinguishing flaky from stale. The signals come from
/// the runner's existing log protocol (templates/explorer*.dart):
///   - `Broke`   = the oracle tripped (an exception block fired, or the run
///     reported a FAIL verdict): the actions REPLAYED and the app broke -> a
///     real regression (the original finding reproduced).
///   - `CouldNotReplay` = a `FUZZ:MISS <act>` occurred BEFORE the replay
///     reached the finding's TRIGGER CONTEXT, so the path to the bug no longer
///     exists and the repro could not be meaningfully attempted -> the early UI
///     changed (stale), NOT a failure.
///   - `Green`   = the original finding did NOT fire AND the replay reached the
///     trigger context (it performed the actions up to the trigger index, or
///     hit the trigger sig, before any miss). A miss AFTER the trigger is fine:
///     that is the fix's downstream effect (the button that used to crash now
///     navigates elsewhere), so the repro still PASSES as a green regression
///     guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunVerdict {
    Green,
    Broke,
    CouldNotReplay,
}

impl RunVerdict {
    /// A short per-run label describing whether the ORIGINAL finding reproduced
    /// on this replay. This is the run's repro verdict (consistent with the
    /// final aggregate), as opposed to the raw drive PASS/FAIL (which only says
    /// the drive ran to completion).
    pub fn as_str(self) -> &'static str {
        match self {
            RunVerdict::Broke => "reproduced",
            RunVerdict::Green => "clean",
            RunVerdict::CouldNotReplay => "could not replay",
        }
    }
}

/// The trigger context a repro records at `keep` time, used to decide whether a
/// miss happened before or after the finding's trigger point. `index` is the
/// count of actions that must replay to reach the trigger (the length of the
/// saved minimized sequence); `sig` is the optional state signature active when
/// the finding fired. Either reaching `index` performed-actions or seeing `sig`
/// in the log counts as reaching the trigger context.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trigger {
    pub index: Option<usize>,
    pub sig: Option<String>,
    pub selector: Option<String>,
    /// The oracle category the original finding belonged to (e.g. `crash`,
    /// `graph`). Selects how `check` re-confirms the finding: crash-class uses
    /// the exception/process-death path; graph-class re-evaluates the graph
    /// invariant over the replay's EXPLORE markers. None falls back to the
    /// crash-class path (the historical behavior).
    pub oracle: Option<String>,
}

impl Trigger {
    /// A trigger with no recorded context: forces the fallback heuristic.
    pub fn unknown() -> Self {
        Trigger {
            index: None,
            sig: None,
            selector: None,
            oracle: None,
        }
    }

    /// Whether any trigger context was recorded (vs the fallback heuristic).
    fn is_known(&self) -> bool {
        self.index.is_some() || self.sig.is_some()
    }

    /// Whether this finding is a GRAPH-invariant finding (re-evaluated over the
    /// replay graph rather than scanned for exceptions).
    /// Whether this finding is a re-render FLICKER finding. Like a graph
    /// invariant it does not announce itself with an exception, so it is
    /// re-confirmed by re-evaluating the EXPLORE:RERENDER records over the
    /// replay graph rather than by scanning for a crash.
    fn is_flicker(&self) -> bool {
        self.oracle.as_deref() == Some("flicker")
    }

    /// Whether this finding is a CONTENT-BUG finding (a broken rendered label).
    /// Like overflow it does not throw, so it is re-confirmed by re-evaluating
    /// the EXPLORE:CONTENTBUG records over the replay graph.
    fn is_content_bug(&self) -> bool {
        self.oracle.as_deref() == Some("content-bug")
    }

    fn is_detached_indicator(&self) -> bool {
        self.oracle.as_deref() == Some("detached-indicator")
    }

    /// Whether this finding is a JANK finding (a main-thread stall on a
    /// transition). Re-confirmed by re-evaluating the EXPLORE:JANK records.
    fn is_jank(&self) -> bool {
        self.oracle.as_deref() == Some("jank")
    }

    /// Whether this finding is a HANG/freeze finding (a no-progress main-thread
    /// block). Re-confirmed by re-evaluating the EXPLORE:HANG records.
    fn is_hang(&self) -> bool {
        self.oracle.as_deref() == Some("hang")
    }

    /// A tester explicitly marked the captured structural state as broken. It
    /// is confirmed only when a clean replay reaches that exact state again.
    fn is_tester_capture(&self) -> bool {
        self.oracle.as_deref() == Some("tester-capture")
    }
}

/// Classify a single replay's drive log into a per-run verdict, WITHOUT a
/// recorded trigger context (older repros / callers that have none). Delegates
/// to the trigger-aware path with `Trigger::unknown()`, which applies the
/// fallback heuristic documented on `verdict_from_log_with_trigger`. `check`
/// itself always has a trigger (it loads the repro's meta), so this convenience
/// is exercised by the fallback tests.
#[allow(dead_code)]
pub fn verdict_from_log(log: &str, passed: bool) -> RunVerdict {
    verdict_from_log_with_trigger(log, passed, &Trigger::unknown())
}

/// Classify a single replay's drive log into a per-run verdict, given the
/// finding's trigger context.
///
/// The decision (in order):
///   1. The original finding REPRODUCED (an app exception fired, or the run
///      reported a non-pass verdict) -> Broke. The bug is back; classification
///      never downgrades a live regression to stale on account of a later miss.
///   2. No finding, and the replay REACHED the trigger context (it performed at
///      least `trigger.index` actions before the first miss, or the log carried
///      the `trigger.sig` state) -> Green. A miss AFTER the trigger is the
///      fix's downstream effect, not staleness: the fixed bug's repro stays
///      green.
///   3. No finding, and a miss happened BEFORE reaching the trigger context ->
///      CouldNotReplay (stale): the early path to the bug no longer exists, so
///      the repro could not be meaningfully attempted.
///
/// Fallback heuristic (no trigger context recorded, e.g. an older repro): treat
/// "no finding fired and at least the first action replayed" as Green,
/// reserving CouldNotReplay for a miss on the VERY FIRST action (or a failure
/// to perform any action at all). This keeps a fixed-bug repro green by default
/// and only calls stale when the replay could not even get off the ground.
pub fn verdict_from_log_with_trigger(log: &str, passed: bool, trigger: &Trigger) -> RunVerdict {
    // Hermetic replay is fail-closed. A request absent from the causal capsule
    // means the environment could not be reconstructed; any resulting app error
    // is secondary and must never be reported as the original bug.
    if log.contains("CAPSULE:MISS ") {
        return RunVerdict::CouldNotReplay;
    }
    // No-verdict guard (mirrors the triage `reproduce` guard in modes/triage.rs):
    // a drive that FAILED but produced NO app exception AND NO replay signal at
    // all never actually ran the case -- the runner crashed/timed out or hit a
    // setup error before replaying a single action. Reading that bare exit as a
    // reproduced finding (Broke -> FAIL) is a FALSE regression: the agent would be
    // told the bug is back when nothing ran. Classify it as CouldNotReplay so the
    // check surfaces STALE ("could not run, re-run/re-record"), never an implied
    // verdict. A genuine reproduced crash always carries the exception block (or
    // at least replay progress markers), so it still takes the Broke path below.
    if !passed && !has_app_exception(log) && !has_replay_signal(log) {
        return RunVerdict::CouldNotReplay;
    }

    // A reproduced finding wins outright: a live regression is never reclassified
    // as stale because a downstream action later missed. This holds for EVERY
    // oracle: a crash during any finding replay is still a regression.
    if !passed || has_app_exception(log) {
        return RunVerdict::Broke;
    }

    // FLICKER findings, like graph invariants, do not throw: the replay re-drives
    // the same transition and the runner re-emits EXPLORE:RERENDER iff the wasteful
    // re-render still happens. Re-confirm by re-evaluating those records over the
    // replay graph rather than scanning for an exception.
    if trigger.is_flicker() {
        return flicker_verdict(log, trigger);
    }

    // CONTENT-BUG / JANK / HANG findings, like graph/flicker, do not
    // throw: the replay re-drives to the same state/transition and the runner
    // re-emits the same EXPLORE:CONTENTBUG / EXPLORE:JANK / EXPLORE:HANG marker iff
    // the defect is still present. Re-confirm by re-evaluating those records.
    if trigger.is_content_bug() {
        return content_bug_verdict(log, trigger);
    }
    if trigger.is_detached_indicator() {
        return detached_indicator_verdict(log, trigger);
    }
    if trigger.is_jank() {
        return jank_verdict(log, trigger);
    }
    if trigger.is_hang() {
        return hang_verdict(log, trigger);
    }
    if trigger.is_tester_capture() {
        return tester_capture_verdict(log, trigger);
    }

    // Count actions performed before the first miss, and whether any miss
    // occurred at all, by walking the log in order.
    let mut performed_before_first_miss = 0usize;
    let mut saw_miss = false;
    let mut saw_trigger_sig = false;
    let mut pending_action = false;
    let mut pending_payload = "";
    let want_sig = trigger.sig.as_deref();
    for line in log.lines() {
        if let Some(sig) = want_sig {
            // Reaching the finding's recorded state signature counts as reaching
            // the trigger context regardless of action accounting. Match ONLY the
            // signature carried by an EXPLORE:STATE marker, by EQUALITY -- never an
            // unanchored substring of the whole line. A short hex/token sig can
            // otherwise collide with unrelated earlier content (a selector, route,
            // or marker that happens to contain the token), which would falsely set
            // `saw_trigger_sig` and read a path-moved replay (the trigger state was
            // never reached -> should be stale/re-record) as Green/Pass.
            if !sig.is_empty() && state_sig_matches(line, sig) {
                saw_trigger_sig = true;
            }
        }
        if line.contains("FUZZ:MISS ") {
            let missed = line
                .split_once("FUZZ:MISS ")
                .map(|(_, payload)| payload.trim())
                .unwrap_or("");
            if pending_action && missed != pending_payload {
                performed_before_first_miss += 1;
            }
            saw_miss = true;
            break;
        }
        if line.contains("FUZZ:ACT ") {
            // FUZZ:ACT is an attempt marker. A following FUZZ:MISS means that
            // exact action was not performed. Commit the previous attempt only
            // after progress or the next attempt proves it completed.
            if pending_action {
                performed_before_first_miss += 1;
            }
            pending_action = true;
            pending_payload = line
                .split_once("FUZZ:ACT ")
                .map(|(_, payload)| payload.trim())
                .unwrap_or("");
        } else if pending_action && line.contains("EXPLORE:STATE ") {
            performed_before_first_miss += 1;
            pending_action = false;
        }
    }

    // No miss at all: the saved sequence replayed clean and the oracle stayed
    // quiet -> the bug is fixed (or never fired). Green.
    if !saw_miss {
        return RunVerdict::Green;
    }

    if trigger.is_known() {
        // The trigger sig appeared before any miss -> reached the trigger.
        if saw_trigger_sig {
            return RunVerdict::Green;
        }
        // Reached the trigger by action count: performed all actions up to the
        // trigger index before the first miss. The miss is downstream of the
        // (now-fixed) trigger, so the repro still passes.
        if let Some(idx) = trigger.index {
            if performed_before_first_miss >= idx {
                return RunVerdict::Green;
            }
        }
        // A miss before the trigger context: the path to the bug is gone.
        return RunVerdict::CouldNotReplay;
    }

    // Fallback (no trigger recorded): stale only if the FIRST action missed
    // (nothing replayed); otherwise the partial replay with no finding is a pass.
    if performed_before_first_miss == 0 {
        RunVerdict::CouldNotReplay
    } else {
        RunVerdict::Green
    }
}

/// Re-confirm an exploratory tester capture. The human supplied the bug signal;
/// the engine supplies reproducibility by requiring the captured structural
/// state to be reached again. A different or unreachable state remains pending
/// rather than becoming a confirmed bug.
fn tester_capture_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let Some(sig) = trigger.sig.as_deref().filter(|sig| !sig.is_empty()) else {
        return RunVerdict::CouldNotReplay;
    };
    let required_actions = trigger.index.unwrap_or(0);
    let mut actions_seen = 0usize;
    for line in log.lines() {
        if line.starts_with("FUZZ:ACT ") {
            actions_seen += 1;
            continue;
        }
        if actions_seen >= required_actions && state_sig_matches(line, sig) {
            return RunVerdict::Broke;
        }
    }
    RunVerdict::CouldNotReplay
}

/// Whether an `EXPLORE:STATE` marker line carries the recorded trigger state
/// signature `want`, by EQUALITY of the parsed sig token (never an unanchored
/// substring of the line). Only `EXPLORE:STATE` lines carry a state signature,
/// so any other line is ignored. The marker payload is everything after
/// `EXPLORE:STATE `; it is either a JSON record with a `"sig"` field (the
/// runner's protocol, mirrored by `map::parse_run`) or a bare `SIG:...` token
/// (the recorded sig is then the whole payload). Match the recorded `want`
/// against the JSON `sig` value when present, else the trimmed bare payload.
fn state_sig_matches(line: &str, want: &str) -> bool {
    let Some(idx) = line.find("EXPLORE:STATE ") else {
        return false;
    };
    let payload = line[idx + "EXPLORE:STATE ".len()..].trim();
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) {
        if let Some(sig) = json.get("sig").and_then(serde_json::Value::as_str) {
            return sig == want;
        }
    }
    payload == want
}

/// Whether the replay produced ANY evidence that it actually ran the case: a
/// performed/missed action, a state/edge explore marker, or a drive-completion
/// line. Used by the no-verdict guard to tell a crashed/setup-errored run (a
/// bare non-zero exit with a signal-less log) from a real replay. The markers
/// come from the runner's log protocol (templates/explorer*.dart), the same
/// ones the per-run classifiers below already key on.
fn has_replay_signal(log: &str) -> bool {
    log.lines().any(|line| {
        line.contains("FUZZ:ACT ")
            || line.contains("FUZZ:MISS ")
            || line.contains("EXPLORE:")
            || line.contains("JOURNEY DONE")
            || line.contains("SEED:BEGIN ")
    })
}

/// Whether the log carries an APP exception block (not the test framework's
/// own). Mirrors the fuzz oracle's framework-exclusion so a check agrees with
/// what fuzz would have called a finding.
fn has_app_exception(log: &str) -> bool {
    log.lines().any(|line| {
        line.contains("EXCEPTION CAUGHT BY")
            // The test framework's own exceptions are not app bugs.
            && !line.contains("TEST FRAMEWORK")
    })
}

/// Re-confirm an older flicker finding over a replay log. Parses
/// presented-frame EXPLORE markers and re-evaluates the visual predicate (via
/// `invariants::recheck_rerender_flicker`) against the recorded violating state
/// sig (`trigger.sig`, the transition's FROM state):
///   - the same transition still has a transient divergent frame -> Broke
///   - the sig is reached without one -> Green (fix held)
///   - the sig is never observed in the replay -> CouldNotReplay (re-record).
///
/// With no recorded sig (older flicker repro), fall back to whether ANY flicker
/// remains in the replay graph: any -> Broke, none -> Green, empty graph (no
/// states observed) -> CouldNotReplay.
fn flicker_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::model::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::model::invariants::recheck_rerender_flicker(&obs, sig) {
            crate::model::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::model::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::model::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::model::invariants::any_rerender_flicker(&obs) {
        RunVerdict::Broke
    } else {
        RunVerdict::Green
    }
}

/// Re-confirm a `no-broken-render` (content-bug) finding over a replay log,
/// mirroring `overflow_verdict`: re-evaluate the EXPLORE:CONTENTBUG records
/// against the recorded violating state sig, falling back to "any broken
/// content remains" when no sig was recorded.
fn content_bug_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::model::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::model::invariants::recheck_content_bug(&obs, sig) {
            crate::model::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::model::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::model::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::model::invariants::any_content_bug(&obs) {
        RunVerdict::Broke
    } else {
        RunVerdict::Green
    }
}

fn detached_indicator_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::model::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::model::invariants::recheck_detached_indicator(
            &obs,
            sig,
            trigger.selector.as_deref(),
        ) {
            crate::model::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::model::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::model::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::model::invariants::any_detached_indicator(&obs) {
        RunVerdict::Broke
    } else {
        // Without a recorded state and exact member, silence is UNKNOWN rather
        // than proof that the old relationship became valid.
        RunVerdict::CouldNotReplay
    }
}

/// Re-confirm a `no-jank` (web jank) finding over a replay log. A jank stall is
/// keyed by the transition's FROM state, so re-evaluate the EXPLORE:JANK
/// records against the recorded sig; fall back to "any jank remains" with no
/// sig.
fn jank_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::model::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::model::invariants::recheck_jank(&obs, sig) {
            crate::model::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::model::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::model::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::model::invariants::any_jank(&obs) {
        RunVerdict::Broke
    } else {
        RunVerdict::Green
    }
}

/// Re-confirm a `no-hang` (freeze) finding over a replay log, mirroring
/// `jank_verdict` against the EXPLORE:HANG records.
fn hang_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::model::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::model::invariants::recheck_hang(&obs, sig) {
            crate::model::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::model::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::model::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::model::invariants::any_hang(&obs) {
        RunVerdict::Broke
    } else {
        RunVerdict::Green
    }
}

/// Aggregate the per-run verdicts of an N-times check into one outcome.
///
///   - all Green                         -> Pass
///   - any CouldNotReplay                -> Stale (path to the trigger is gone)
///   - mixed Green/Broke (some of each)  -> Flaky (non-deterministic app)
///   - all Broke                         -> Fail (deterministic regression)
///
/// Stale now means specifically "the replay could not REACH the finding's
/// trigger context" (a miss before the trigger), not "some later action
/// missed". A fixed bug whose fix changes downstream navigation (so a recorded
/// action misses AFTER the trigger) classifies Green per
/// `verdict_from_log_with_trigger` and so stays a required regression guard
/// rather than dropping to stale.
///
/// Stale still takes precedence over a fail mix: if some runs could not even
/// reach the trigger, the right message is "the early path moved, re-record",
/// not "it failed". A run that both reproduced the finding and half-replayed is
/// already Broke per `verdict_from_log_with_trigger` (a live regression wins).
pub fn classify(verdicts: &[RunVerdict]) -> Outcome {
    if verdicts.is_empty() {
        return Outcome::Stale;
    }
    if verdicts.contains(&RunVerdict::CouldNotReplay) {
        return Outcome::Stale;
    }
    let green = verdicts.iter().filter(|v| **v == RunVerdict::Green).count();
    let broke = verdicts.iter().filter(|v| **v == RunVerdict::Broke).count();
    match (green, broke) {
        (_, 0) => Outcome::Pass,
        (0, _) => Outcome::Fail,
        _ => Outcome::Flaky,
    }
}

/// A check result for one repro: the aggregate outcome plus the green rate
/// (e.g. 7/10) so the flaky/pass detail is reportable.
#[derive(Clone, Debug)]
pub struct CheckResult {
    pub outcome: Outcome,
    pub green: usize,
    pub total: usize,
}

impl CheckResult {
    pub fn from_verdicts(verdicts: &[RunVerdict]) -> Self {
        CheckResult {
            outcome: classify(verdicts),
            green: verdicts.iter().filter(|v| **v == RunVerdict::Green).count(),
            total: verdicts.len(),
        }
    }

    /// "7/10" green-over-total rate string.
    pub fn rate(&self) -> String {
        format!("{}/{}", self.green, self.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_and_deterministic() {
        let a = repro_id(7, &["tap:Login", "type:user", "tap:Submit"]);
        let b = repro_id(7, &["tap:Login", "type:user", "tap:Submit"]);
        assert_eq!(a, b);
        assert_eq!(a.len(), 12);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn finding_id_scopes_zero_action_findings_and_remains_stable() {
        let a = finding_id(
            "web:https://one.example",
            "crash:no-exception:a",
            0,
            &[] as &[&str],
        );
        let same = finding_id(
            "web:https://one.example",
            "crash:no-exception:a",
            0,
            &[] as &[&str],
        );
        let other_target = finding_id(
            "web:https://two.example",
            "crash:no-exception:a",
            0,
            &[] as &[&str],
        );
        let other_oracle = finding_id(
            "web:https://one.example",
            "occlusion:no-occluded-control:a",
            0,
            &[] as &[&str],
        );

        assert_eq!(a, same);
        assert_ne!(a, other_target);
        assert_ne!(a, other_oracle);
    }

    #[test]
    fn public_prefix_helpers_define_public_id_shapes() {
        let raw = "abcdef123456";
        assert_eq!(display_finding_id(raw), "fnd_abcdef123456");
        assert_eq!(display_repro_id(raw), "rep_abcdef123456");
        assert_eq!(display_finding_id("fnd_abcdef123456"), "fnd_abcdef123456");
        assert_eq!(display_repro_id("rep_abcdef123456"), "rep_abcdef123456");
        assert_eq!(raw_finding_id("fnd_abcdef123456"), Some(raw));
        assert_eq!(raw_repro_id("rep_abcdef123456"), Some(raw));
        assert_eq!(raw_finding_id(raw), None);
        assert_eq!(raw_repro_id(raw), None);
    }

    #[test]
    fn resolve_accepts_public_repro_ids_and_aliases() {
        let root = std::env::temp_dir().join(format!("reproit-rep-{}", std::process::id()));
        let meta = Meta {
            id: "abcdef123456".to_string(),
            alias: Some("checkout".to_string()),
            status: Status::Quarantined,
            seed: 7,
            created: "2026-06-27T00:00:00Z".to_string(),
            last_checked: None,
            last_result: None,
            trigger_index: Some(1),
            trigger_sig: None,
            trigger_selector: None,
            oracle: Some("crash".to_string()),
            record_url: None,
            record_action: None,
        };
        save_meta(&root, &meta).unwrap();
        assert!(resolve(&root, "abcdef123456").is_none());
        assert_eq!(resolve(&root, "rep_abcdef123456").unwrap().id, meta.id);
        assert_eq!(resolve(&root, "checkout").unwrap().id, meta.id);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn id_is_whitespace_insensitive_self_deduping() {
        // The same case captured with stray whitespace dedupes to one id.
        let clean = repro_id(1, &["tap:A", "tap:B"]);
        let messy = repro_id(1, &["  tap:A ", "tap:B", "   "]);
        assert_eq!(clean, messy);
    }

    #[test]
    fn id_depends_on_seed_actions_and_order() {
        let base = repro_id(1, &["tap:A", "tap:B"]);
        assert_ne!(base, repro_id(2, &["tap:A", "tap:B"]), "seed matters");
        assert_ne!(base, repro_id(1, &["tap:A", "tap:C"]), "actions matter");
        assert_ne!(base, repro_id(1, &["tap:B", "tap:A"]), "order matters");
    }

    #[test]
    fn verdict_miss_before_first_action_is_could_not_replay_fallback() {
        // No trigger recorded (older repro): a miss on the VERY FIRST action
        // means nothing replayed -> stale by the fallback heuristic.
        let log = "FUZZ:MISS tap:A\nJOURNEY DONE\n";
        assert_eq!(verdict_from_log(log, true), RunVerdict::CouldNotReplay);
    }

    #[test]
    fn unmatched_capsule_request_is_stale_even_if_it_causes_an_error() {
        let log = "CAPSULE:MISS GET /api action=0\nEXCEPTION CAUGHT BY WEB PAGE\nTypeError: \
                   failed fetch\n";
        assert_eq!(
            verdict_from_log_with_trigger(log, false, &Trigger::unknown()),
            RunVerdict::CouldNotReplay
        );
    }

    #[test]
    fn verdict_partial_replay_no_finding_is_green_fallback() {
        // No trigger recorded: at least the first action replayed and no finding
        // fired, so the partial replay is a PASS, not stale.
        let log = "FUZZ:ACT tap:A\nFUZZ:MISS tap:B\nJOURNEY DONE\n";
        assert_eq!(verdict_from_log(log, true), RunVerdict::Green);
    }

    #[test]
    fn verdict_failed_verdict_is_broke_even_with_miss() {
        // A reproduced finding (non-pass verdict) wins over a later miss.
        let log = "FUZZ:ACT tap:A\nFUZZ:MISS tap:B\nJOURNEY DONE\n";
        assert_eq!(verdict_from_log(log, false), RunVerdict::Broke);
    }

    #[test]
    fn verdict_app_exception_is_broke() {
        let log = "\
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: The following assertion was thrown:
flutter: boom
flutter: ════════════════════════
JOURNEY DONE
";
        assert_eq!(verdict_from_log(log, true), RunVerdict::Broke);
    }

    #[test]
    fn verdict_framework_exception_is_not_broke() {
        let log = "\
flutter: ══╡ EXCEPTION CAUGHT BY FLUTTER TEST FRAMEWORK ╞══
flutter: boom
JOURNEY DONE
";
        assert_eq!(verdict_from_log(log, true), RunVerdict::Green);
    }

    #[test]
    fn verdict_failed_verdict_is_broke() {
        assert_eq!(verdict_from_log("JOURNEY DONE\n", false), RunVerdict::Broke);
        assert_eq!(verdict_from_log("JOURNEY DONE\n", true), RunVerdict::Green);
    }

    // ----- no-verdict guard (the crashed/timed-out runner case) -----
    //
    // A drive that FAILED but produced neither an app exception NOR any replay
    // signal never ran the case (the runner crashed/timed out or hit a setup
    // error). It must NOT read as a reproduced finding: that would be a FALSE
    // FAIL. The guard classifies it CouldNotReplay -> STALE.

    #[test]
    fn empty_failed_log_is_could_not_replay_not_false_fail() {
        // The bare case: drive failed, log empty. Old behavior: `!passed` ->
        // Broke -> a FALSE FAIL. Now: no signal, no exception -> CouldNotReplay.
        assert_eq!(verdict_from_log("", false), RunVerdict::CouldNotReplay);
        assert_eq!(verdict_from_log("\n\n", false), RunVerdict::CouldNotReplay);
    }

    #[test]
    fn setup_error_chatter_without_replay_signal_is_could_not_replay() {
        // A drive that failed during setup prints diagnostics but no FUZZ/EXPLORE
        // markers and no JOURNEY DONE: it never replayed the case -> not a verdict.
        let log = "\
flutter: Could not connect to the device.
Error: build failed.
";
        assert_eq!(verdict_from_log(log, false), RunVerdict::CouldNotReplay);
        assert_eq!(classify(&[RunVerdict::CouldNotReplay; 3]), Outcome::Stale);
        assert_eq!(Outcome::Stale.exit_code(), 3);
    }

    #[test]
    fn failed_drive_with_replay_signal_is_still_broke() {
        // The guard must NOT swallow a real reproduction: a failed drive that DID
        // replay (it carries action markers / JOURNEY DONE) is still Broke. This
        // is the line between "the runner died" and "the run failed the case".
        assert_eq!(
            verdict_from_log("FUZZ:ACT tap:A\nJOURNEY DONE\n", false),
            RunVerdict::Broke
        );
        assert_eq!(verdict_from_log("JOURNEY DONE\n", false), RunVerdict::Broke);
    }

    #[test]
    fn failed_drive_with_exception_is_still_broke() {
        // A failed drive carrying an app exception is a reproduction even with no
        // FUZZ markers (the crash fired before/at the first action): the exception
        // is itself the verdict signal, so the guard does not fire.
        let log = "\
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: boom
flutter: ════════════════════════
";
        assert_eq!(verdict_from_log(log, false), RunVerdict::Broke);
    }

    #[test]
    fn classify_all_green_is_pass() {
        let v = vec![RunVerdict::Green; 5];
        assert_eq!(classify(&v), Outcome::Pass);
    }

    #[test]
    fn classify_all_broke_is_fail() {
        let v = vec![RunVerdict::Broke; 3];
        assert_eq!(classify(&v), Outcome::Fail);
    }

    #[test]
    fn classify_mixed_green_broke_is_flaky() {
        let v = vec![
            RunVerdict::Green,
            RunVerdict::Broke,
            RunVerdict::Green,
            RunVerdict::Green,
        ];
        assert_eq!(classify(&v), Outcome::Flaky);
    }

    #[test]
    fn classify_could_not_replay_outranks_fail() {
        // A could-not-reach-trigger outranks a fail mix: re-record beats "failed".
        let v = vec![
            RunVerdict::Green,
            RunVerdict::CouldNotReplay,
            RunVerdict::Broke,
        ];
        assert_eq!(classify(&v), Outcome::Stale);
    }

    // ----- trigger-context classification (the dogfood fix) -----

    /// A trigger context recorded at `keep`: the finding fired after `index`
    /// actions. The replay logs below interleave FUZZ:ACT / FUZZ:MISS in order.
    fn trig(index: usize) -> Trigger {
        Trigger {
            index: Some(index),
            sig: None,
            selector: None,
            oracle: None,
        }
    }

    #[test]
    fn crash_repro_that_reproduces_is_fail() {
        // (1) The original exception fires on replay -> FAIL (exit 1).
        let trigger = trig(3);
        let log = "\
FUZZ:ACT tap:A
FUZZ:ACT tap:B
FUZZ:ACT tap:Crash
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: boom
flutter: ════════════════════════
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(log, true, &trigger),
            RunVerdict::Broke
        );
        assert_eq!(classify(&[RunVerdict::Broke; 3]), Outcome::Fail);
        assert_eq!(Outcome::Fail.exit_code(), 1);
    }

    #[test]
    fn miss_after_trigger_is_pass_the_fixed_bug_case() {
        // (2) The bug is FIXED: no exception, the replay reaches the trigger
        // (performs all 3 trigger actions), and a recorded action AFTER the
        // trigger misses because the fix changed downstream navigation. This
        // must be PASS (exit 0), not stale: the repro stays a green guard.
        let trigger = trig(3);
        let log = "\
FUZZ:ACT tap:A
FUZZ:ACT tap:B
FUZZ:ACT tap:WasCrash
FUZZ:MISS tap:Downstream
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(log, true, &trigger),
            RunVerdict::Green
        );
        assert_eq!(classify(&[RunVerdict::Green; 3]), Outcome::Pass);
        assert_eq!(Outcome::Pass.exit_code(), 0);
    }

    #[test]
    fn miss_before_trigger_is_stale() {
        // (3) A miss BEFORE reaching the trigger context (only 1 of 3 trigger
        // actions performed): the early path to the bug is gone -> STALE (exit 3).
        let trigger = trig(3);
        let log = "\
FUZZ:ACT tap:A
FUZZ:MISS tap:B
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(log, true, &trigger),
            RunVerdict::CouldNotReplay
        );
        assert_eq!(classify(&[RunVerdict::CouldNotReplay; 3]), Outcome::Stale);
        assert_eq!(Outcome::Stale.exit_code(), 3);
    }

    #[test]
    fn attempted_action_that_misses_does_not_reach_trigger() {
        let trigger = trig(1);
        let log = "FUZZ:ACT tap:key:load\nFUZZ:MISS tap:key:load\nJOURNEY DONE\n";
        assert_eq!(
            verdict_from_log_with_trigger(log, true, &trigger),
            RunVerdict::CouldNotReplay
        );
    }

    #[test]
    fn clean_full_replay_with_trigger_is_pass() {
        // No miss, no finding, trigger reached: the plainest fixed-bug PASS.
        let trigger = trig(2);
        let log = "FUZZ:ACT tap:A\nFUZZ:ACT tap:B\nJOURNEY DONE\n";
        assert_eq!(
            verdict_from_log_with_trigger(log, true, &trigger),
            RunVerdict::Green
        );
    }

    #[test]
    fn trigger_sig_reached_before_miss_is_pass() {
        // The optional sig path: reaching the recorded trigger sig before any
        // miss counts as reaching the trigger even if the action count fell short.
        let trigger = Trigger {
            index: Some(9),
            sig: Some("SIG:checkout".to_string()),
            selector: None,
            oracle: None,
        };
        let log = "\
FUZZ:ACT tap:A
EXPLORE:STATE SIG:checkout
FUZZ:MISS tap:Pay
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(log, true, &trigger),
            RunVerdict::Green
        );
    }

    #[test]
    fn trigger_sig_substring_collision_is_stale_not_pass() {
        // Regression: the recorded trigger sig is a short token that ALSO appears
        // as a substring of an unrelated EARLIER log line (a selector here), but
        // the actual trigger STATE is never reached -- the path moved and the first
        // action missed. The sig must be matched by EQUALITY on EXPLORE:STATE
        // markers only, not by an unanchored `line.contains(sig)`. An unanchored
        // match would falsely set saw_trigger_sig and return Green/Pass, silently
        // turning a stale (should-re-record) repro into a passing one. The correct
        // verdict is CouldNotReplay -> Stale.
        let trigger = Trigger {
            index: Some(9),
            sig: Some("checkout".to_string()),
            selector: None,
            oracle: None,
        };
        let log = "\
FUZZ:MISS tap:checkout-button
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(log, true, &trigger),
            RunVerdict::CouldNotReplay
        );

        // And the converse still holds: the sig DOES appear as a proper
        // EXPLORE:STATE marker before the miss -> the trigger was reached -> Green.
        let reached = "\
EXPLORE:STATE {\"sig\":\"checkout\",\"labels\":[\"Pay\"]}
FUZZ:MISS tap:Pay
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(reached, true, &trigger),
            RunVerdict::Green
        );
    }

    #[test]
    fn trigger_flaky_still_works() {
        // (4) Flakiness across the N runs is unaffected: a deterministic-finding
        // run mixed with clean runs is still FLAKY (exit 2). Each per-run verdict
        // comes from the trigger-aware classifier.
        let trigger = trig(2);
        let clean = "FUZZ:ACT tap:A\nFUZZ:ACT tap:B\nJOURNEY DONE\n";
        let broke = "\
FUZZ:ACT tap:A
FUZZ:ACT tap:B
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: boom
flutter: ════════════════════════
JOURNEY DONE
";
        let verdicts = vec![
            verdict_from_log_with_trigger(clean, true, &trigger),
            verdict_from_log_with_trigger(broke, true, &trigger),
            verdict_from_log_with_trigger(clean, true, &trigger),
        ];
        assert_eq!(verdicts[0], RunVerdict::Green);
        assert_eq!(verdicts[1], RunVerdict::Broke);
        assert_eq!(classify(&verdicts), Outcome::Flaky);
        assert_eq!(Outcome::Flaky.exit_code(), 2);
    }

    #[test]
    fn tester_capture_confirms_only_the_exact_structural_state() {
        let trigger = Trigger {
            index: Some(1),
            sig: Some("broken-checkout".to_string()),
            selector: None,
            oracle: Some("tester-capture".to_string()),
        };
        let reached = "FUZZ:ACT tap:key:checkout\nEXPLORE:STATE \
                       {\"sig\":\"broken-checkout\"}\nJOURNEY DONE\n";
        let changed =
            "FUZZ:ACT tap:key:checkout\nEXPLORE:STATE {\"sig\":\"fixed-checkout\"}\nJOURNEY DONE\n";
        let premature = "EXPLORE:STATE {\"sig\":\"broken-checkout\"}\nFUZZ:ACT \
                         tap:key:checkout\nEXPLORE:STATE {\"sig\":\"fixed-checkout\"}\n";
        assert_eq!(
            verdict_from_log_with_trigger(reached, true, &trigger),
            RunVerdict::Broke
        );
        assert_eq!(
            verdict_from_log_with_trigger(changed, true, &trigger),
            RunVerdict::CouldNotReplay
        );
        assert_eq!(
            verdict_from_log_with_trigger(premature, true, &trigger),
            RunVerdict::CouldNotReplay
        );
    }

    #[test]
    fn detached_indicator_replay_requires_exact_relationship_and_proof() {
        let trigger = Trigger {
            index: Some(0),
            sig: Some("nav".into()),
            selector: Some("key:id:dot".into()),
            oracle: Some("detached-indicator".into()),
        };
        let proven = concat!(
            "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[]}\n",
            "EXPLORE:RELATIONSTATUS {\"sig\":\"nav\",\"outcome\":\"PROVEN\",\"checks\":[",
            "{\"kind\":\"indicator-anchor\",\"dependentKey\":\"key:id:dot\",",
            "\"ownerKey\":\"key:id:tab\",\"containerKey\":\"key:id:tabs\",",
            "\"outcome\":\"PROVEN\",\"violation\":\"detached\"}]}\n",
            "EXPLORE:RELATION {\"sig\":\"nav\",\"items\":[",
            "{\"kind\":\"indicator-anchor\",\"dependentKey\":\"key:id:dot\",",
            "\"ownerKey\":\"key:id:tab\",\"containerKey\":\"key:id:tabs\",",
            "\"violation\":\"detached\",\"maxGap\":8,\"gap\":90}]}\n",
        );
        assert_eq!(
            verdict_from_log_with_trigger(proven, true, &trigger),
            RunVerdict::Broke
        );
        let valid = proven
            .replace("\"outcome\":\"PROVEN\"", "\"outcome\":\"VALID\"")
            .lines()
            .filter(|line| !line.starts_with("EXPLORE:RELATION "))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            verdict_from_log_with_trigger(&valid, true, &trigger),
            RunVerdict::Green
        );
        let unknown = "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[]}\nEXPLORE:RELATIONSTATUS \
                       {\"sig\":\"nav\",\"outcome\":\"UNKNOWN\",\"checks\":[]}";
        assert_eq!(
            verdict_from_log_with_trigger(unknown, true, &trigger),
            RunVerdict::CouldNotReplay
        );
    }

    #[test]
    fn crash_repro_unaffected_by_graph_path() {
        // A crash-oracle repro (or one with no oracle) is untouched: it still
        // uses the exception path, never the graph re-evaluation.
        let crash = Trigger {
            index: Some(2),
            sig: None,
            selector: None,
            oracle: Some("crash".to_string()),
        };
        let clean = "FUZZ:ACT tap:A\nFUZZ:ACT tap:B\nJOURNEY DONE\n";
        assert_eq!(
            verdict_from_log_with_trigger(clean, true, &crash),
            RunVerdict::Green
        );
        let exc = "\
FUZZ:ACT tap:A
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: boom
flutter: ════════════════════════
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(exc, true, &crash),
            RunVerdict::Broke
        );
    }

    #[test]
    fn outcome_severity_orders_for_suite_worst() {
        assert!(Outcome::Fail > Outcome::Flaky);
        assert!(Outcome::Flaky > Outcome::Stale);
        assert!(Outcome::Stale > Outcome::Pass);
        // The suite's worst is the max.
        let outcomes = [Outcome::Pass, Outcome::Stale, Outcome::Pass];
        assert_eq!(*outcomes.iter().max().unwrap(), Outcome::Stale);
    }

    #[test]
    fn exit_codes_match_the_contract() {
        assert_eq!(Outcome::Pass.exit_code(), 0);
        assert_eq!(Outcome::Fail.exit_code(), 1);
        assert_eq!(Outcome::Flaky.exit_code(), 2);
        assert_eq!(Outcome::Stale.exit_code(), 3);
    }

    #[test]
    fn check_result_reports_rate() {
        let v = vec![RunVerdict::Green, RunVerdict::Broke, RunVerdict::Green];
        let r = CheckResult::from_verdicts(&v);
        assert_eq!(r.outcome, Outcome::Flaky);
        assert_eq!(r.rate(), "2/3");
    }
}
