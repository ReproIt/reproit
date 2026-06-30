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
    /// The finding's TRIGGER POINT: the count of actions that must replay before
    /// the original finding fired (i.e. the position of the last action in the
    /// saved, minimized sequence). A replay that performs this many actions
    /// without an earlier miss has REACHED the trigger context, so a clean run
    /// is a real PASS (the fix held) and any miss AFTER this point is just the
    /// fix's downstream effect, not a stale path. A miss BEFORE this point means
    /// the path to the trigger no longer exists -> STALE. None for older repros
    /// kept before this field existed (handled by the fallback heuristic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_index: Option<usize>,
    /// The state signature that was active when the original finding fired, if it
    /// was recoverable at keep time. Optional companion to `trigger_index`: when
    /// present, reaching this sig in the replay log also counts as reaching the
    /// trigger context. For GRAPH-invariant findings (e.g. `no-dead-end`) this is
    /// also the VIOLATING state signature that `check` re-evaluates the invariant
    /// against. None when the report carried no sig.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_sig: Option<String>,
    /// The ORACLE category the finding belongs to (crash/graph/jank/leak/a11y/
    /// divergence/i18n), recorded at `keep` so `check` re-confirms the SAME
    /// finding by its oracle rather than only scanning for exceptions. A
    /// crash-class finding (or None, for repros kept before this field existed)
    /// uses the existing exception/process-death logic; a graph-class finding
    /// re-evaluates its invariant over the replay's EXPLORE markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<String>,
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
///                 reported a FAIL verdict): the actions REPLAYED and the app
///                 broke -> a real regression (the original finding reproduced).
///   - `CouldNotReplay` = a `FUZZ:MISS <act>` occurred BEFORE the replay reached
///                 the finding's TRIGGER CONTEXT, so the path to the bug no
///                 longer exists and the repro could not be meaningfully
///                 attempted -> the early UI changed (stale), NOT a failure.
///   - `Green`   = the original finding did NOT fire AND the replay reached the
///                 trigger context (it performed the actions up to the trigger
///                 index, or hit the trigger sig, before any miss). A miss AFTER
///                 the trigger is fine: that is the fix's downstream effect (the
///                 button that used to crash now navigates elsewhere), so the
///                 repro still PASSES as a green regression guard.
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
            oracle: None,
        }
    }

    /// Whether any trigger context was recorded (vs the fallback heuristic).
    fn is_known(&self) -> bool {
        self.index.is_some() || self.sig.is_some()
    }

    /// Whether this finding is a GRAPH-invariant finding (re-evaluated over the
    /// replay graph rather than scanned for exceptions).
    fn is_graph(&self) -> bool {
        self.oracle.as_deref() == Some("graph")
    }

    /// Whether this finding is a re-render FLICKER finding. Like a graph
    /// invariant it does not announce itself with an exception, so it is
    /// re-confirmed by re-evaluating the EXPLORE:RERENDER records over the
    /// replay graph rather than by scanning for a crash.
    fn is_flicker(&self) -> bool {
        self.oracle.as_deref() == Some("flicker")
    }

    /// Whether this finding is a DOM/layout OVERFLOW finding. Like a graph or
    /// flicker invariant it does not throw, so it is re-confirmed by re-evaluating
    /// the EXPLORE:OVERFLOW records over the replay graph rather than by scanning
    /// for an exception.
    fn is_overflow(&self) -> bool {
        self.oracle.as_deref() == Some("overflow")
    }

    /// Whether this finding is a CONTENT-BUG finding (a broken rendered label).
    /// Like overflow it does not throw, so it is re-confirmed by re-evaluating the
    /// EXPLORE:CONTENTBUG records over the replay graph.
    fn is_content_bug(&self) -> bool {
        self.oracle.as_deref() == Some("content-bug")
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
///      the `trigger.sig` state) -> Green. A miss AFTER the trigger is the fix's
///      downstream effect, not staleness: the fixed bug's repro stays green.
///   3. No finding, and a miss happened BEFORE reaching the trigger context ->
///      CouldNotReplay (stale): the early path to the bug no longer exists, so
///      the repro could not be meaningfully attempted.
///
/// Fallback heuristic (no trigger context recorded, e.g. an older repro): treat
/// "no finding fired and at least the first action replayed" as Green, reserving
/// CouldNotReplay for a miss on the VERY FIRST action (or a failure to perform
/// any action at all). This keeps a fixed-bug repro green by default and only
/// calls stale when the replay could not even get off the ground.
pub fn verdict_from_log_with_trigger(log: &str, passed: bool, trigger: &Trigger) -> RunVerdict {
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
    // oracle: a crash during a graph repro's replay is still a regression.
    if !passed || has_app_exception(log) {
        return RunVerdict::Broke;
    }

    // GRAPH-invariant findings (e.g. no-dead-end) do not announce themselves with
    // an exception: the replay reaches the trap, the trailing actions legitimately
    // FUZZ:MISS at the dead end, and the run still prints "All tests passed". So
    // re-confirm the ORIGINAL finding by re-running its invariant over the replay
    // graph rather than looking for exceptions.
    if trigger.is_graph() {
        return graph_verdict(log, trigger);
    }

    // FLICKER findings, like graph invariants, do not throw: the replay re-drives
    // the same transition and the runner re-emits EXPLORE:RERENDER iff the wasteful
    // re-render still happens. Re-confirm by re-evaluating those records over the
    // replay graph rather than scanning for an exception.
    if trigger.is_flicker() {
        return flicker_verdict(log, trigger);
    }

    // OVERFLOW findings, like graph/flicker invariants, do not throw: the replay
    // re-drives to the same state and the runner re-emits EXPLORE:OVERFLOW iff the
    // layout still clips/overflows. Re-confirm by re-evaluating those records over
    // the replay graph rather than scanning for an exception.
    // CONTENT-BUG / JANK / HANG findings, like graph/flicker/overflow, do not
    // throw: the replay re-drives to the same state/transition and the runner
    // re-emits the same EXPLORE:CONTENTBUG / EXPLORE:JANK / EXPLORE:HANG marker iff
    // the defect is still present. Re-confirm by re-evaluating those records.
    if trigger.is_content_bug() {
        return content_bug_verdict(log, trigger);
    }
    if trigger.is_jank() {
        return jank_verdict(log, trigger);
    }
    if trigger.is_hang() {
        return hang_verdict(log, trigger);
    }
    if trigger.is_overflow() {
        return overflow_verdict(log, trigger);
    }

    // Count actions performed before the first miss, and whether any miss
    // occurred at all, by walking the log in order.
    let mut performed_before_first_miss = 0usize;
    let mut saw_miss = false;
    let mut saw_trigger_sig = false;
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
            saw_miss = true;
            break;
        }
        if line.contains("FUZZ:ACT ") {
            performed_before_first_miss += 1;
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
/// come from the runner's log protocol (templates/explorer*.dart), the same ones
/// the per-run classifiers below already key on.
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

/// Re-confirm a GRAPH-invariant finding over a replay log. Parses the replay's
/// EXPLORE:STATE/EDGE markers and re-evaluates the SAME `no-dead-end` invariant
/// (via `invariants::recheck_dead_end`) against the recorded violating state sig
/// (`trigger.sig`):
///   - the invariant trips again at that sig -> Broke (the dead end is back).
///   - the sig is reached but is no longer a dead end -> Green (the fix held).
///   - the sig is never observed in the replay -> CouldNotReplay (re-record).
///
/// With no recorded sig (older graph repro), fall back to whether ANY dead end
/// remains in the replay graph: any dead end -> Broke, none -> Green, and an
/// empty graph (no states observed) -> CouldNotReplay.
fn graph_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::invariants::recheck_dead_end(&obs, sig) {
            crate::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    // No recorded sig: judge the whole replay graph.
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::invariants::any_dead_end(&obs) {
        RunVerdict::Broke
    } else {
        RunVerdict::Green
    }
}

/// Re-confirm a `rerender-flicker` finding over a replay log. Parses the
/// replay's EXPLORE markers and re-evaluates the SAME churn predicate (via
/// `invariants::recheck_rerender_flicker`) against the recorded violating state
/// sig (`trigger.sig`, the transition's FROM state):
///   - the same transition still churns persistent chrome -> Broke (flicker back)
///   - the sig is reached but no transition from it churns -> Green (fix held)
///   - the sig is never observed in the replay -> CouldNotReplay (re-record).
///
/// With no recorded sig (older flicker repro), fall back to whether ANY churn
/// remains in the replay graph: any -> Broke, none -> Green, empty graph (no
/// states observed) -> CouldNotReplay.
fn flicker_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::invariants::recheck_rerender_flicker(&obs, sig) {
            crate::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::invariants::any_rerender_flicker(&obs) {
        RunVerdict::Broke
    } else {
        RunVerdict::Green
    }
}

/// Re-confirm a `no-overflow` finding over a replay log. Parses the replay's
/// EXPLORE markers and re-evaluates the SAME overflow predicate (via
/// `invariants::recheck_overflow`) against the recorded violating state sig:
///   - the recorded state still overflows -> Broke (the clip/overflow is back)
///   - the sig is reached but nothing overflows there -> Green (the fix held)
///   - the sig is never observed in the replay -> CouldNotReplay (re-record).
///
/// With no recorded sig (older overflow repro), fall back to whether ANY overflow
/// remains in the replay graph: any -> Broke, none -> Green, empty graph (no
/// states observed) -> CouldNotReplay.
fn overflow_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::invariants::recheck_overflow(&obs, sig) {
            crate::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::invariants::any_overflow(&obs) {
        RunVerdict::Broke
    } else {
        RunVerdict::Green
    }
}

/// Re-confirm a `no-broken-render` (content-bug) finding over a replay log,
/// mirroring `overflow_verdict`: re-evaluate the EXPLORE:CONTENTBUG records
/// against the recorded violating state sig, falling back to "any broken content
/// remains" when no sig was recorded.
fn content_bug_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::invariants::recheck_content_bug(&obs, sig) {
            crate::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::invariants::any_content_bug(&obs) {
        RunVerdict::Broke
    } else {
        RunVerdict::Green
    }
}

/// Re-confirm a `no-jank` (web jank) finding over a replay log. A jank stall is
/// keyed by the transition's FROM state, so re-evaluate the EXPLORE:JANK records
/// against the recorded sig; fall back to "any jank remains" with no sig.
fn jank_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::invariants::recheck_jank(&obs, sig) {
            crate::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::invariants::any_jank(&obs) {
        RunVerdict::Broke
    } else {
        RunVerdict::Green
    }
}

/// Re-confirm a `no-hang` (freeze) finding over a replay log, mirroring
/// `jank_verdict` against the EXPLORE:HANG records.
fn hang_verdict(log: &str, trigger: &Trigger) -> RunVerdict {
    let obs = crate::map::parse_run(log);
    if let Some(sig) = trigger.sig.as_deref().filter(|s| !s.is_empty()) {
        return match crate::invariants::recheck_hang(&obs, sig) {
            crate::invariants::GraphRecheck::StillViolating => RunVerdict::Broke,
            crate::invariants::GraphRecheck::Fixed => RunVerdict::Green,
            crate::invariants::GraphRecheck::NotReached => RunVerdict::CouldNotReplay,
        };
    }
    if obs.states.is_empty() {
        return RunVerdict::CouldNotReplay;
    }
    if crate::invariants::any_hang(&obs) {
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
/// trigger context" (a miss before the trigger), not "some later action missed".
/// A fixed bug whose fix changes downstream navigation (so a recorded action
/// misses AFTER the trigger) classifies Green per `verdict_from_log_with_trigger`
/// and so stays a required regression guard rather than dropping to stale.
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
            oracle: Some("crash".to_string()),
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
EXPLORE:STATE {\"sig\":\"checkout\",\"labels\":[\"Pay\"],\"unlabeled\":0}
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

    // ----- graph-invariant re-confirmation (the cross-cutting fix) -----
    //
    // A `no-dead-end` repro replays, reaches the trap state, and the trailing
    // actions legitimately FUZZ:MISS at the dead end while the run still reports
    // "All tests passed" with NO exception. The old classifier returned PASS/
    // STALE; the graph oracle now re-evaluates the invariant over the replay's
    // EXPLORE markers so a re-reached dead end is a real FAIL.

    /// A graph trigger recording the violating dead-end state sig.
    fn graph_trig(sig: &str) -> Trigger {
        Trigger {
            index: Some(2),
            sig: Some(sig.to_string()),
            oracle: Some("graph".to_string()),
        }
    }

    #[test]
    fn dead_end_replay_rereaches_trap_is_fail() {
        // (a) The repro replays, reaches `advanced`, which is STILL a dead end
        // (only a back edge), and trailing actions miss at the trap. No
        // exception, "passed" true: the OLD code would call this PASS. The graph
        // oracle re-trips the invariant -> Broke -> FAIL (exit 1).
        let trigger = graph_trig("advanced");
        let log = "\
EXPLORE:STATE {\"sig\":\"home\",\"labels\":[\"Go\"],\"unlabeled\":0}
EXPLORE:EDGE {\"from\":\"home\",\"action\":\"tap:Advanced\",\"to\":\"advanced\"}
EXPLORE:STATE {\"sig\":\"advanced\",\"labels\":[\"Advanced\"],\"unlabeled\":0}
EXPLORE:EDGE {\"from\":\"advanced\",\"action\":\"back\",\"to\":\"home\"}
FUZZ:ACT tap:Advanced
FUZZ:MISS tap:Continue
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
    fn dead_end_replay_after_fix_has_forward_exit_is_pass() {
        // (b) After a fix, the trap now has an outgoing forward edge (a Continue
        // control). The invariant no longer trips at `advanced` -> Green -> PASS.
        let trigger = graph_trig("advanced");
        let log = "\
EXPLORE:STATE {\"sig\":\"home\",\"labels\":[\"Go\"],\"unlabeled\":0}
EXPLORE:EDGE {\"from\":\"home\",\"action\":\"tap:Advanced\",\"to\":\"advanced\"}
EXPLORE:STATE {\"sig\":\"advanced\",\"labels\":[\"Advanced\",\"Continue\"],\"unlabeled\":0}
EXPLORE:EDGE {\"from\":\"advanced\",\"action\":\"tap:Continue\",\"to\":\"next\"}
EXPLORE:STATE {\"sig\":\"next\",\"labels\":[\"Next\"],\"unlabeled\":0}
FUZZ:ACT tap:Advanced
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
    fn dead_end_replay_cannot_reach_trap_is_stale() {
        // (c) The replay never reaches `advanced` (the early path moved). The
        // invariant cannot be re-evaluated -> CouldNotReplay -> STALE (exit 3).
        let trigger = graph_trig("advanced");
        let log = "\
EXPLORE:STATE {\"sig\":\"home\",\"labels\":[\"Go\"],\"unlabeled\":0}
EXPLORE:EDGE {\"from\":\"home\",\"action\":\"tap:Go\",\"to\":\"feed\"}
EXPLORE:STATE {\"sig\":\"feed\",\"labels\":[\"Feed\"],\"unlabeled\":0}
FUZZ:MISS tap:Advanced
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
    fn graph_repro_exception_on_replay_still_fails() {
        // A crash during a graph repro's replay is still a regression: the
        // exception path wins before graph re-evaluation runs.
        let trigger = graph_trig("advanced");
        let log = "\
EXPLORE:STATE {\"sig\":\"home\",\"labels\":[\"Go\"],\"unlabeled\":0}
flutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══════
flutter: boom
flutter: ════════════════════════
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(log, true, &trigger),
            RunVerdict::Broke
        );
    }

    #[test]
    fn graph_repro_without_sig_uses_any_dead_end() {
        // An older graph repro with no recorded sig: any dead end in the replay
        // graph re-trips the finding (Broke); a clean graph is Green.
        let trigger = Trigger {
            index: Some(1),
            sig: None,
            oracle: Some("graph".to_string()),
        };
        let with_sink = "\
EXPLORE:STATE {\"sig\":\"home\",\"labels\":[\"Go\"],\"unlabeled\":0}
EXPLORE:EDGE {\"from\":\"home\",\"action\":\"tap:Advanced\",\"to\":\"advanced\"}
EXPLORE:STATE {\"sig\":\"advanced\",\"labels\":[\"Advanced\"],\"unlabeled\":0}
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(with_sink, true, &trigger),
            RunVerdict::Broke
        );
        let clean = "\
EXPLORE:STATE {\"sig\":\"home\",\"labels\":[\"Go\"],\"unlabeled\":0}
EXPLORE:EDGE {\"from\":\"home\",\"action\":\"tap:Go\",\"to\":\"feed\"}
EXPLORE:STATE {\"sig\":\"feed\",\"labels\":[\"Feed\"],\"unlabeled\":0}
EXPLORE:EDGE {\"from\":\"feed\",\"action\":\"tap:Go\",\"to\":\"home\"}
JOURNEY DONE
";
        assert_eq!(
            verdict_from_log_with_trigger(clean, true, &trigger),
            RunVerdict::Green
        );
    }

    #[test]
    fn crash_repro_unaffected_by_graph_path() {
        // A crash-oracle repro (or one with no oracle) is untouched: it still
        // uses the exception path, never the graph re-evaluation.
        let crash = Trigger {
            index: Some(2),
            sig: None,
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
