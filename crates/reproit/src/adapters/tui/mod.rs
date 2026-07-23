//! TUI backend: drive a terminal app (vim, lazygit, k9s, Claude Code, any
//! CLI/TUI) inside a pseudo-terminal and emit the same marker protocol every
//! other backend uses. The "screen" is the VT cell grid parsed from the app's
//! ANSI output; an action is a keystroke.
//!
//! This is the most deterministic backend: a PTY is fully headless (no display
//! server), keystrokes go to the PTY (never the real keyboard), it runs at full
//! speed with no settle-for-animation waits, and the same key sequence replays
//! to the same screen. Spawned as `reproit __tui` by drive.rs.
//!
//! Env:
//!   REPROIT_TUI_CMD       the terminal command to launch (run via `sh -c`)
//!   REPROIT_FUZZ_CONFIG   fuzz config json
//! (seed/budget/replay/prefix/edgeWeights)

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reproit_tui_sig::{content_fingerprint, labels_of, structural_sig};

// Screenshot capture: render the vt100 cell grid to a PNG store/doc image.
mod capture;
mod fuzz_config;
mod interaction;
mod scenario;
mod session;
mod shot;
use capture::{shoot, Clip, ClipCapture};
use fuzz_config::{load_fuzz, Rng};
#[cfg(test)]
use interaction::{
    mouse_click_bytes, mouse_protocol, observe_mouse_protocol, observe_mouse_protocol_stream,
    GtElement, MouseProtocol,
};
use interaction::{mouse_probe, Groundtruth};
use scenario::run_scenario_actor;
#[cfg(test)]
use session::count_full_erases;
use session::{looks_crashed, spawn_session};

const ROWS: u16 = 40;
const COLS: u16 = 120;
const ACTION_BUDGET: u32 = 36;
const MAP_ACTION_BUDGET: u32 = 72;

/// No-progress floor: a session ends after this many consecutive INEFFECTIVE
/// actions (no skeleton-sig and no content-fingerprint change). Crossing it is
/// also the TUI freeze/hang signal: the app stopped responding to input for a
/// sustained run of keystrokes. Named so the HANG oracle and the loop guard
/// share one floor and the emitted bucket is deterministic.
const STUCK_FLOOR: u32 = 14;

/// The action alphabet: the keys a fuzzer presses, and the bytes they send.
/// Covers navigation + confirm + the common vim/less/q vocabulary.
const KEYS: &[(&str, &str)] = &[
    ("Down", "\x1b[B"),
    ("Up", "\x1b[A"),
    ("Right", "\x1b[C"),
    ("Left", "\x1b[D"),
    ("Enter", "\r"),
    ("Tab", "\t"),
    ("Esc", "\x1b"),
    ("Space", " "),
    ("slash", "/"),
    ("star", "*"),
    ("colon", ":"),
    // control keys: the classic TUI crash triggers (cancel a prompt, EOF).
    ("CtrlC", "\x03"),
    ("CtrlD", "\x04"),
    // letters: enable text entry (insert mode) and the wide vocabulary of
    // single-key commands real TUIs bind (vim/helix/gitui/etc.). Each letter's
    // byte is itself; in an input/insert mode they type text, in normal mode
    // they fire commands, both are how real crashes get reached.
    ("a", "a"),
    ("b", "b"),
    ("c", "c"),
    ("d", "d"),
    ("e", "e"),
    ("f", "f"),
    ("g", "g"),
    ("h", "h"),
    ("i", "i"),
    ("j", "j"),
    ("k", "k"),
    ("l", "l"),
    ("m", "m"),
    ("n", "n"),
    ("o", "o"),
    ("p", "p"),
    ("q", "q"),
    ("r", "r"),
    ("s", "s"),
    ("t", "t"),
    ("u", "u"),
    ("v", "v"),
    ("w", "w"),
    ("x", "x"),
    ("y", "y"),
    ("z", "z"),
    ("0", "0"),
    ("1", "1"),
    ("2", "2"),
    ("3", "3"),
    ("4", "4"),
    ("5", "5"),
    ("6", "6"),
    ("7", "7"),
    ("8", "8"),
    ("9", "9"),
    ("dollar", "$"),
];

/// Keys that are worth pressing in essentially any TUI, regardless of app:
/// navigation, confirm/cancel, and the classic crash triggers (cancel a prompt,
/// EOF). These are unioned into every command-aware action space so we never
/// lose the universal crash paths even when an app advertises a tiny keymap.
const UNIVERSAL: &[&str] = &[
    "Down", "Up", "Right", "Left", "Enter", "Tab", "Esc", "Space", "slash", "CtrlC", "CtrlD",
];

/// Map a single advertised character to one of our KEYS names (or None if we
/// don't model that key). Used by the footer-hint scraper and could be reused
/// by any "the app told us this key exists" source.
mod action;
use action::*;
fn emit(s: &str) {
    println!("{s}");
    let _ = std::io::stdout().flush();
}

// APP-INVARIANT oracle (EXPLORE:INVARIANT, SDK-self-triggered).
//
// The app declares its own predicates via the reproit SDK (`ReproIt.invariant(
// "id", fn)`). Under the fuzzer the SDK evaluates them on its state-observe
// hook and reports the FAILURES on a diagnostic channel as a marker line
//   REPROIT_INVARIANT {"sig":"<sig-or-empty>","items":[{"id","message"}...]}
// This backend maps each marker into the CLI wire line the engine parses,
//   EXPLORE:INVARIANT {"sig":"<runner sig>","items":[...]}
// keyed on the state signature the runner is currently on (map.rs substitutes
// nothing; the runner owns the sig), de-duped per state.
//
// CHANNEL (why a runner-provisioned side file, not stderr): the contract
// prefers stderr for TUI, but a PTY is the exception it anticipates. The
// child's stdout AND stderr are dup'd onto the same slave, so both ARE the
// rendered-frame byte stream this backend parses into the VT grid; there is no
// stderr separable from the frames. Worse, that stream is load-bearing for the
// crash oracle (`looks_crashed` scans the grid for a rendered "panicked at"
// that reaches the screen ONLY because a panic prints to stderr). A marker on
// stderr would corrupt the very frame we measure and be indistinguishable from
// a crash render. So the runner provisions a per-run file, hands its path to
// every launched session via `REPROIT_INVARIANT_FILE` (which is ALSO the SDK's
// fuzzer-detection gate: absent in production, the registry stays inert), and
// scrapes it here. This is a genuine PORT: stderr is conflated with frames, the
// file is not.

/// Path to this `reproit __tui` process's invariant marker file (per-pid, so
/// concurrent runners never share one). Provisioned once; handed to each
/// session.
fn marker_file_path() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        std::env::temp_dir()
            .join(format!("reproit-invariant-{}.ndjson", std::process::id()))
            .to_string_lossy()
            .into_owned()
    })
}

/// Structural input-purpose registry for terminal apps. A terminal's rendered
/// cell grid contains no retained widget metadata, so the SDK writes
/// declarations to this runner-owned side channel. It is non-visual,
/// locale-independent, and exists only while reproit launches the app.
fn input_file_path() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        std::env::temp_dir()
            .join(format!("reproit-inputs-{}.ndjson", std::process::id()))
            .to_string_lossy()
            .into_owned()
    })
}

mod invariants;
use invariants::*;
/// The target child's resident set size (RSS) in BYTES, or None on failure. RSS
/// is the OS process analogue of the web runner's v8 `heap_used`: the soak
/// oracle (modes/soak.rs) reads first-vs-last to compute the per-cycle slope.
fn rss_bytes(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let kib: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        Some(kib * 1024)
    }
}

/// Emit one `MEMORY:SAMPLE {"t_ms","heap_used"}` for the target child, the SAME
/// shape every desktop/web runner emits and the soak oracle parses (heap_used
/// carries RSS bytes). No-op when the pid is gone or RSS can't be read.
fn sample_rss(pid: u32, t_ms: u64) {
    if let Some(rss) = rss_bytes(pid) {
        emit(&format!(
            "MEMORY:SAMPLE {}",
            serde_json::json!({ "t_ms": t_ms, "heap_used": rss })
        ));
    }
}

fn coverage_is_incomplete(
    failed: bool,
    actions_attempted: usize,
    actions_effective: usize,
    nonzero_exits: u32,
) -> bool {
    !failed && actions_attempted > 0 && actions_effective == 0 && nonzero_exits > 0
}

/// The visible screen as (signature, fingerprint, labels).
///
/// SIGNATURE: built from the LAYOUT SKELETON (`skeleton_of`) PLUS a bounded
/// numeric value-class section (`numeric_value_classes`). Box-drawing borders,
/// field/gap extents, digit and symbol positions, and the cursor position are
/// structural and locale-invariant; natural-language words are collapsed to a
/// placeholder before hashing. The numeric value-classes give value-state apps
/// (a counter, a clock, a calculator) a few distinct states instead of one
/// frozen skeleton. The same screen rendered in English and German hashes to
/// the same node (docs/cli.md hard invariant), because value-classes are
/// buckets, not raw values, and the strict-decimal rule is locale-safe.
///
/// FINGERPRINT: a runner-local content fingerprint over the FULL screen text
/// (the actual rendered cells, digits and words included). This is the TUI
/// analogue of Layer 1 effect detection: it changes whenever any on-screen
/// value changes, even when the skeleton signature does not, so the explorer
/// never stalls on a value-only update (a counter incrementing). It is
/// ephemeral and NEVER enters the canonical state identity (`seen`); it only
/// answers "did the action do anything" (docs/signature.md, "Terminal and
/// instrumented surfaces").
///
/// LABELS: unchanged, the human-facing word set (display only). Full-screen
/// TUIs are wide box-drawing grids; tokenizing after blanking box glyphs yields
/// a stable label set for narrow (jless) and wide (gitui) UIs alike. These feed
/// `map show` and never the signature.
mod screen;
use screen::*;
pub fn run() -> Result<()> {
    let cmdline = std::env::var("REPROIT_TUI_CMD")
        .ok()
        .filter(|s| !s.is_empty())
        .context("REPROIT_TUI_CMD (terminal command to drive) required")?;
    // Multi-actor scenario: this process plays ONE actor of an authored
    // multi-user journey, pulling each action from the host conductor instead
    // of fuzzing. Same env contract as the web runner (the orchestrator passes
    // defines as env to every non-flutter backend).
    if let Some(base) = std::env::var("REPROIT_SCENARIO_BARRIER")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return run_scenario_actor(&cmdline, &base);
    }
    let fuzz = load_fuzz();
    let map_mode =
        fuzz.seed == 0 && fuzz.replay.is_none() && fuzz.prefix.is_none() && fuzz.seeds.is_empty();
    let mut rng = Rng::new(fuzz.seed);
    emit("JOURNEY claimed role=a");
    if fuzz.seed != 0 {
        emit(&format!("JOURNEY[a] step: fuzz seed={}", fuzz.seed));
    }

    // The branch-from corpus: a frontier prefix (if any) plus every production
    // seed path. Each session picks one entry to replay before branching, so we
    // fuzz outward from real/known-deep states instead of always cold-launching.
    let mut corpus: Vec<Vec<String>> = Vec::new();
    if let Some(p) = &fuzz.prefix {
        if !p.is_empty() {
            corpus.push(p.clone());
        }
    }
    corpus.extend(fuzz.seeds.iter().cloned());
    let longest_seed = corpus.iter().map(|p| p.len()).max().unwrap_or(0);
    // budget = branch actions + room to replay the longest seed first.
    let budget = fuzz.replay.as_ref().map(|r| r.len()).unwrap_or_else(|| {
        let actions = if map_mode && !fuzz.configured {
            std::env::var("REPROIT_MAP_ACTION_BUDGET")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(MAP_ACTION_BUDGET)
        } else {
            fuzz.budget
        };
        (actions as usize) + longest_seed
    });
    // round-robin / least-used seed picker state.
    let mut seed_uses: Vec<u64> = vec![0; corpus.len()];

    // App-invariant scrape (EXPLORE:INVARIANT): truncate a fresh marker file so
    // the first read starts at offset 0, then scrape it after every settle. The
    // launched sessions inherit REPROIT_INVARIANT_FILE (set in spawn_session).
    let _ = std::fs::File::create(marker_file_path());
    let _ = std::fs::File::create(input_file_path());
    let mut inv = InvariantScrape::new(marker_file_path());

    let mut seen: BTreeSet<String> = BTreeSet::new();
    // Blank-screen guard, run-wide (spans relaunched sessions): has the app
    // painted at least one non-blank screen yet? Only then can a later
    // all-whitespace screen be the blank-screen bug rather than a slow boot.
    let mut seen_content = false;
    let mut tried: BTreeSet<String> = BTreeSet::new();
    let mut actions_by_state: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut graph: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut launch_sig: Option<String> = None;
    // Live coverage guidance: how many times we've taken each (state, action)
    // THIS run, keyed "sig|key:Name". Feeds the UCB explore term.
    let mut live_visits: BTreeMap<String, u64> = BTreeMap::new();
    // UCB bookkeeping: cumulative reward per arm (reward paid when an action
    // reveals a NEW state), and total pulls out of each state. Tabular, no ML.
    let mut arm_reward: BTreeMap<String, f64> = BTreeMap::new();
    let mut state_pulls: BTreeMap<String, u64> = BTreeMap::new();
    let mut announced_space = false;
    // A/B switch: REPROIT_TUI_UNIFORM=1 disables command-awareness (no bound
    // priority / bonus, full alphabet treated uniformly) so the uniform baseline
    // can be measured head-to-head under the same seed and budget.
    let uniform = std::env::var("REPROIT_TUI_UNIFORM")
        .map(|v| v == "1")
        .unwrap_or(false);
    // Fraction of picks that explore the unbound long tail (the rest focus the
    // bound command set). Higher = closer to uniform coverage / safer when the
    // keymap is incomplete; lower = tighter focus / faster crashes on rich apps.
    let eps: f64 = std::env::var("REPROIT_TUI_EPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.5);
    // Operability/accessibility ground truth (EXPLORE:GROUNDTRUTH). SGR mouse
    // clicks -> mouse-only controls are gated behind REPROIT_TUI_MOUSE=1 because it
    // sends mouse-reporting escapes and extra input the default keyboard-only
    // run does not, and not every app honors it.
    let mut gt = Groundtruth::new();
    let mouse = std::env::var("REPROIT_TUI_MOUSE")
        .map(|v| v == "1")
        .unwrap_or(false);
    // States reached by a keystroke this run: a state in here that a mouse click
    // ALSO reaches is keyboard-operable; a state ONLY a click reaches is the
    // mouse-only gap Signal B emits. The launch/start states seed it (reachable
    // with no input at all, so never mouse-only).
    let mut keyboard_reached: BTreeSet<String> = BTreeSet::new();
    let mut failed = false;
    let mut i = 0usize;
    let mut sessions = 0u32;
    let mut actions_attempted = 0usize;
    let mut actions_effective = 0usize;
    let mut nonzero_exits = 0u32;
    let mut launch_failures = 0u32;
    // Optional frame capture: REPROIT_TUI_FRAMES=path makes us record the real
    // app's rendered screen after each action, so a side-by-side demo can show
    // the actual app reacting to every step (proof it's a real reproduction).
    let frames_path = std::env::var("REPROIT_TUI_FRAMES")
        .ok()
        .filter(|s| !s.is_empty());
    let mut frames: Vec<serde_json::Value> = Vec::new();
    // LEAK sampler (--soak): the soak tier writes a flat {"replay":[..]} of the
    // cycle repeated N times, so a replay run IS a soak. In that mode we sample
    // the child's RSS once at session start and after each replayed action,
    // forming the RSS-vs-time series soak.rs reads (it uses first-vs-last to get
    // the per-cycle slope). A TUI app is an OS process with a pid, so this is the
    // same MEMORY:SAMPLE signal the AppKit/AT-SPI desktop runners emit. No-op
    // outside replay (a plain fuzz walk is not a soak), matching every runner.
    let is_soak = fuzz.replay.is_some();
    let soak_start = Instant::now();
    // --record-video clip capture: only in replay mode (a clip reproduces one finding)
    // and only when REPROIT_VIDEO_DIR is set. We film the frames render_screen
    // produces during the replay, then box the finding's element after it settles.
    let mut clip: Option<ClipCapture> = if fuzz.replay.is_some() {
        fuzz.clip.as_ref().and_then(ClipCapture::arm)
    } else {
        None
    };

    // Returns (signature, content_fingerprint, was_this_state_newly_discovered).
    // The bool is the UCB reward signal. The fingerprint is the runner-local
    // Layer-1 effect-detection token (full screen text, value-sensitive); it is
    // NEVER inserted into `seen`, only compared step-to-step to decide whether an
    // action did anything (docs/signature.md "Terminal and instrumented surfaces").
    let emit_state = |parser: &Arc<Mutex<vt100::Parser>>,
                      seen: &mut BTreeSet<String>,
                      seen_content: &mut bool|
     -> (String, String, bool) {
        let (sig, fp, labels) = snapshot(parser);
        let grid = grid_of(parser);
        let observation = serde_json::json!({
            "sig": sig,
            "labels": labels,
            "elements": structural_input_elements()
        });
        emit(&crate::domain::runner::observation_frame_line(&observation));
        let is_new = seen.insert(sig.clone());
        if is_new {
            let payload = serde_json::json!({
                "sig": sig,
                "labels": labels,
                "elements": structural_input_elements()
            });
            emit(&format!("EXPLORE:STATE {payload}"));
            // CONTENT-BUG oracle (EXPLORE:CONTENTBUG): scan the SETTLED screen for
            // the same broken-content artifacts the web runner catches ([object
            // Object], unrendered {{...}}/${...}, whole-word undefined/null/NaN).
            // Emitted once per newly-seen state (keyed by the same sig as STATE so
            // the engine attributes it to this node), each item keyed by the
            // `pos:R,C` of the match. Pure function of the grid, so it re-confirms
            // on replay; a clean screen emits nothing.
            let bugs = detect_content_bugs(&grid);
            if !bugs.is_empty() {
                let items: Vec<serde_json::Value> = bugs
                    .iter()
                    .map(
                        |b| serde_json::json!({ "key": b.key, "reason": b.reason, "text": b.text }),
                    )
                    .collect();
                let payload = serde_json::json!({ "sig": sig, "items": items });
                emit(&format!("EXPLORE:CONTENTBUG {payload}"));
            }
            // ZERO-CONTRAST oracle (EXPLORE:ZEROCONTRAST): an emphasized glyph
            // run whose resolved foreground exactly equals its resolved
            // background renders invisible where visibility is structurally
            // required (a selected row, an explicitly styled region). Same
            // emission rules as CONTENTBUG: once per newly-seen state, keyed
            // by the same sig, silent when the screen is clean.
            let invisible = detect_zero_contrast(&color_grid_of(parser));
            if !invisible.is_empty() {
                let items: Vec<serde_json::Value> = invisible
                    .iter()
                    .map(|z| serde_json::json!({ "key": z.key, "text": z.text, "color": z.color }))
                    .collect();
                let payload = serde_json::json!({ "sig": sig, "items": items });
                emit(&format!("EXPLORE:ZEROCONTRAST {payload}"));
            }
            // BROKEN-ASSET (tofu) oracle (EXPLORE:BROKENASSET): a cell rendering
            // the U+FFFD replacement character is broken text encoding reaching
            // the screen. Same emission rules as CONTENTBUG: once per newly-seen
            // state, keyed by the same sig, silent when the screen is clean.
            let tofu = detect_tofu(&grid);
            if !tofu.is_empty() {
                let items: Vec<serde_json::Value> = tofu
                    .iter()
                    .map(|(k, detail)| {
                        serde_json::json!({ "key": k, "reason": "tofu", "detail": detail })
                    })
                    .collect();
                let payload = serde_json::json!({ "sig": sig, "items": items });
                emit(&format!("EXPLORE:BROKENASSET {payload}"));
            }
            // BLANK-SCREEN oracle (EXPLORE:BLANKSCREEN): a settled screen with
            // zero non-whitespace cells in a non-zero PTY, once the app has
            // painted at least one non-blank screen earlier in the run (the
            // `seen_content` guard, so a slow boot never fires). Emitted once per
            // newly-seen state, keyed by the same sig as STATE. Blankness must
            // PERSIST: an Ink-style app clears then repaints its whole region every
            // frame, so this settled sample can catch the all-whitespace transient
            // between the two. Only when the first sample is blank do we pay a short
            // re-sample; a screen that has ink on the re-sample is a repaint gap, not
            // a blank screen, and stays silent (a measured non-reproducible FP).
            if blank_screen_item(&grid, *seen_content).is_some() {
                std::thread::sleep(Duration::from_millis(BLANK_RESAMPLE_MS));
                let regrid = grid_of(parser);
                if let Some((w, h)) = blank_screen_persisted(&grid, &regrid, *seen_content) {
                    let payload = serde_json::json!({
                        "sig": sig,
                        "items": [ { "key": "root", "w": w, "h": h } ],
                    });
                    emit(&format!("EXPLORE:BLANKSCREEN {payload}"));
                }
            }
        }
        if screen_has_ink(&grid) {
            *seen_content = true;
        }
        (sig, fp, is_new)
    };

    // Outer loop: (re)launch the app and fuzz until the action budget is spent.
    // A clean app exit (a quit key like `q`) is NOT a bug and is NOT the end of
    // fuzzing, relaunch and keep going. Only a crash (panic / non-zero exit)
    // stops the run.
    'fuzz: while i < budget {
        sessions += 1;
        let (master, mut child, parser, writer, erases, _mouse) = match spawn_session(&cmdline) {
            Ok(s) => s,
            Err(e) => {
                launch_failures += 1;
                emit(&format!("JOURNEY[a] step: launch failed: {e}"));
                break;
            }
        };
        let launch_settle_ms = if map_mode {
            if sessions == 1 {
                450
            } else {
                220
            }
        } else if sessions == 1 {
            900
        } else {
            450
        };
        std::thread::sleep(Duration::from_millis(launch_settle_ms));
        // The target child's pid, for the --soak RSS sampler. The session-start
        // sample (t_ms=0 on the first session) is the soak baseline; per-action
        // samples below extend the RSS-vs-time series.
        let child_pid = child.process_id();
        if is_soak {
            if let Some(pid) = child_pid {
                let t = if sessions == 1 {
                    0
                } else {
                    soak_start.elapsed().as_millis() as u64
                };
                sample_rss(pid, t);
            }
        }
        let (mut cur_sig, mut cur_fp, _) = emit_state(&parser, &mut seen, &mut seen_content);
        inv.flush_for(&cur_sig);
        if launch_sig.is_none() {
            launch_sig = Some(cur_sig.clone());
        }
        // The start/launch state is reachable with NO input, so it can never be
        // a mouse-only state (Signal B).
        keyboard_reached.insert(cur_sig.clone());
        if frames_path.is_some() && frames.is_empty() {
            let scr = parser.lock().unwrap().screen().contents();
            frames.push(serde_json::json!({ "action": "(launch)", "screen": scr }));
        }
        // --record-video: film the launch/start frame before any action, so the clip
        // opens on the app's initial screen (the lead-in the desktop runners get
        // from screencapture's warm-up).
        if let Some(cap) = clip.as_mut() {
            cap.capture(&parser);
        }
        // Pick this session's seed path to branch from (least-used wins, so we
        // rotate through the corpus). Pure replay overrides seeding entirely.
        let session_seed: Option<&Vec<String>> = if fuzz.replay.is_some() || corpus.is_empty() {
            None
        } else {
            let idx = (0..corpus.len()).min_by_key(|&k| seed_uses[k]).unwrap_or(0);
            seed_uses[idx] += 1;
            Some(&corpus[idx])
        };
        let mut sp = 0usize; // cursor into the session seed path
        let mut stuck = 0u32;
        // (from_sig, action) of the action that began the current no-progress
        // run, so when `stuck` crosses STUCK_FLOOR we attribute the HANG to the
        // transition that started the freeze, not the last (redundant) one. Set
        // on the first ineffective action of a run; cleared on any effect.
        let mut hang_origin: Option<(String, String)> = None;
        // HANG is emitted at most once per no-progress run (per session) so a
        // long freeze is one finding, not STUCK_FLOOR copies.
        let mut hang_emitted = false;
        let mut exhausted_this_session = false;

        while i < budget && stuck < STUCK_FLOOR {
            // Command-aware action space for THIS screen: the app's bound keys
            // (keymap + advertised footer hints) ∪ universal nav/crash keys,
            // falling back to the full alphabet only when nothing app-specific
            // is known. Most TUI keys are no-ops; this is what stops us wasting
            // ~80% of presses.
            let (space, bound_raw) = action_space(&cmdline, &parser);
            // Uniform A/B: empty bound set => no key is prioritized or bonused,
            // so ucb_pick degrades to plain UCB1 over the full flat alphabet.
            let (space, bound) = if map_mode {
                // Map mode is graph discovery, not adversarial crash hunting.
                // Drive the finite command/nav surface the app advertises and the
                // universal navigation keys, but skip crash triggers and the
                // unbound alphabet tail. Fuzz mode keeps those.
                let mapped: Vec<String> = space
                    .iter()
                    .filter(|action| bound_raw.contains(*action) && !is_crash_trigger(action))
                    .cloned()
                    .collect();
                let mapped_bound: BTreeSet<String> = mapped.iter().cloned().collect();
                (mapped, mapped_bound)
            } else {
                let bound = if uniform { BTreeSet::new() } else { bound_raw };
                (space, bound)
            };
            remember_actions(
                &mut actions_by_state,
                &cur_sig,
                ordered_actions(&space, &bound),
            );
            if !announced_space {
                announced_space = true;
                let seeded = if corpus.is_empty() {
                    String::new()
                } else {
                    format!(", seeded from {} production path(s)", corpus.len())
                };
                emit(&format!(
                    "JOURNEY[a] step: command-aware action space ({} keys, {} bound first){seeded}",
                    space.len(),
                    bound.len()
                ));
            }
            // Systematic map mode: take an untried action from this state. If the
            // state is exhausted, follow the known graph to the nearest state with
            // untried actions. If no frontier is reachable from here but some
            // frontier still exists globally, relaunch and replay from the start
            // on the next session. If no frontier exists, mapping is done.
            let systematic = |cur: &str| -> Option<String> {
                if let Some(action) = first_untried_action(&actions_by_state, &tried, cur) {
                    return Some(action);
                }
                if let Some(path) = path_to_frontier(&graph, &actions_by_state, &tried, cur) {
                    if let Some(action) = path.first() {
                        return Some(action.clone());
                    }
                }
                None
            };
            // replay > session seed path (branch-from) > UCB bandit > systematic
            let act: Option<String> = if let Some(r) = &fuzz.replay {
                r.get(i).cloned()
            } else if let Some(path) = session_seed {
                if sp < path.len() {
                    let a = path[sp].clone();
                    sp += 1;
                    Some(a)
                } else if fuzz.seed != 0 {
                    Some(ucb_pick(
                        &space,
                        &bound,
                        &cur_sig,
                        &live_visits,
                        &arm_reward,
                        &state_pulls,
                        fuzz.edge_weights.get(&cur_sig),
                        eps,
                        &mut rng,
                    ))
                } else {
                    systematic(&cur_sig)
                }
            } else if fuzz.seed != 0 {
                Some(ucb_pick(
                    &space,
                    &bound,
                    &cur_sig,
                    &live_visits,
                    &arm_reward,
                    &state_pulls,
                    fuzz.edge_weights.get(&cur_sig),
                    eps,
                    &mut rng,
                ))
            } else {
                systematic(&cur_sig)
            };
            let Some(act) = act else {
                if has_frontier(&actions_by_state, &tried) && launch_sig.as_ref() != Some(&cur_sig)
                {
                    exhausted_this_session = true;
                    break;
                }
                break 'fuzz;
            };
            // A "shoot:<name>" action is a screenshot point, not a keystroke:
            // render the CURRENT screen to a PNG and print the SHOOT marker, then
            // move on without sending any bytes or running the crash/effect
            // oracle (a capture changes nothing on screen). These only arrive via
            // an author-supplied replay/prefix/seed path; the UCB/systematic
            // pickers emit only "key:" actions. We still advance the step counter
            // so a replay/seed cursor progresses past the shoot.
            if let Some(name) = act.strip_prefix("shoot:") {
                emit(&crate::domain::runner::action_frame_line(None, &act));
                shoot(&parser, name);
                i += 1;
                if frames_path.is_some() {
                    let scr = parser.lock().unwrap().screen().contents();
                    frames.push(serde_json::json!({ "action": act, "screen": scr }));
                }
                continue;
            }
            emit(&crate::domain::runner::action_frame_line(None, &act));
            actions_attempted += 1;
            tried.insert(edge_key(&cur_sig, &act));
            *live_visits.entry(edge_key(&cur_sig, &act)).or_insert(0) += 1;
            *state_pulls.entry(cur_sig.clone()).or_insert(0) += 1;

            let key_name = act.strip_prefix("key:").unwrap_or(&act);
            let bytes = bytes_for_key(&parser, key_name);
            // Capture the grid BEFORE the keypress so Signal A can locate the
            // rectangle this action repaints (the diff between before and after),
            // and snapshot the full-erase count so the re-render oracle can tell
            // whether this action triggered a full clear+redraw.
            let pre_grid = grid_of(&parser);
            let erases_before = erases.load(Ordering::Relaxed);
            if !bytes.is_empty() {
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(&bytes);
                    let _ = w.flush();
                }
            }
            std::thread::sleep(Duration::from_millis(if map_mode { 120 } else { 260 }));
            i += 1;
            if frames_path.is_some() {
                let scr = parser.lock().unwrap().screen().contents();
                frames.push(serde_json::json!({ "action": act, "screen": scr }));
            }
            // --record-video: film this action's settled frame. The LAST replay action is
            // the finding's trigger, so once the replay cursor is spent we also
            // resolve the sel to a cell rect from the screen it just left behind.
            if let Some(cap) = clip.as_mut() {
                cap.capture(&parser);
                if fuzz.replay.as_ref().is_some_and(|r| i >= r.len()) {
                    cap.mark_trigger(&parser);
                }
            }
            // LEAK sampler (--soak): sample the child's RSS after this replayed
            // action settled, extending the RSS-vs-time series. No-op if the pid
            // is gone (the read just fails). Only in replay/soak mode.
            if is_soak {
                if let Some(pid) = child_pid {
                    sample_rss(pid, soak_start.elapsed().as_millis() as u64);
                }
            }

            // Oracle: a panic rendered to the screen, or the process dying.
            if looks_crashed(&parser) {
                emit("EXCEPTION CAUGHT BY TUI APP");
                emit("The following crash was rendered to the terminal:");
                for line in parser.lock().unwrap().screen().contents().lines().take(12) {
                    if !line.trim().is_empty() {
                        emit(line.trim_end());
                    }
                }
                emit("\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}");
                failed = true;
                let _ = child.kill();
                break 'fuzz;
            }
            if let Ok(Some(status)) = child.try_wait() {
                let code = status.exit_code();
                // A crash is a PANIC/ABORT/SIGNAL, not a benign non-zero exit.
                // Apps legitimately exit 1/2 on handled errors (e.g. gitui
                // pressing push with no remote -> "inconclusive remotes", exit
                // 1). Only treat a Rust panic (101) or a signal kill (>=128,
                // e.g. SIGABRT 134 / SIGSEGV 139) as a crash. Panics that print
                // but linger are still caught by looks_crashed() above.
                if code == 101 || code >= 128 {
                    emit("EXCEPTION CAUGHT BY TUI APP");
                    emit(&format!(
                        "The process crashed (exit code {code}) after {act}"
                    ));
                    emit("\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}");
                    failed = true;
                    break 'fuzz;
                }
                if code != 0 {
                    nonzero_exits += 1;
                }
                // Clean exit or a handled error -> relaunch via the outer loop.
                break;
            }

            let (next_sig, next_fp, is_new) = emit_state(&parser, &mut seen, &mut seen_content);
            inv.flush_for(&next_sig);
            // Layer-1 effect detection (TUI analogue): an action is EFFECTIVE iff
            // the skeleton signature changed OR the runner-local content
            // fingerprint changed. The fingerprint catches value-only updates (a
            // counter incrementing) that leave the skeleton frozen, so the
            // explorer does not stall when only on-screen values move.
            let sig_changed = next_sig != cur_sig;
            let effective = sig_changed || next_fp != cur_fp;
            if effective {
                actions_effective += 1;
            }
            // UCB reward: discovering a brand-new state pays full, moving to a
            // known-but-different state pays a little (still progress), staying
            // put pays nothing. An effective value-only change (same skeleton,
            // different content) still counts as progress so the bandit keeps
            // probing a live value-state screen instead of writing it off.
            let reward = if is_new {
                1.0
            } else if effective {
                0.25
            } else {
                0.0
            };
            *arm_reward.entry(format!("{cur_sig}|{act}")).or_insert(0.0) += reward;
            if sig_changed {
                let payload = serde_json::json!({ "from": cur_sig, "action": act, "to": next_sig });
                emit(&format!("EXPLORE:EDGE {payload}"));
                remember_edge(&mut graph, &cur_sig, &act, &next_sig);
            }
            // A keystroke reaching `next_sig` proves that state is keyboard-
            // operable (feeds the mouse-only test).
            keyboard_reached.insert(next_sig.clone());
            let post_grid = grid_of(&parser);
            // RE-RENDER FLICKER (EXPLORE:RERENDER, TUI analogue of the web
            // node-identity churn). This action made the app emit a FULL-SCREEN
            // erase (it cleared and repainted everything), yet the persistent
            // chrome rows (the box-drawing frame/panes) came back BYTE-IDENTICAL.
            // The app tore down and redrew chrome that did not change: a wasteful
            // full repaint. We require the action to be EFFECTIVE (something did
            // change), so a steady idle redraw is not flagged; the churned anchors
            // are the unchanged chrome rows. Deterministic: a pure function of the
            // app's own output bytes (the erase) and the two settled grids, so it
            // re-confirms on replay. An empty churn list (no surviving chrome) is
            // dropped, mirroring the web runner.
            let erased = erases.load(Ordering::Relaxed) > erases_before;
            if effective && erased {
                let churned = churned_chrome_rows(&pre_grid, &post_grid, 16);
                if !churned.is_empty() {
                    let payload = serde_json::json!({
                        "from": cur_sig, "action": act, "churned": churned,
                    });
                    emit(&format!("EXPLORE:RERENDER {payload}"));
                }
            }
            // `stuck` is the no-progress counter that ends a session. An action
            // with ANY effect (a new node, or just a value tick) resets it, so a
            // value-state app does not get abandoned as stalled. Crossing
            // STUCK_FLOOR is also the FREEZE/HANG signal: the app stopped
            // responding to input for a sustained run of keystrokes.
            if effective {
                stuck = 0;
                hang_origin = None;
                hang_emitted = false;
            } else {
                if stuck == 0 {
                    // First ineffective action of a fresh no-progress run: this is
                    // the transition that begins the freeze, so attribute the HANG
                    // to it (not the later redundant presses).
                    hang_origin = Some((cur_sig.clone(), act.clone()));
                }
                stuck += 1;
                // HANG (EXPLORE:HANG): the app ignored input for STUCK_FLOOR
                // consecutive keystrokes -> it has frozen / stopped responding,
                // the TUI analogue of the web watchdog's main-thread freeze. Keyed
                // by (from, action) like the web runner; the bucket is the
                // deterministic no-progress floor (count of ignored keystrokes, not
                // wall-clock ms, since a PTY has no frame timing). Emitted once per
                // run so a long freeze is one finding.
                if stuck >= STUCK_FLOOR && !hang_emitted {
                    if let Some((from, action)) = &hang_origin {
                        // `unit: keypresses` (a PTY has no frame clock), so the Rust
                        // message renders ">= 14 keypresses", not a bogus ">= 14ms".
                        let payload = serde_json::json!({
                            "from": from,
                            "action": action,
                            "bucket": STUCK_FLOOR,
                            "unit": "keypresses",
                        });
                        emit(&format!("EXPLORE:HANG {payload}"));
                        hang_emitted = true;
                    }
                }
            }
            cur_sig = next_sig;
            cur_fp = next_fp;
        }
        let _ = child.kill();
        drop(master);
        if exhausted_this_session {
            continue;
        }
    }

    // --record-video clip finalize: assemble the filmed frames into clip.mov, write the
    // finding box's cell rect + time window to box-spec.json, and emit
    // FINDING:BOXED. The host box-overlay step draws the red box post-capture.
    if let Some(mut cap) = clip.take() {
        cap.finalize();
    }

    if map_mode && !fuzz.configured && i >= budget && has_frontier(&actions_by_state, &tried) {
        emit(&format!(
            "EXPLORE:TRUNCATED {}",
            serde_json::json!({
                "reason": "action-budget",
                "budget": budget,
                "states": actions_by_state.len(),
            })
        ));
    }

    // SIGNAL B (mouse-only operability), gated behind REPROIT_TUI_MOUSE=1. Drive
    // deterministic mouse clicks, in the encoding requested by the app, at
    // hotspots (bracketed labels, reverse-video runs) and watch for states that a
    // click reaches but NO keystroke did. Such
    // a state is reachable only by pointer -> the control that leads there is
    // mouse-only / not keyboard-operable. Runs in its own fresh session(s) so it
    // never perturbs the keyboard exploration above; failures (an app that ignores
    // mouse reporting) are silent, the keyboard signal still stands.
    if mouse && !failed {
        mouse_probe(&cmdline, &mut seen, &keyboard_reached, &mut gt);
    }

    let transitions_observed: usize = graph.values().map(Vec::len).sum();
    let coverage_incomplete =
        coverage_is_incomplete(failed, actions_attempted, actions_effective, nonzero_exits);
    let stop_reason = if failed {
        "crash"
    } else if launch_failures > 0 {
        "launch-failed"
    } else if coverage_incomplete {
        "no-effective-actions-after-nonzero-exit"
    } else if i >= budget {
        "action-budget"
    } else {
        "frontier-exhausted"
    };
    emit(&format!(
        "EXPLORE:COVERAGE {}",
        serde_json::json!({
            "platform": "tui",
            "complete": !failed && !coverage_incomplete && launch_failures == 0,
            "states": seen.len(),
            "transitions": transitions_observed,
            "actionsAttempted": actions_attempted,
            "actionsEffective": actions_effective,
            "sessions": sessions,
            "nonzeroExits": nonzero_exits,
            "launchFailures": launch_failures,
            "stopReason": stop_reason,
        })
    ));

    emit(&format!(
        "JOURNEY[a] step: explored {} states over {} session(s), {} actions",
        seen.len(),
        sessions,
        i
    ));
    emit("JOURNEY DONE");
    emit(if failed {
        "Some tests failed"
    } else {
        "All tests passed"
    });
    if let Some(fp) = &frames_path {
        let _ = std::fs::write(fp, serde_json::to_string(&frames).unwrap_or_default());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
