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
//!   REPROIT_FUZZ_CONFIG   fuzz config json (seed/budget/replay/prefix/edgeWeights)

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reproit_tui_sig::{content_fingerprint, labels_of, structural_sig};

// Screenshot capture: render the vt100 cell grid to a PNG store/doc image.
#[path = "tui_shot.rs"]
mod shot;

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
fn char_to_keyname(c: char) -> Option<String> {
    match c {
        'a'..='z' => Some(c.to_string()),
        '0'..='9' => Some(c.to_string()),
        '/' => Some("slash".into()),
        '*' => Some("star".into()),
        ':' => Some("colon".into()),
        '$' => Some("dollar".into()),
        ' ' => Some("Space".into()),
        _ => None,
    }
}

/// Known single-key command vocabularies for popular TUIs, keyed by a substring
/// of the launch command. A TUI's command set is FINITE and mostly documented;
/// pressing only the bound keys (plus UNIVERSAL) spends budget on actions that
/// actually do something, instead of the full a-z alphabet where ~80% of keys
/// are no-ops in any given state. Returns key NAMES (matching KEYS).
fn app_keymap(cmdline: &str) -> Option<&'static [&'static str]> {
    // NB: deliberately NO plain-quit `q`. A clean quit just ends the session
    // and burns a relaunch (capping per-session depth); it isn't a command
    // worth prioritizing. Crash-triggering control keys (CtrlC/CtrlD) live in
    // UNIVERSAL instead, so the cancel-a-prompt panics are still reached.
    let c = cmdline.to_lowercase();
    if c.contains("jless") {
        // json viewer: vim-ish nav, fold/expand, search.
        Some(&[
            "j", "k", "h", "l", "Space", "Enter", "slash", "n", "i", "c", "e", "0", "dollar",
        ])
    } else if c.contains("gitui") {
        // git TUI: number keys switch tabs, letters act on the focused panel.
        Some(&[
            "Tab", "Enter", "Esc", "1", "2", "3", "4", "5", "Space", "c", "s", "d", "p",
        ])
    } else if c.contains("helix") || c.contains("/hx") || c.ends_with(" hx") || c == "hx" {
        // modal editor: i enters insert (text entry), Esc returns to normal.
        Some(&[
            "i", "Esc", "h", "j", "k", "l", "colon", "slash", "o", "x", "d", "u",
        ])
    } else if c.contains("lazygit") {
        Some(&["Tab", "Enter", "Space", "Esc", "x", "c", "s", "p", "d"])
    } else if c.contains("k9s") {
        Some(&["colon", "slash", "Enter", "Esc", "d", "l", "s"])
    } else if c.contains("htop") || c.contains("btop") {
        Some(&["Space", "slash", "Enter", "Esc", "u", "k"])
    } else if c.contains("less") || c.contains("moar") {
        Some(&["j", "k", "Space", "slash", "n", "g"])
    } else {
        None
    }
}

/// Scrape the visible screen for key hints the app advertises in its footer /
/// status bar: "q:quit  /:search  n:next", "[c] commit", "<Tab> switch". TUIs
/// are self-documenting; the keys they print ARE the bound ones. Catches the
/// app-specific commands no static table knows about. Returns key NAMES.
fn scrape_hint_keys(parser: &Arc<Mutex<vt100::Parser>>) -> BTreeSet<String> {
    let contents = parser.lock().unwrap().screen().contents();
    let chars: Vec<char> = contents.chars().collect();
    let mut keys = BTreeSet::new();
    for idx in 0..chars.len() {
        let ch = chars[idx];
        // pattern: a single isolated char immediately before ':' -> "q:quit".
        if ch == ':' && idx >= 1 {
            let k = chars[idx - 1];
            let isolated = idx < 2 || !chars[idx - 2].is_alphanumeric();
            // and the ':' is followed by a letter (a label), not another digit
            // (avoid matching clock "12:34" / ratios).
            let labelish = chars.get(idx + 1).is_some_and(|n| n.is_alphabetic());
            if isolated && labelish {
                if let Some(name) = char_to_keyname(k) {
                    keys.insert(name);
                }
            }
        }
        // pattern: [X] or <X> -> a bracketed single-char key hint.
        if (ch == ']' || ch == '>') && idx >= 2 {
            let open = chars[idx - 2];
            if open == '[' || open == '<' {
                if let Some(name) = char_to_keyname(chars[idx - 1]) {
                    keys.insert(name);
                }
            }
        }
    }
    keys
}

/// The action space for the current screen. Returns (all, bound):
///   all   = the FULL key alphabet as "key:Name" options. ALWAYS the complete
///           set, so a command-aware run never explores *less* than the old
///           uniform run; unbound keys stay reachable.
///   bound = the keys we have reason to believe DO something here (app keymap ∪
///           advertised footer hints ∪ UNIVERSAL nav/crash). These are
///           PRIORITIZED, not exclusive: ucb_pick tries every bound key before
///           any unbound one and keeps a small standing bonus on them, so the
///           finite real command set gets the budget first while the long tail
///           of letters is still eventually probed.
fn action_space(
    cmdline: &str,
    parser: &Arc<Mutex<vt100::Parser>>,
) -> (Vec<String>, BTreeSet<String>) {
    let all: Vec<String> = KEYS.iter().map(|(n, _)| format!("key:{n}")).collect();
    let mut bound: BTreeSet<String> = UNIVERSAL.iter().map(|s| format!("key:{s}")).collect();
    if let Some(km) = app_keymap(cmdline) {
        bound.extend(km.iter().map(|s| format!("key:{s}")));
    }
    bound.extend(scrape_hint_keys(parser).iter().map(|s| format!("key:{s}")));
    (all, bound)
}

/// UCB1 over the (state, action) arms, with epsilon-greedy focus on the bound
/// keys. For each arm, value = average reward (paid when an action discovers a
/// NEW state) and the explore term favors rarely-pulled arms; unpulled arms are
/// optimistic (taken first within their group). The keys split into two groups:
/// BOUND (app keymap / footer hints / nav-and-crash) and the unbound long tail
/// of letters. With probability 1-EPS we pick the best BOUND arm (the finite
/// real command set gets the bulk of the budget); with probability EPS we pick
/// the best UNBOUND arm, so the long tail is still swept and coverage is never
/// worse than the uniform alphabet. An empty bound set (the A/B uniform mode)
/// degrades cleanly to plain UCB1 over the flat alphabet. Prior cloud visit
/// counts (edge_weights) count as pulls so cross-run knowledge persists but
/// carry no reward. Tabular, no ML. Returns a "key:Name" option.
#[allow(clippy::too_many_arguments)] // tabular bandit state; a struct would obscure more than help
fn ucb_pick(
    actions: &[String],
    bound: &BTreeSet<String>,
    cur_sig: &str,
    live_visits: &BTreeMap<String, u64>,
    arm_reward: &BTreeMap<String, f64>,
    state_pulls: &BTreeMap<String, u64>,
    ew: Option<&BTreeMap<String, u64>>,
    eps: f64,
    rng: &mut Rng,
) -> String {
    const C: f64 = std::f64::consts::SQRT_2;
    let n_live = *state_pulls.get(cur_sig).unwrap_or(&0);
    let n_static: u64 = ew.map(|m| m.values().sum()).unwrap_or(0);
    let ln_n = ((1 + n_live + n_static) as f64).ln().max(0.0);
    let score = |opt: &str, jitter: f64| -> f64 {
        let key = format!("{cur_sig}|{opt}");
        let live = *live_visits.get(&key).unwrap_or(&0);
        let stat = ew.and_then(|m| m.get(opt)).copied().unwrap_or(0);
        let n = live + stat;
        if n == 0 {
            1e9 + jitter
        } else {
            let exploit = if live > 0 {
                arm_reward.get(&key).copied().unwrap_or(0.0) / live as f64
            } else {
                0.0
            };
            exploit + C * (ln_n / n as f64).sqrt() + jitter
        }
    };
    // Decide which group to draw from this step (epsilon-greedy). Fall back to
    // the other group if the chosen one is empty.
    let want_unbound = rng.unit() < eps;
    let mut best: Option<(f64, String)> = None;
    let consider = |opt: &String, jit: f64, best: &mut Option<(f64, String)>| {
        let s = score(opt, jit);
        if best.as_ref().is_none_or(|(b, _)| s > *b) {
            *best = Some((s, opt.clone()));
        }
    };
    for opt in actions {
        let is_bound = bound.contains(opt);
        let in_group = if want_unbound { !is_bound } else { is_bound };
        if in_group {
            let jit = rng.unit() * 1e-6;
            consider(opt, jit, &mut best);
        }
    }
    // chosen group empty (e.g. all keys bound, or uniform mode wanting bound):
    // fall back to the full set.
    if best.is_none() {
        for opt in actions {
            let jit = rng.unit() * 1e-6;
            consider(opt, jit, &mut best);
        }
    }
    best.map(|(_, o)| o)
        .unwrap_or_else(|| "key:Down".to_string())
}

fn edge_key(sig: &str, action: &str) -> String {
    format!("{sig}|{action}")
}

fn ordered_actions(space: &[String], bound: &BTreeSet<String>) -> Vec<String> {
    space
        .iter()
        .filter(|o| bound.contains(*o))
        .chain(space.iter().filter(|o| !bound.contains(*o)))
        .cloned()
        .collect()
}

fn is_crash_trigger(action: &str) -> bool {
    matches!(action, "key:CtrlC" | "key:CtrlD")
}

/// The byte sequence a `key:<Name>` action sends. Arrow keys honor the app's
/// cursor-key mode (DECCKM): SS3 (`ESC O B`) when the app called keypad()/smkx,
/// else CSI (`ESC [ B`). Unknown names yield no bytes (the caller decides
/// whether that is a MISS). Shared by the fuzz loop and the scenario actor so
/// both press keys identically.
fn bytes_for_key(parser: &Arc<Mutex<vt100::Parser>>, key_name: &str) -> Vec<u8> {
    match key_name {
        "Up" | "Down" | "Right" | "Left" => {
            let app = parser.lock().unwrap().screen().application_cursor();
            let c = match key_name {
                "Up" => 'A',
                "Down" => 'B',
                "Right" => 'C',
                _ => 'D',
            };
            if app {
                format!("\x1bO{c}").into_bytes()
            } else {
                format!("\x1b[{c}").into_bytes()
            }
        }
        _ => KEYS
            .iter()
            .find(|(n, _)| *n == key_name)
            .map(|(_, b)| b.as_bytes().to_vec())
            .unwrap_or_default(),
    }
}

fn remember_actions(
    actions_by_state: &mut BTreeMap<String, Vec<String>>,
    sig: &str,
    actions: Vec<String>,
) {
    let known = actions_by_state.entry(sig.to_string()).or_default();
    for action in actions {
        if !known.contains(&action) {
            known.push(action);
        }
    }
}

fn first_untried_action(
    actions_by_state: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
    sig: &str,
) -> Option<String> {
    actions_by_state.get(sig).and_then(|actions| {
        actions
            .iter()
            .find(|action| !tried.contains(&edge_key(sig, action)))
            .cloned()
    })
}

fn has_frontier(
    actions_by_state: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
) -> bool {
    actions_by_state
        .keys()
        .any(|sig| first_untried_action(actions_by_state, tried, sig).is_some())
}

fn remember_edge(
    graph: &mut BTreeMap<String, Vec<(String, String)>>,
    from: &str,
    action: &str,
    to: &str,
) {
    let edges = graph.entry(from.to_string()).or_default();
    if !edges.iter().any(|(a, t)| a == action && t == to) {
        edges.push((action.to_string(), to.to_string()));
    }
}

fn path_to_frontier(
    graph: &BTreeMap<String, Vec<(String, String)>>,
    actions_by_state: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
    from: &str,
) -> Option<Vec<String>> {
    if first_untried_action(actions_by_state, tried, from).is_some() {
        return Some(Vec::new());
    }
    let mut seen = BTreeSet::new();
    let mut q = std::collections::VecDeque::new();
    seen.insert(from.to_string());
    q.push_back((from.to_string(), Vec::<String>::new()));
    while let Some((sig, path)) = q.pop_front() {
        if let Some(edges) = graph.get(&sig) {
            for (action, to) in edges {
                if !seen.insert(to.clone()) {
                    continue;
                }
                let mut next_path = path.clone();
                next_path.push(action.clone());
                if first_untried_action(actions_by_state, tried, to).is_some() {
                    return Some(next_path);
                }
                q.push_back((to.clone(), next_path));
            }
        }
    }
    None
}

fn emit(s: &str) {
    println!("{s}");
    let _ = std::io::stdout().flush();
}

// ── APP-INVARIANT oracle (EXPLORE:INVARIANT, SDK-self-triggered) ────────────
//
// The app declares its own predicates via the reproit SDK (`ReproIt.invariant(
// "id", fn)`). Under the fuzzer the SDK evaluates them on its state-observe hook
// and reports the FAILURES on a diagnostic channel as a marker line
//   REPROIT_INVARIANT {"sig":"<sig-or-empty>","items":[{"id","message"}...]}
// This backend maps each marker into the CLI wire line the engine parses,
//   EXPLORE:INVARIANT {"sig":"<runner sig>","items":[...]}
// keyed on the state signature the runner is currently on (map.rs substitutes
// nothing; the runner owns the sig), de-duped per state.
//
// CHANNEL (why a runner-provisioned side file, not stderr): the contract prefers
// stderr for TUI, but a PTY is the exception it anticipates. The child's stdout
// AND stderr are dup'd onto the same slave, so both ARE the rendered-frame byte
// stream this backend parses into the VT grid; there is no stderr separable from
// the frames. Worse, that stream is load-bearing for the crash oracle
// (`looks_crashed` scans the grid for a rendered "panicked at" that reaches the
// screen ONLY because a panic prints to stderr). A marker on stderr would
// corrupt the very frame we measure and be indistinguishable from a crash
// render. So the runner provisions a per-run file, hands its path to every
// launched session via `REPROIT_INVARIANT_FILE` (which is ALSO the SDK's
// fuzzer-detection gate: absent in production, the registry stays inert), and
// scrapes it here. This is a genuine PORT: stderr is conflated with frames, the
// file is not.

/// Path to this `reproit __tui` process's invariant marker file (per-pid, so
/// concurrent runners never share one). Provisioned once; handed to each session.
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
/// cell grid contains no retained widget metadata, so the SDK writes declarations
/// to this runner-owned side channel. It is non-visual, locale-independent, and
/// exists only while reproit launches the app.
fn input_file_path() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        std::env::temp_dir()
            .join(format!("reproit-inputs-{}.ndjson", std::process::id()))
            .to_string_lossy()
            .into_owned()
    })
}

fn structural_input_elements() -> Vec<serde_json::Value> {
    let Ok(raw) = std::fs::read_to_string(input_file_path()) else {
        return Vec::new();
    };
    let mut by_selector = BTreeMap::<String, String>::new();
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(sel) = v.get("sel").and_then(|x| x.as_str()) else {
            continue;
        };
        let Some(purpose) = v.get("inputPurpose").and_then(|x| x.as_str()) else {
            continue;
        };
        if let Some(canonical) = crate::appmap::normalize_input_purpose(Some(purpose), sel) {
            by_selector.insert(sel.to_string(), canonical);
        }
    }
    by_selector
        .into_iter()
        .map(|(sel, input_purpose)| {
            serde_json::json!({
                "sel": sel, "role": "textfield", "label": "", "inputPurpose": input_purpose
            })
        })
        .collect()
}

/// Parse one line for the SDK marker `REPROIT_INVARIANT {json}`. Returns
/// `(sig, items)` where `items` is the list of VIOLATED `(id, message)` pairs and
/// `sig` is the SDK's own signature (or empty when it does not know it). `None`
/// for a non-marker line, malformed json, or an empty item list, so a clean
/// settle (no marker) and a garbled line both stay silent.
fn parse_invariant_marker(line: &str) -> Option<(String, Vec<(String, String)>)> {
    const MARK: &str = "REPROIT_INVARIANT ";
    let idx = line.find(MARK)?;
    let json: serde_json::Value = serde_json::from_str(line[idx + MARK.len()..].trim()).ok()?;
    let items: Vec<(String, String)> = json
        .get("items")?
        .as_array()?
        .iter()
        .filter_map(|it| {
            let id = it.get("id").and_then(|v| v.as_str())?.to_string();
            let message = it
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((id, message))
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let sig = json
        .get("sig")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((sig, items))
}

/// Incrementally scrapes the marker file and hands the runner the violations to
/// re-emit for its current state. The SDK and the runner compute the SAME
/// canonical TUI signature (reproit-tui-sig, golden-pinned), so a marker that
/// carries the SDK's own sig is matched to the runner's identical sig; an
/// empty-sig marker is attributed to the runner's next observed state. Per-sig
/// de-dup keeps a standing violation from being reported on every settle.
struct InvariantScrape {
    path: String,
    offset: u64,
    pending: Vec<u8>, // bytes of a not-yet-terminated trailing line across reads
    by_sig: BTreeMap<String, Vec<(String, String)>>,
    fallback: Option<Vec<(String, String)>>,
    emitted: BTreeSet<String>,
}

impl InvariantScrape {
    fn new(path: &str) -> Self {
        InvariantScrape {
            path: path.to_string(),
            offset: 0,
            pending: Vec::new(),
            by_sig: BTreeMap::new(),
            fallback: None,
            emitted: BTreeSet::new(),
        }
    }

    /// Fold any newly appended marker lines into the pending maps. Reads bytes
    /// (not a String) and decodes only COMPLETE lines, so a read that lands
    /// mid-codepoint or mid-line never drops a marker.
    fn ingest(&mut self) {
        use std::io::{Read as _, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return;
        };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        let Ok(n) = f.read_to_end(&mut buf) else {
            return;
        };
        self.offset += n as u64;
        self.pending.extend_from_slice(&buf);
        while let Some(nl) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=nl).collect();
            let text = String::from_utf8_lossy(&line);
            if let Some((sig, items)) = parse_invariant_marker(&text) {
                if sig.is_empty() {
                    self.fallback = Some(items);
                } else {
                    self.by_sig.insert(sig, items);
                }
            }
        }
    }

    /// The violations to report for `sig`, once (ingesting first). `None` when
    /// the app registered no failing invariant for this state, or it was already
    /// reported (per-sig de-dup).
    fn pending_for(&mut self, sig: &str) -> Option<Vec<(String, String)>> {
        self.ingest();
        let items = self
            .by_sig
            .get(sig)
            .cloned()
            .or_else(|| self.fallback.take());
        let items = items?;
        if items.is_empty() || !self.emitted.insert(sig.to_string()) {
            return None;
        }
        Some(items)
    }

    /// Re-emit `EXPLORE:INVARIANT` for `sig` if the app reported a violation there.
    fn flush_for(&mut self, sig: &str) {
        let Some(items) = self.pending_for(sig) else {
            return;
        };
        let arr: Vec<serde_json::Value> = items
            .iter()
            .map(|(id, message)| serde_json::json!({ "id": id, "message": message }))
            .collect();
        emit(&format!(
            "EXPLORE:INVARIANT {}",
            serde_json::json!({ "sig": sig, "items": arr })
        ));
    }
}

/// The target child's resident set size (RSS) in BYTES, or None on failure. RSS
/// is the OS process analogue of the web runner's v8 `heap_used`: the soak oracle
/// (modes/soak.rs) reads first-vs-last to compute the per-cycle slope. Linux reads
/// `VmRSS` from `/proc/<pid>/status` (reported in kB); every other unix reads
/// `ps -o rss= -p <pid>` (reported in KiB on macOS/BSD), matching how the AppKit /
/// AT-SPI desktop runners sample. A pure read of the OS, so the same process state
/// yields the same number; never taken outside soak (replay) mode.
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

/// Sanitize a shoot name to the contract's `[A-Za-z0-9_/-]` alphabet, matching
/// the orchestrator-side filter in drive.rs so the runner writes the same path
/// the orchestrator looks for.
fn sanitize_shot_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-'))
        .collect()
}

/// Screenshot-capture contract (see backends/drive.rs): render the CURRENT
/// vt100 screen to `$REPROIT_SHOTS_DIR/<name>.png`, then print `SHOOT:<name>`.
/// If REPROIT_SHOTS_DIR is unset we skip the PNG but still print the marker, so
/// the journey timeline still records the shoot point. A leading dir in `<name>`
/// (the `/` in the alphabet) is created under the shots dir.
fn shoot(parser: &Arc<Mutex<vt100::Parser>>, raw_name: &str) {
    let name = sanitize_shot_name(raw_name);
    if name.is_empty() {
        return;
    }
    if let Ok(dir) = std::env::var("REPROIT_SHOTS_DIR") {
        if !dir.is_empty() {
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let img = shot::render_screen(parser);
            if let Err(e) = img.save(&path) {
                emit(&format!("JOURNEY[a] step: shoot {name} render failed: {e}"));
            }
        }
    }
    // Print the marker regardless: the orchestrator confirms the PNG (RunnerSide
    // capture) and logs the shoot point either way.
    emit(&format!("SHOOT:{name}"));
}

// ── --record clip capture (video + finding box) ────────────────────────────
//
// Every finding gets a filmed clip with a red box on the offending element, on
// EVERY backend. The TUI "window" IS its own rendered cell buffer, so there is
// NO OS screen capture and no privacy concern (unlike the desktop runners that
// film a real window): we compose the video from the very frames render_screen
// already produces during the replay, then draw the box post-capture with the
// shared box-overlay.mjs (the uniform path for backends that cannot inject a live
// DOM overlay). This mirrors the macOS-AX runner's startClipCapture / finalize.

/// Parse a positional clip selector into `(row, Some(col))` (a cell anchor) or
/// `(row, None)` (the whole row). Accepts the position-key shapes the TUI oracles
/// emit -- `pos:R,C`, `region:R,C`, `row:R,C`, `row:R`, or a bare `R,C` / `R` --
/// by taking the text after the last `:` and splitting on `,`. Returns None when
/// there is no numeric row (e.g. a label-style selector that has no cell), so the
/// caller reports drew:false rather than boxing the wrong place.
fn parse_sel_pos(sel: &str) -> Option<(usize, Option<usize>)> {
    let body = sel.rsplit(':').next().unwrap_or(sel);
    let mut it = body.split(',');
    let r: usize = it.next()?.trim().parse().ok()?;
    let c = it.next().and_then(|s| s.trim().parse::<usize>().ok());
    Some((r, c))
}

/// Resolve a clip selector to a CELL rect on the CURRENT screen, in the video's
/// own pixel space (x=col*CELL_W, y=row*CELL_H, ...). With a column anchor we box
/// the element's text extent around it, tolerating SINGLE-space gaps so a menu
/// label like `Toggle Sound` boxes as one run (a two-space gap ends the run); an
/// anchor on a blank cell snaps to the nearest ink on that row. With no column we
/// box the row's whole non-blank extent. Returns None when the row is off-screen
/// or entirely blank, matching the "element couldn't be located" -> drew:false path.
fn resolve_clip_rect(
    parser: &Arc<Mutex<vt100::Parser>>,
    sel: &str,
) -> Option<(u32, u32, u32, u32)> {
    let (r, c_opt) = parse_sel_pos(sel)?;
    let grid = grid_of(parser);
    if r >= grid.len() {
        return None;
    }
    let row = &grid[r];
    let n = row.len();
    let is_ink = |i: usize| i < n && !row[i].is_whitespace();
    let (c0, c1) = match c_opt {
        Some(c) => {
            // Anchor on ink; if the exact cell is blank, snap to the nearest ink
            // on this row so a slightly-off column still boxes the element.
            let anchor = if is_ink(c) {
                c
            } else {
                (0..n)
                    .filter(|&i| is_ink(i))
                    .min_by_key(|&i| (i as isize - c as isize).unsigned_abs())?
            };
            // Grow left/right across ink, stepping over a lone space (but not two).
            let mut lo = anchor;
            while lo > 0 && (is_ink(lo - 1) || (lo >= 2 && is_ink(lo - 2))) {
                lo -= 1;
            }
            let mut hi = anchor;
            while hi + 1 < n && (is_ink(hi + 1) || (hi + 2 < n && is_ink(hi + 2))) {
                hi += 1;
            }
            (lo, hi)
        }
        None => {
            let lo = (0..n).find(|&i| is_ink(i))?;
            let hi = (0..n).rev().find(|&i| is_ink(i))?;
            (lo, hi)
        }
    };
    let x = c0 as u32 * shot::CELL_W;
    let y = r as u32 * shot::CELL_H;
    let w = (c1 - c0 + 1) as u32 * shot::CELL_W;
    let h = shot::CELL_H;
    Some((x, y, w, h))
}

/// Assemble a numbered PNG sequence (`frame%04d.png`) into `out` at `fps`. The
/// yuv420p pixel format needs even dimensions; a CELL_W/CELL_H (8x16) grid is
/// always even, so no padding is needed. Returns whether ffmpeg succeeded.
fn assemble_clip(frames_dir: &std::path::Path, fps: u32, out: &std::path::Path) -> bool {
    let pattern = frames_dir.join("frame%04d.png");
    std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-framerate",
            &fps.to_string(),
            "-i",
        ])
        .arg(&pattern)
        .args(["-pix_fmt", "yuv420p"])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The --record clip capture state for one replay. Frames are written as PNGs and
/// assembled to `$REPROIT_VIDEO_DIR/clip.mov`; the finding's cell rect + a time
/// window go to `box-spec.json`; a `FINDING:BOXED` marker reports whether the box
/// drew, all in the video's own px/sec space (logical == px here, so box-overlay
/// scales by 1).
struct ClipCapture {
    video_dir: std::path::PathBuf,
    frames_dir: std::path::PathBuf,
    fps: u32,
    count: usize,
    video_w: u32,
    video_h: u32,
    /// Capture-relative time (s) of the frame right after the triggering action.
    trigger_time: f64,
    /// The finding element's rect (px), resolved at the triggering action.
    rect: Option<(u32, u32, u32, u32)>,
    sel: String,
    label: String,
    oracle: String,
}

impl ClipCapture {
    /// Arm capture if REPROIT_VIDEO_DIR is set (the caller also gates on a replay
    /// being present). Creates the frames scratch dir under the video dir.
    fn arm(clip: &Clip) -> Option<Self> {
        let dir = std::env::var("REPROIT_VIDEO_DIR")
            .ok()
            .filter(|s| !s.is_empty())?;
        let video_dir = std::path::PathBuf::from(dir);
        let frames_dir = video_dir.join("frames");
        std::fs::create_dir_all(&frames_dir).ok()?;
        Some(ClipCapture {
            video_dir,
            frames_dir,
            // ~260ms per replayed action settle -> ~4 fps tracks real time.
            fps: 4,
            count: 0,
            video_w: 0,
            video_h: 0,
            trigger_time: 0.0,
            rect: None,
            sel: clip.sel.clone(),
            label: clip.label.clone(),
            oracle: clip.oracle.clone(),
        })
    }

    /// Render the current screen and append it as the next frame.
    fn capture(&mut self, parser: &Arc<Mutex<vt100::Parser>>) {
        let img = shot::render_screen(parser);
        if self.count == 0 {
            self.video_w = img.width();
            self.video_h = img.height();
        }
        let path = self.frames_dir.join(format!("frame{:04}.png", self.count));
        if img.save(&path).is_ok() {
            self.count += 1;
        }
    }

    /// Capture-relative time (s) of the most recently captured frame.
    fn last_time(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            (self.count - 1) as f64 / self.fps as f64
        }
    }

    /// Mark the just-executed action as the finding's trigger: record its frame
    /// time and resolve the sel to a cell rect from the settled screen (freshest
    /// at the tap, exactly as the macOS-AX runner grabs the element handle there).
    fn mark_trigger(&mut self, parser: &Arc<Mutex<vt100::Parser>>) {
        self.trigger_time = self.last_time();
        if let Some(rect) = resolve_clip_rect(parser, &self.sel) {
            self.rect = Some(rect);
        }
    }

    /// Assemble clip.mov, write box-spec.json, and emit FINDING:BOXED. Pads the
    /// tail by holding the last frame so the box stays visible in the final second
    /// (a screen recording keeps rolling after the action; the verifier grabs the
    /// last frame). drew=false when nothing filmed or the element never resolved.
    fn finalize(&mut self) {
        if self.count > 0 {
            let last = self
                .frames_dir
                .join(format!("frame{:04}.png", self.count - 1));
            for _ in 0..(self.fps * 2) {
                let dst = self.frames_dir.join(format!("frame{:04}.png", self.count));
                if std::fs::copy(&last, &dst).is_ok() {
                    self.count += 1;
                } else {
                    break;
                }
            }
        }
        let mov = self.video_dir.join("clip.mov");
        let assembled = self.count > 0 && assemble_clip(&self.frames_dir, self.fps, &mov);
        let mut drew = false;
        if assembled {
            if let Some((x, y, w, h)) = self.rect {
                let t0 = (self.trigger_time - 0.3).max(0.0);
                let spec = serde_json::json!({
                    "videoW": self.video_w,
                    "videoH": self.video_h,
                    "boxes": [{
                        "x": x, "y": y, "w": w, "h": h,
                        "tStart": t0, "tEnd": 1e9,
                        "label": self.label, "color": "red",
                    }],
                });
                let spec_path = self.video_dir.join("box-spec.json");
                if std::fs::write(&spec_path, spec.to_string()).is_ok() {
                    drew = true;
                }
            }
        }
        emit(&format!(
            "FINDING:BOXED {}",
            serde_json::json!({
                "oracle": self.oracle,
                "sel": self.sel,
                "mov": mov.to_string_lossy(),
                "drew": drew,
            })
        ));
    }
}

/// --record clip plan (replay mode only). When present AND REPROIT_VIDEO_DIR is
/// set, the driver assembles the frames it renders during the replay into
/// clip.mov and, after the replay settles, resolves the finding's `sel` to a CELL
/// rect of the offending screen region, writing box-spec.json next to clip.mov so
/// the host box-overlay step draws the red finding box (the uniform post-capture
/// path every non-DOM backend shares).
struct Clip {
    /// A positional selector for the finding's screen region, e.g. `pos:R,C`,
    /// `region:R,C`, or `row:R`. Mapped to a cell rect on the settled screen.
    sel: String,
    /// Caption text drawn on the box.
    label: String,
    /// Oracle id, echoed back on the FINDING:BOXED marker.
    oracle: String,
}

struct Fuzz {
    seed: u32,
    budget: u32,
    configured: bool,
    replay: Option<Vec<String>>,
    prefix: Option<Vec<String>>,
    edge_weights: BTreeMap<String, BTreeMap<String, u64>>,
    /// --record clip plan (see Clip); armed only alongside a replay.
    clip: Option<Clip>,
    // Production-seeded corpus: real user paths (from SDK telemetry) to replay
    // into a realistic deep state, then BRANCH outward from. Bugs cluster where
    // users actually go, and the costly part of fuzzing is reaching a valid deep
    // state, so a real path teleports us there for free.
    seeds: Vec<Vec<String>>,
}

fn load_fuzz() -> Fuzz {
    let mut f = Fuzz {
        seed: 0,
        budget: ACTION_BUDGET,
        configured: false,
        replay: None,
        prefix: None,
        edge_weights: BTreeMap::new(),
        clip: None,
        seeds: Vec::new(),
    };
    let Ok(path) = std::env::var("REPROIT_FUZZ_CONFIG") else {
        return f;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return f;
    };
    let Ok(j) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return f;
    };
    f.configured = true;
    if let Some(s) = j.get("seed").and_then(|v| v.as_u64()) {
        f.seed = s as u32;
    }
    if let Some(b) = j.get("budget").and_then(|v| v.as_u64()) {
        f.budget = b as u32;
    }
    f.replay = j.get("replay").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    });
    f.prefix = j.get("prefix").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    });
    // --record clip plan: {"clip":{"sel","label","oracle"}}. Only meaningful in
    // replay mode with REPROIT_VIDEO_DIR set; the driver checks both before arming.
    if let Some(c) = j.get("clip").and_then(|v| v.as_object()) {
        if let Some(sel) = c.get("sel").and_then(|v| v.as_str()) {
            f.clip = Some(Clip {
                sel: sel.to_string(),
                label: c
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("finding")
                    .to_string(),
                oracle: c
                    .get("oracle")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    if let Some(ew) = j.get("edgeWeights").and_then(|v| v.as_object()) {
        for (sig, m) in ew {
            if let Some(mm) = m.as_object() {
                let inner = mm
                    .iter()
                    .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                    .collect();
                f.edge_weights.insert(sig.clone(), inner);
            }
        }
    }
    // seeds: a corpus of real user paths (each an array of "key:Name" actions),
    // typically lifted from production SDK telemetry. We branch outward from
    // these instead of always launching cold.
    if let Some(arr) = j.get("seeds").and_then(|v| v.as_array()) {
        for path in arr {
            if let Some(steps) = path.as_array() {
                let p: Vec<String> = steps
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                if !p.is_empty() {
                    f.seeds.push(p);
                }
            }
        }
    }
    f
}

/// xorshift32, same recurrence as every other runner; high-bit reduction so
/// small alphabets don't hit the low-bit weakness.
struct Rng {
    s: u32,
}
impl Rng {
    fn new(seed: u32) -> Self {
        Rng {
            s: if seed == 0 { 1 } else { seed },
        }
    }
    fn step(&mut self) -> u32 {
        self.s ^= self.s << 13;
        self.s ^= self.s >> 17;
        self.s ^= self.s << 5;
        self.s
    }
    fn unit(&mut self) -> f64 {
        (self.step() as f64) / (u32::MAX as f64)
    }
}

/// The visible screen as (signature, fingerprint, labels).
///
/// SIGNATURE: built from the LAYOUT SKELETON (`skeleton_of`) PLUS a bounded
/// numeric value-class section (`numeric_value_classes`). Box-drawing borders,
/// field/gap extents, digit and symbol positions, and the cursor position are
/// structural and locale-invariant; natural-language words are collapsed to a
/// placeholder before hashing. The numeric value-classes give value-state apps
/// (a counter, a clock, a calculator) a few distinct states instead of one
/// frozen skeleton. The same screen rendered in English and German hashes to the
/// same node (docs/cli.md hard invariant), because value-classes are buckets, not
/// raw values, and the strict-decimal rule is locale-safe.
///
/// FINGERPRINT: a runner-local content fingerprint over the FULL screen text
/// (the actual rendered cells, digits and words included). This is the TUI
/// analogue of Layer 1 effect detection: it changes whenever any on-screen value
/// changes, even when the skeleton signature does not, so the explorer never
/// stalls on a value-only update (a counter incrementing). It is ephemeral and
/// NEVER enters the canonical state identity (`seen`); it only answers "did the
/// action do anything" (docs/signature.md, "Terminal and instrumented surfaces").
///
/// LABELS: unchanged, the human-facing word set (display only). Full-screen TUIs
/// are wide box-drawing grids; tokenizing after blanking box glyphs yields a
/// stable label set for narrow (jless) and wide (gitui) UIs alike. These feed
/// `map show` and never the signature.
fn snapshot(parser: &Arc<Mutex<vt100::Parser>>) -> (String, String, Vec<String>) {
    let (contents, cursor) = {
        let p = parser.lock().unwrap();
        let s = p.screen();
        // A hidden cursor's write position changes as a TUI repaints and does
        // not represent user focus. Only a visible cursor belongs in identity.
        let cursor = if s.hide_cursor() {
            (0, 0)
        } else {
            s.cursor_position()
        };
        (s.contents(), cursor)
    };
    let sig = structural_sig(&contents, cursor);
    let fp = content_fingerprint(&contents, cursor);
    let labels = labels_of(&contents);
    (sig, fp, labels)
}

// ── Operability / accessibility signals (EXPLORE:GROUNDTRUTH) ──────────────
//
// A TUI has ONE input channel: keystrokes. So the operability "graph 1" (what a
// user can actually do) and the keyboard/a11y "graph 2" coincide for the normal
// case. A grounded gap appears only when an SGR mouse-operable region cannot be
// reached through the keyboard. Missing nearby text is not evidence of a missing
// role or accessible name and is deliberately ignored.
//
//   Mouse-only signal (gated by REPROIT_TUI_MOUSE=1): we drive SGR mouse
//   clicks at deterministic hotspots (bracketed `[ Save ]`, reverse-video runs,
//   footer hint tokens). A state reached by a click but by NO keystroke is
//   mouse-only / not keyboard-operable: operable:true, a11y.inTabOrder:false +
//   keyboardActivatable:false (the engine counts these -> keyboard_unreachable +
//   pointer_only).

/// A snapshot of the visible cell grid as a row-major char matrix (one char per
/// cell; wide-char continuations and empty cells render as a space). Used to
/// locate the DIFF RECTANGLE between two frames and to scan a sub-region for
/// word runs, both of which need cell coordinates that `contents()` (a single
/// newline-joined string with trailing blanks trimmed) does not preserve.
fn grid_of(parser: &Arc<Mutex<vt100::Parser>>) -> Vec<Vec<char>> {
    let p = parser.lock().unwrap();
    let screen = p.screen();
    let (rows, cols) = screen.size();
    let mut grid = vec![vec![' '; cols as usize]; rows as usize];
    for r in 0..rows {
        for c in 0..cols {
            if let Some(cell) = screen.cell(r, c) {
                let s = cell.contents();
                grid[r as usize][c as usize] = s.chars().next().unwrap_or(' ');
            }
        }
    }
    grid
}

/// One broken-content artifact found on the settled screen: the offending
/// position (a stable `pos:R,C` key), the artifact class, and the clipped text.
/// Serialized into the `items` array of an `EXPLORE:CONTENTBUG` line.
struct ContentBug {
    /// `pos:R,C` of the match start (0-based row, col). Stable for a fixed
    /// settled screen, so the finding id is the same across runs and replays.
    key: String,
    /// The high-confidence artifact class: `object-object` or
    /// `unrendered-template`.
    reason: &'static str,
    /// The clipped offending text (human detail; key+reason are the identity).
    text: String,
}

/// CONTENT-BUG oracle (deterministic, settled-screen text scan). The TUI analogue
/// of the web runner's `detectContentBugs`, restricted to artifacts that remain
/// unambiguous without DOM/accessibility semantics:
///   - `[object Object]`      : an object coerced to a string (the canonical bug)
///   - `{{ ... }}` / `${ ... }`: an unrendered template placeholder (binding never ran)
/// Bare `undefined`, `null`, and `NaN` are valid data/code values in JSON viewers,
/// logs, editors, and dashboards. A terminal grid cannot determine their origin,
/// so treating them as defects creates deterministic false positives.
/// We scan the SETTLED cell grid row by row (each row is one logical text run, so
/// a wrapped artifact is not stitched across rows, matching how a TUI paints), and
/// key each finding by the `pos:R,C` of the match start, deduped by (key, reason).
/// Pure function of the grid, so the same settled screen yields the same findings
/// on every run and on replay (no timing, no pixels). A clean screen renders none
/// of these, so the control stays silent (no marker). The bracketed/`{{}}`/`${}`
/// classes are matched as substrings.
fn detect_content_bugs(grid: &[Vec<char>]) -> Vec<ContentBug> {
    const OBJ: &[char] = &[
        '[', 'o', 'b', 'j', 'e', 'c', 't', ' ', 'O', 'b', 'j', 'e', 'c', 't', ']',
    ];
    let mut out: Vec<ContentBug> = Vec::new();
    let mut seen: BTreeSet<(String, &'static str)> = BTreeSet::new();
    let mut push = |row: usize, col: usize, reason: &'static str, text: String| {
        let key = format!("pos:{row},{col}");
        if seen.insert((key.clone(), reason)) {
            out.push(ContentBug { key, reason, text });
        }
    };
    // The clipped human-detail text starting at a column (bounded length).
    let snippet = |row: &[char], col: usize| -> String {
        row[col..(col + 40).min(row.len())].iter().collect()
    };
    for (r, row) in grid.iter().enumerate() {
        let n = row.len();
        let mut c = 0usize;
        while c < n {
            // first-match-wins, same precedence order as the web classifier.
            if c + OBJ.len() <= n && row[c..c + OBJ.len()] == *OBJ {
                push(r, c, "object-object", snippet(row, c));
                c += OBJ.len();
                continue;
            }
            // `{{ ... }}` on the same row: a `{{` with a closing `}}` after it.
            if c + 1 < n && row[c] == '{' && row[c + 1] == '{' {
                if let Some(end) =
                    (c + 2..n).find(|&k| row[k] == '}' && k + 1 < n && row[k + 1] == '}')
                {
                    push(r, c, "unrendered-template", snippet(row, c));
                    c = end + 2;
                    continue;
                }
            }
            // `${ ... }` on the same row: a `${` with a closing `}` after it.
            if c + 1 < n && row[c] == '$' && row[c + 1] == '{' {
                if let Some(end) = (c + 2..n).find(|&k| row[k] == '}') {
                    push(r, c, "unrendered-template", snippet(row, c));
                    c = end + 1;
                    continue;
                }
            }
            c += 1;
        }
    }
    // Stable order: by key then reason, so the marker is byte-identical run to run.
    out.sort_by(|a, b| a.key.cmp(&b.key).then(a.reason.cmp(b.reason)));
    out
}

/// BROKEN-ASSET oracle (tofu: rendered U+FFFD, settled-screen text scan). The
/// TUI slice of the web runner's `brokenAssetScan`: a cell rendering the U+FFFD
/// replacement character is broken text encoding reaching the screen. U+FFFD is
/// what a decoder emits on malformed input, never a glyph an app paints on
/// purpose, so the test is a pure cell check with no false positives. A
/// terminal has no images and no font loads, so tofu is the only broken-asset
/// class here (the img/font classes stay web-only). Each finding is keyed by
/// the `pos:R,C` of the offending cell (stable for a fixed settled screen) with
/// a short clipped excerpt around the char as the human detail. Pure function
/// of the grid, so the same settled screen yields the same findings on every
/// run and on replay; a clean screen yields nothing (no marker). Capped so a
/// screen full of mojibake cannot flood the marker.
fn detect_tofu(grid: &[Vec<char>]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (r, row) in grid.iter().enumerate() {
        for (c, &ch) in row.iter().enumerate() {
            if ch != '\u{FFFD}' {
                continue;
            }
            let start = c.saturating_sub(20);
            let end = (c + 21).min(row.len());
            let excerpt: String = row[start..end].iter().collect();
            out.push((format!("pos:{r},{c}"), excerpt.trim().to_string()));
            if out.len() >= 20 {
                break;
            }
        }
        if out.len() >= 20 {
            break;
        }
    }
    // Stable order: by key, so the marker is byte-identical run to run.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// DYNAMIC-TYPE and SCROLL-ROUND-TRIP are excluded on the TUI tier, no ground
// truth. dynamic-type: a terminal has a FIXED character-cell grid and no OS text
// scale to bump -- "larger text" is the user's terminal font, outside the app,
// with no per-app transform to drive. scroll-round-trip: a TUI scrolls only
// through app-defined key handling (there is no scroll viewport with an exact
// offset to jump to and restore), so the same-content-at-same-offset identity
// cannot be driven deterministically. Both carry on the web + Flutter tiers.

/// Does the grid render ANY non-whitespace cell? The blank-screen oracle's
/// content test, and the source of its `seen_content` guard: only a screen the
/// app has actually painted on counts as content.
fn screen_has_ink(grid: &[Vec<char>]) -> bool {
    grid.iter().flatten().any(|c| !c.is_whitespace())
}

// SAFE-AREA oracle: EXCLUDED on the TUI/desktop runners. A terminal grid (and a
// desktop window) has NO device safe-area inset -- there is no notch, status bar,
// Dynamic Island, or home indicator, so there is no inset geometry to measure a
// control against. The oracle is native-mobile only.
//
// PERMISSION-WALK oracle: EXCLUDED on the TUI/desktop runners. A terminal app has
// no runtime OS permission the runner can DENY (no camera/location grant flow),
// so there is no permission-denial sweep to run.

/// BLANK-SCREEN oracle (EXPLORE:BLANKSCREEN): the settled screen renders ZERO
/// non-whitespace cells while the PTY has non-zero size, the TUI analogue of
/// the web white-screen-of-death (the app cleared the screen and painted
/// nothing back). Guarded by `seen_content`: the app must have painted at least
/// one non-blank screen earlier in the run, so an app that simply has not drawn
/// yet (a slow boot) never fires. Returns the `(w, h)` of the blank grid to
/// carry in the marker item, or None when the screen shows content, the PTY is
/// zero-sized, or no content was ever seen. Pure function of the grid + flag.
fn blank_screen_item(grid: &[Vec<char>], seen_content: bool) -> Option<(i64, i64)> {
    if !seen_content || screen_has_ink(grid) {
        return None;
    }
    let rows = grid.len();
    let cols = grid.first().map(|r| r.len()).unwrap_or(0);
    if rows == 0 || cols == 0 {
        return None;
    }
    Some((cols as i64, rows as i64))
}

/// How long to wait before re-sampling a screen that looked blank, so a whole-region
/// clear+repaint (an Ink-style app that wipes then redraws every frame) has time to
/// paint the new frame we would otherwise mistake for a blank screen.
const BLANK_RESAMPLE_MS: u64 = 120;

/// BLANK-SCREEN with persistence: given the settled sample and a re-sample taken a
/// short delay later, the screen is blank ONLY if BOTH samples are blank. Ink-style
/// apps clear-and-repaint their whole region every frame, so a single settled sample
/// can land on the all-whitespace transient between the clear and the repaint; the
/// same state then showed up both as BLANKSCREEN and as a GROUNDTRUTH with operable
/// regions in one fuzz run (a measured FP). If the re-sample has ANY ink we caught a
/// repaint gap, not a genuinely blank screen, so we stay silent. Returns the blank
/// grid's `(w, h)` (from the first sample) when both are blank, else None.
fn blank_screen_persisted(
    sample: &[Vec<char>],
    resample: &[Vec<char>],
    seen_content: bool,
) -> Option<(i64, i64)> {
    let item = blank_screen_item(sample, seen_content)?;
    // The re-sample must ALSO be blank; ink on it means the first was a transient.
    blank_screen_item(resample, seen_content)?;
    Some(item)
}

/// Is a row "persistent chrome": does it contain box-drawing border glyphs (the
/// frame/panes a full-screen TUI keeps painted across states)? Used by the
/// re-render oracle to name the anchors a wasteful full repaint tore down and
/// rebuilt unchanged.
fn is_chrome_row(row: &[char]) -> bool {
    row.iter()
        .any(|&ch| ('\u{2500}'..='\u{257f}').contains(&ch))
}

/// The persistent-chrome rows that survived a transition BYTE-IDENTICAL: rows
/// present and unchanged in both the pre- and post-action grids that carry
/// box-drawing chrome. When the app issued a full-screen erase on the action
/// (so it cleared and repainted everything), these unchanged chrome rows are the
/// ones it needlessly tore down and redrew, the VT analogue of the web runner's
/// reconciled-but-rebuilt anchors. Returns stable `row:R` keys (R is the 0-based
/// row), capped so a tall frame cannot flood the marker. Deterministic: a pure
/// function of the two grids.
fn churned_chrome_rows(pre: &[Vec<char>], post: &[Vec<char>], cap: usize) -> Vec<String> {
    let rows = pre.len().min(post.len());
    let mut out = Vec::new();
    for r in 0..rows {
        if pre[r] == post[r] && is_chrome_row(&pre[r]) {
            out.push(format!("row:{r}"));
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

/// One ground-truth element accumulated for a state. Serialized into the
/// `elements` array of an `EXPLORE:GROUNDTRUTH` line. The a11y dims default to
/// "present/true" at the engine and are only emitted when KNOWN-false, so we keep
/// the struct minimal and let serde omit the rest.
#[derive(Clone)]
struct GtElement {
    id: String,
    gesture_kind: &'static str,
    /// false => emit a11y.inTabOrder:false + keyboardActivatable:false.
    keyboard_operable: bool,
}

/// Accumulates `EXPLORE:GROUNDTRUTH` elements per state signature, and emits one
/// consolidated marker line per state whenever its element set changes. Keyed by
/// element id so a control rediscovered on a later visit does not double-count.
struct Groundtruth {
    /// sig -> (id -> element)
    by_state: BTreeMap<String, BTreeMap<String, GtElement>>,
    /// sigs that have a focus trap observed (none, for now: TUIs have no Tab-
    /// ring we can prove trapped, so this stays false and is here for parity).
    focus_trap: BTreeSet<String>,
}

impl Groundtruth {
    fn new() -> Self {
        Groundtruth {
            by_state: BTreeMap::new(),
            focus_trap: BTreeSet::new(),
        }
    }

    /// Record an element for `sig` and (re)emit the state's groundtruth line if
    /// the element was new or changed. Returns true if a line was emitted.
    fn record(&mut self, sig: &str, el: GtElement) -> bool {
        let map = self.by_state.entry(sig.to_string()).or_default();
        let changed = match map.get(&el.id) {
            Some(prev) => {
                prev.gesture_kind != el.gesture_kind
                    || prev.keyboard_operable != el.keyboard_operable
            }
            None => true,
        };
        if changed {
            map.insert(el.id.clone(), el);
            self.emit(sig);
        }
        changed
    }

    /// Emit `EXPLORE:GROUNDTRUTH` for one state. The `sig` is byte-identical to
    /// the `EXPLORE:STATE` sig so the engine keys the gaps to the same node. Each
    /// element carries `operable:true` plus the a11y dims that are KNOWN-false;
    /// dims left out default to true at the engine, so we only ever ASSERT a
    /// failure we actually observed.
    fn emit(&self, sig: &str) {
        let Some(map) = self.by_state.get(sig) else {
            return;
        };
        let elements: Vec<serde_json::Value> = map
            .values()
            .map(|el| {
                let mut a11y = serde_json::Map::new();
                if !el.keyboard_operable {
                    // mouse-only control: not in the (nonexistent) keyboard tab
                    // order, and not keyboard-activatable.
                    a11y.insert("inTabOrder".into(), serde_json::Value::Bool(false));
                    a11y.insert("keyboardActivatable".into(), serde_json::Value::Bool(false));
                }
                serde_json::json!({
                    "id": el.id,
                    "operable": true,
                    "gestureKind": el.gesture_kind,
                    "a11y": serde_json::Value::Object(a11y),
                })
            })
            .collect();
        let payload = serde_json::json!({
            "sig": sig,
            "focusTrap": self.focus_trap.contains(sig),
            "elements": elements,
        });
        emit(&format!("EXPLORE:GROUNDTRUTH {payload}"));
    }
}

/// A deterministic mouse hotspot: a (row, col) cell to click, plus a stable id
/// describing where it came from. Signal B clicks these.
struct Hotspot {
    row: u16,
    col: u16,
    id: String,
}

/// Scan the grid for deterministic mouse hotspots, in a stable scan order:
///   - bracketed labels: `[ Save ]`, `[Yes]`, `<OK>` -> click the bracket center.
///   - reverse-video (highlighted/selected) cell runs -> click the run center.
///   - footer hint tokens advertised as keys -> already keyboard-reachable, so
///     they are NOT added (a mouse click there would never be "mouse-only").
/// Returns at most `cap` hotspots so a dense screen cannot explode the budget.
fn mouse_hotspots(parser: &Arc<Mutex<vt100::Parser>>, cap: usize) -> Vec<Hotspot> {
    let p = parser.lock().unwrap();
    let screen = p.screen();
    let (rows, cols) = screen.size();
    let mut spots: Vec<Hotspot> = Vec::new();
    // Bracketed-label hotspots: an open bracket/angle, some text, a close.
    for r in 0..rows {
        let mut open: Option<u16> = None;
        for c in 0..cols {
            let ch = screen
                .cell(r, c)
                .and_then(|cell| cell.contents().chars().next())
                .unwrap_or(' ');
            match ch {
                '[' | '<' => open = Some(c),
                ']' | '>' => {
                    if let Some(o) = open.take() {
                        if c > o {
                            let mid = o + (c - o) / 2;
                            spots.push(Hotspot {
                                row: r,
                                col: mid,
                                id: format!("bracket:{r},{o}"),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // Reverse-video run hotspots: a maximal run of inverse cells is a highlighted
    // / selected widget; click its center.
    for r in 0..rows {
        let mut run_start: Option<u16> = None;
        for c in 0..=cols {
            let inv = c < cols
                && screen
                    .cell(r, c)
                    .map(|cell| cell.inverse())
                    .unwrap_or(false);
            match (inv, run_start) {
                (true, None) => run_start = Some(c),
                (false, Some(s)) => {
                    let end = c - 1;
                    let mid = s + (end - s) / 2;
                    spots.push(Hotspot {
                        row: r,
                        col: mid,
                        id: format!("reverse:{r},{s}"),
                    });
                    run_start = None;
                }
                _ => {}
            }
        }
    }
    spots.truncate(cap);
    spots
}

/// Enable SGR mouse reporting on the slave PTY, the way a terminal would for an
/// app that requested it: 1000 (button events) + 1006 (SGR extended encoding).
/// Apps that called mousemask()/enabled mouse will now receive clicks; apps that
/// did not simply ignore the report bytes (harmless).
fn enable_mouse_reporting(writer: &Arc<Mutex<Box<dyn Write + Send>>>) {
    if let Ok(mut w) = writer.lock() {
        let _ = w.write_all(b"\x1b[?1000h\x1b[?1006h");
        let _ = w.flush();
    }
}

/// Send one SGR mouse click (press + release) at a 0-based (row, col) cell. SGR
/// mouse coordinates are 1-based, so we add 1. `\x1b[<0;C;Rm` is button-0 press,
/// `m` (vs `M`) is release in the SGR encoding; we send press then release so an
/// app that only reacts on release still fires.
fn send_mouse_click(writer: &Arc<Mutex<Box<dyn Write + Send>>>, row: u16, col: u16) {
    let (c, r) = (col + 1, row + 1);
    let press = format!("\x1b[<0;{c};{r}M");
    let release = format!("\x1b[<0;{c};{r}m");
    if let Ok(mut w) = writer.lock() {
        let _ = w.write_all(press.as_bytes());
        let _ = w.write_all(release.as_bytes());
        let _ = w.flush();
    }
}

/// Signal B driver: relaunch the app, enable mouse reporting, and click each
/// deterministic hotspot from the start screen, recording any state a click
/// reaches that NO keystroke did (a mouse-only / not-keyboard-operable control).
/// Deterministic: hotspots are scanned in a fixed order and clicked once each,
/// from a freshly relaunched start state per click so clicks don't compound.
fn mouse_probe(
    cmdline: &str,
    seen: &mut BTreeSet<String>,
    keyboard_reached: &BTreeSet<String>,
    gt: &mut Groundtruth,
) {
    // Cap on hotspots clicked, so a button-dense screen can't blow the budget.
    const MOUSE_BUDGET: usize = 12;
    emit("JOURNEY[a] step: mouse probe (Signal B) enabled");
    // Find the start-screen hotspots once (from a throwaway session), then click
    // each from its own fresh relaunch so the click is the ONLY input.
    let Ok((master0, mut child0, parser0, _w0, _e0)) = spawn_session(cmdline) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(900));
    let start_sig = snapshot(&parser0).0;
    let hotspots = mouse_hotspots(&parser0, MOUSE_BUDGET);
    let _ = child0.kill();
    drop(master0);
    if hotspots.is_empty() {
        return;
    }
    for hs in &hotspots {
        let Ok((master, mut child, parser, writer, _erases)) = spawn_session(cmdline) else {
            continue;
        };
        std::thread::sleep(Duration::from_millis(900));
        enable_mouse_reporting(&writer);
        std::thread::sleep(Duration::from_millis(60));
        send_mouse_click(&writer, hs.row, hs.col);
        std::thread::sleep(Duration::from_millis(300));
        if looks_crashed(&parser) {
            // A click that crashes the app is a finding, but Signal B is about
            // operability gaps, not the crash oracle; leave crash reporting to
            // the keyboard pass and just move on.
            let _ = child.kill();
            drop(master);
            continue;
        }
        let (sig, _fp, _labels) = snapshot(&parser);
        // Register a never-before-seen state so the engine knows the node, then
        // decide: a state the click reached that NO keystroke reached, and that
        // differs from the start screen, is mouse-only.
        // LIFECYCLE-metamorphic oracles (rotation, background-restore) are NOT
        // ported to the TUI backend: a terminal has no device orientation to
        // rotate (a PTY resize is not an orientation change) and no app-lifecycle
        // background/foreground transition (a TUI process is not sent to the
        // background and resumed the way a mobile/desktop app is), so the ground
        // truth those oracles assert cannot be produced here.
        if seen.insert(sig.clone()) {
            let labels = snapshot(&parser).2;
            let payload = serde_json::json!({ "sig": sig, "labels": labels, "elements": structural_input_elements() });
            emit(&format!("EXPLORE:STATE {payload}"));
        }
        if sig != start_sig && !keyboard_reached.contains(&sig) {
            // The control on the START screen at this hotspot is mouse-only: it
            // led somewhere the keyboard never did. Emit it on the start sig.
            gt.record(
                &start_sig,
                GtElement {
                    id: hs.id.clone(),
                    gesture_kind: "mouse",
                    keyboard_operable: false,
                },
            );
        }
        let _ = child.kill();
        drop(master);
    }
}

fn looks_crashed(parser: &Arc<Mutex<vt100::Parser>>) -> bool {
    let contents = parser.lock().unwrap().screen().contents();
    contents.contains("panicked at")
        || contents.contains("Traceback (most recent call last)")
        || contents.contains("thread 'main' panicked")
}

/// Full-screen TUIs (helix, lazygit, k9s, Claude Code) probe the terminal at
/// startup and BLOCK rendering until they get answers. A dumb PTY never replies,
/// so they stall at a blank screen. We scan the app's output for the common
/// queries and write canned responses back, so the app proceeds and renders.
fn answer_queries(
    chunk: &[u8],
    parser: &Arc<Mutex<vt100::Parser>>,
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
) {
    let mut resp: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i + 2 < chunk.len() {
        if chunk[i] == 0x1b && chunk[i + 1] == b'[' {
            let rest = &chunk[i..];
            if rest.starts_with(b"\x1b[c") || rest.starts_with(b"\x1b[0c") {
                // Primary Device Attributes -> claim a VT220-class terminal.
                resp.extend_from_slice(b"\x1b[?62;22c");
            } else if rest.starts_with(b"\x1b[>c") || rest.starts_with(b"\x1b[>0c") {
                // Secondary Device Attributes -> a plausible xterm identity.
                resp.extend_from_slice(b"\x1b[>0;276;0c");
            } else if rest.starts_with(b"\x1b[5n") {
                // Device status report -> OK.
                resp.extend_from_slice(b"\x1b[0n");
            } else if rest.starts_with(b"\x1b[6n") {
                // Cursor position report -> the parser's current cursor (1-based).
                let (row, col) = parser.lock().unwrap().screen().cursor_position();
                resp.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            } else if rest.starts_with(b"\x1b[?u") {
                // Kitty keyboard protocol query -> report "supported, 0 flags".
                resp.extend_from_slice(b"\x1b[?0u");
            } else if rest.starts_with(b"\x1b[?2026$p") {
                // DECRQM for synchronized output -> reset/not active.
                resp.extend_from_slice(b"\x1b[?2026;2$y");
            } else if rest.starts_with(b"\x1b[>q") {
                // XTVERSION -> a terminal name/version string.
                resp.extend_from_slice(b"\x1bP>|reproit(0.1)\x1b\\");
            }
        }
        i += 1;
    }
    if !resp.is_empty() {
        if let Ok(mut w) = writer.lock() {
            let _ = w.write_all(&resp);
            let _ = w.flush();
        }
    }
}

/// Count the full-screen ERASE-DISPLAY sequences (`CSI 2 J` / `CSI 3 J`) in a
/// raw output chunk. An app that clears the WHOLE screen and redraws it on a
/// keystroke is doing a full re-render; a well-behaved TUI (ncurses optimized
/// output, ratatui's diffing renderer) emits targeted cell updates and almost
/// never a full ED. So a full ED in response to an action is the deterministic
/// byte-stream signature of a wasteful full repaint, the VT analogue of the
/// web runner's node-identity churn. We count both `2J` (erase all) and `3J`
/// (erase all + scrollback); `0J`/`1J` (erase to end/start) are partial and not
/// counted. Pure scan, so the same app output yields the same count on replay.
fn count_full_erases(chunk: &[u8]) -> u64 {
    let mut n = 0u64;
    let mut i = 0usize;
    while i + 3 < chunk.len() {
        if chunk[i] == 0x1b
            && chunk[i + 1] == b'['
            && (chunk[i + 2] == b'2' || chunk[i + 2] == b'3')
            && chunk[i + 3] == b'J'
        {
            n += 1;
            i += 4;
        } else {
            i += 1;
        }
    }
    n
}

type Session = (
    Box<dyn portable_pty::MasterPty + Send>,
    Box<dyn portable_pty::Child + Send + Sync>,
    Arc<Mutex<vt100::Parser>>,
    Arc<Mutex<Box<dyn Write + Send>>>,
    // Running count of full-screen ERASE-DISPLAY sequences the app has emitted.
    // Sampled before/after each keystroke so the re-render oracle can tell when
    // an action triggered a full clear+redraw.
    Arc<AtomicU64>,
);

/// Open a PTY, launch the target via `sh -c`, start a reader thread feeding a
/// fresh VT parser, and return the handles. Called once per session: we
/// relaunch on a clean app exit so a quit key doesn't end fuzzing early.
fn spawn_session(cmdline: &str) -> Result<Session> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows: ROWS,
        cols: COLS,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    // The OS shell to interpret the command line (args + PATH resolution). On unix,
    // `sh -c "exec <cmd>"`: `exec` REPLACES the shell with the app at the same pid,
    // so child.process_id() is reliably the app's, not the wrapping `sh`'s -- the
    // --soak RSS sampler keys on that pid (most shells auto-exec a single simple
    // command, but that's an optimization, not guaranteed). On Windows there is no
    // `sh`/`exec`, so `cmd /c <cmd>` (the app runs as cmd's child; the /proc-based
    // RSS sampler is unix-only regardless, so this only affects --soak there).
    #[cfg(windows)]
    let (shell, flag, line) = ("cmd", "/c", cmdline.to_string());
    #[cfg(not(windows))]
    let (shell, flag, line) = ("sh", "-c", format!("exec {cmdline}"));
    let mut cmd = CommandBuilder::new(shell);
    cmd.arg(flag);
    cmd.arg(line);
    if let Some(cwd) = std::env::var_os("REPROIT_TUI_CWD").filter(|p| !p.is_empty()) {
        cmd.cwd(cwd);
    }
    cmd.env("TERM", "xterm-256color");
    // App-invariant channel + fuzzer-detection gate: the SDK writes its
    // REPROIT_INVARIANT markers to this file (see InvariantScrape) and, seeing
    // the var, evaluates its registry; absent (production) the registry is inert.
    cmd.env("REPROIT_INVARIANT_FILE", marker_file_path());
    cmd.env("REPROIT_INPUTS_FILE", input_file_path());
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(pair.master.take_writer()?));
    let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
    let erases = Arc::new(AtomicU64::new(0));
    {
        let parser = parser.clone();
        let writer = writer.clone();
        let erases = erases.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let e = count_full_erases(&buf[..n]);
                if e > 0 {
                    erases.fetch_add(e, Ordering::Relaxed);
                }
                parser.lock().unwrap().process(&buf[..n]);
                answer_queries(&buf[..n], &parser, &writer);
            }
        });
    }
    Ok((pair.master, child, parser, writer, erases))
}

// ── Multi-actor scenario client (the conductor protocol) ───────────────────
//
// The host conductor (modes/barrier.rs) owns identity and ordering for an
// authored multi-user scenario; a runner only has to speak three verbs over
// localhost HTTP and execute one action at a time:
//   GET  /claim               -> role letter (`a`, `b`, ...) | `ERR full`
//   GET  /next?device=<role>  -> `WAIT` | `ACT\t<action>` | `DONE`
//   POST /done?device=<role>  -> `OK`
// This is the same client the web/electron/tauri runners and the flutter
// explorer implement; only the action execution below is terminal-specific.

/// One HTTP exchange with the conductor: send the request, read the whole
/// response, return the body. The conductor answers every request with
/// `Content-Length` + `Connection: close`, so read-to-end after the blank line
/// is exactly the body. std::net keeps the sync PTY runner free of any async
/// runtime.
fn barrier_hit(base: &str, method: &str, path: &str) -> Result<String> {
    let addr = base.trim_end_matches('/');
    let addr = addr.strip_prefix("http://").unwrap_or(addr);
    let mut sock = std::net::TcpStream::connect(addr)
        .with_context(|| format!("connecting to conductor at {addr}"))?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        sock,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    sock.flush()?;
    let mut raw = String::new();
    sock.read_to_string(&mut raw)?;
    Ok(raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.trim().to_string())
        .unwrap_or_default())
}

/// Emit EXPLORE:STATE for a newly seen screen and return its signature, the
/// scenario-side twin of the fuzz loop's emit_state (states a scenario reaches,
/// often only reachable with a peer acting, still land in the map).
fn observe_scenario(parser: &Arc<Mutex<vt100::Parser>>, seen: &mut BTreeSet<String>) -> String {
    let (sig, _fp, labels) = snapshot(parser);
    if seen.insert(sig.clone()) {
        let payload = serde_json::json!({ "sig": sig, "labels": labels, "elements": structural_input_elements() });
        emit(&format!("EXPLORE:STATE {payload}"));
    }
    sig
}

/// Play ONE actor of a multi-user scenario: launch the app in a PTY, then loop
/// pulling this actor's next action from the conductor and acking completion,
/// so N runner processes interleave exactly as the journey specifies. The
/// terminal action vocabulary:
///   key:<Name>            press the key (same alphabet as fuzz/replay)
///   type:<finder>=<v>     type <v> literally; a PTY has ONE input channel
///                         (the keyboard), so the finder names intent, not a
///                         target to locate
///   back                  Esc, the universal "leave this screen" key
///   shoot:<name>          screenshot point (same contract as replay)
///   assert:text=<t>       the visible screen contains <t>
///   assert:count:<f>=<n>  the visible screen shows <f> exactly <n> times
///   auth:<acct>           unsupported (a terminal has no session store to
///                         restore); loud no-op so ordering still advances
/// Anything else (a `tap:<sel>` authored for a pointer surface) is a FUZZ:MISS,
/// so a stale or cross-surface journey fails loudly instead of silently
/// passing. Crash detection is the same oracle as fuzzing (a rendered panic,
/// or a panic/signal exit).
fn run_scenario_actor(cmdline: &str, base: &str) -> Result<()> {
    // Role identity: the per-process env label wins (each TUI actor is its own
    // process with its own env, so the label is reliable, unlike a shared-build
    // simulator); a runner without one claims a distinct role atomically.
    let mut role = std::env::var("REPROIT_DEVICE").unwrap_or_default();
    if role.is_empty() {
        role = match barrier_hit(base, "GET", "/claim") {
            Ok(r) if !r.is_empty() && !r.starts_with("ERR") => r,
            _ => "a".to_string(),
        };
    }
    emit(&format!("JOURNEY claimed role={role}"));

    let (_master, mut child, parser, writer, _erases) = spawn_session(cmdline)?;
    std::thread::sleep(Duration::from_millis(900));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cur_sig = observe_scenario(&parser, &mut seen);
    let mut failed = false;

    'actor: for _guard in 0..100_000u32 {
        let body = match barrier_hit(base, "GET", &format!("/next?device={role}")) {
            Ok(b) => b,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if body == "DONE" {
            break;
        }
        if body == "WAIT" {
            std::thread::sleep(Duration::from_millis(40));
            continue;
        }
        let act = body.strip_prefix("ACT\t").unwrap_or(&body).to_string();
        emit(&format!("FUZZ:ACT {role} {act}"));

        if let Some(name) = act.strip_prefix("shoot:") {
            // Screenshot point: capture the current screen, no state move.
            shoot(&parser, name);
        } else if let Some(a) = act.strip_prefix("assert:") {
            let contents = parser.lock().unwrap().screen().contents();
            if let Some(want) = a.strip_prefix("text=") {
                let ok = contents.contains(want);
                emit(&format!(
                    "FUZZ:ASSERT {} text={} actor={role}",
                    if ok { "pass" } else { "fail" },
                    serde_json::json!(want)
                ));
            } else if let Some(rest) = a.strip_prefix("count:") {
                let (finder, want) = rest.rsplit_once('=').unwrap_or((rest, "0"));
                let want: i64 = want.parse().unwrap_or(0);
                let got = if finder.is_empty() {
                    0
                } else {
                    contents.matches(finder).count() as i64
                };
                emit(&format!(
                    "FUZZ:ASSERT {} count {finder} want={want} got={got} actor={role}",
                    if got == want { "pass" } else { "fail" }
                ));
            } else {
                emit(&format!("FUZZ:ASSERT fail unsupported {a} actor={role}"));
            }
        } else if let Some(acct) = act.strip_prefix("auth:") {
            emit(&format!(
                "JOURNEY[a] step: auth-restore unsupported on tui runner; \
                 drive the login keys explicitly for auth:{acct}"
            ));
        } else {
            // Input actions: keystrokes into the PTY.
            let bytes: Vec<u8> = if act == "back" {
                b"\x1b".to_vec()
            } else if let Some(a) = act.strip_prefix("type:") {
                let value = a.rsplit_once('=').map(|(_, v)| v).unwrap_or(a);
                value.as_bytes().to_vec()
            } else if let Some(key) = act.strip_prefix("key:") {
                let b = bytes_for_key(&parser, key);
                if b.is_empty() {
                    emit(&format!("FUZZ:MISS {role} {act}"));
                }
                b
            } else {
                emit(&format!("FUZZ:MISS {role} {act}"));
                Vec::new()
            };
            if !bytes.is_empty() {
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(&bytes);
                    let _ = w.flush();
                }
            }
            std::thread::sleep(Duration::from_millis(260));
        }

        // Crash oracle, same rules as fuzzing: a panic rendered on screen, or
        // the process dying with a panic/signal code. A crashed actor cannot
        // continue, and we deliberately do NOT ack the step, so the conductor's
        // diagnose() names this actor and action as the stall point.
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
            break 'actor;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let code = status.exit_code();
            if code == 101 || code >= 128 {
                emit("EXCEPTION CAUGHT BY TUI APP");
                emit(&format!(
                    "The process crashed (exit code {code}) after {act}"
                ));
                emit("\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}");
            } else {
                // A clean quit mid-scenario still strands the remaining steps
                // (and this actor's peers), so it fails the run; relaunching
                // would silently resume from the start screen, not the state
                // the scenario was in.
                emit(&format!(
                    "JOURNEY[a] step: app exited (code {code}) before the scenario finished"
                ));
            }
            failed = true;
            break 'actor;
        }

        let next_sig = observe_scenario(&parser, &mut seen);
        if next_sig != cur_sig {
            let payload = serde_json::json!({ "from": cur_sig, "action": act, "to": next_sig });
            emit(&format!("EXPLORE:EDGE {payload}"));
        }
        cur_sig = next_sig;

        let _ = barrier_hit(base, "POST", &format!("/done?device={role}"));
    }

    let _ = child.kill();
    emit("JOURNEY DONE");
    emit(if failed {
        "Some tests failed"
    } else {
        "All tests passed"
    });
    Ok(())
}

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
    // clicks -> mouse-only controls are gated behind REPROIT_TUI_MOUSE=1 because it sends
    // mouse-reporting escapes and extra input the default keyboard-only run does
    // not, and not every app honors it.
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
    // --record clip capture: only in replay mode (a clip reproduces one finding)
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
        let is_new = seen.insert(sig.clone());
        if is_new {
            let payload = serde_json::json!({ "sig": sig, "labels": labels, "elements": structural_input_elements() });
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
        let (master, mut child, parser, writer, erases) = match spawn_session(&cmdline) {
            Ok(s) => s,
            Err(e) => {
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
        // --record: film the launch/start frame before any action, so the clip
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
                emit(&format!("FUZZ:ACT {act}"));
                shoot(&parser, name);
                i += 1;
                if frames_path.is_some() {
                    let scr = parser.lock().unwrap().screen().contents();
                    frames.push(serde_json::json!({ "action": act, "screen": scr }));
                }
                continue;
            }
            emit(&format!("FUZZ:ACT {act}"));
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
            // --record: film this action's settled frame. The LAST replay action is
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
                            "from": from, "action": action, "bucket": STUCK_FLOOR, "unit": "keypresses",
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

    // --record clip finalize: assemble the filmed frames into clip.mov, write the
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
    // deterministic SGR mouse clicks at hotspots (bracketed labels, reverse-video
    // runs) and watch for states that a click reaches but NO keystroke did. Such
    // a state is reachable only by pointer -> the control that leads there is
    // mouse-only / not keyboard-operable. Runs in its own fresh session(s) so it
    // never perturbs the keyboard exploration above; failures (an app that ignores
    // mouse reporting) are silent, the keyboard signal still stands.
    if mouse && !failed {
        mouse_probe(&cmdline, &mut seen, &keyboard_reached, &mut gt);
    }

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
mod tests {
    use super::*;

    #[test]
    fn tui_auth_registry_is_structural_and_locale_independent() {
        let path = input_file_path();
        std::fs::write(
            path,
            "{\"sel\":\"key:telefono\",\"inputPurpose\":\"tel\"}\n{\"sel\":\"key:codigo\",\"inputPurpose\":\"one-time-code\"}\n",
        )
        .unwrap();
        let elements = structural_input_elements();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0]["inputPurpose"], "otp");
        assert_eq!(elements[1]["inputPurpose"], "phone");
        assert!(elements.iter().all(|e| e["label"] == ""));
    }

    // Property tests (Hegel): hold the determinism invariants for ANY input.

    #[hegel::test]
    fn rng_is_reproducible_for_any_seed(tc: hegel::TestCase) {
        let seed: u32 = tc.draw(hegel::generators::integers::<u32>());
        let (mut a, mut b) = (Rng::new(seed), Rng::new(seed));
        for _ in 0..64 {
            assert_eq!(a.step(), b.step(), "same seed must yield the same stream");
        }
    }

    #[hegel::test]
    fn signature_is_a_pure_function_of_the_skeleton(tc: hegel::TestCase) {
        // The state signature must be a deterministic function of the screen's
        // structural skeleton: same skeleton + cursor -> same sig, every time.
        let contents: String = tc.draw(hegel::generators::text());
        let cur: (u16, u16) = (
            tc.draw(hegel::generators::integers::<u16>()),
            tc.draw(hegel::generators::integers::<u16>()),
        );
        assert_eq!(
            structural_sig(&contents, cur),
            structural_sig(&contents, cur),
            "structural sig must be deterministic"
        );
    }

    #[hegel::test]
    fn words_do_not_change_the_signature(tc: hegel::TestCase) {
        // Swapping ASCII letters (a stand-in for translating the UI) must not move
        // the signature: the localized identity of words is excluded by construction.
        let base: String = tc.draw(hegel::generators::text());
        let translated: String = base
            .chars()
            .map(|c| if c.is_ascii_alphabetic() { 'Z' } else { c })
            .collect();
        assert_eq!(
            structural_sig(&base, (0, 0)),
            structural_sig(&translated, (0, 0)),
            "swapping letters (translation) must not change the structural sig"
        );
    }

    // The runner primitives that make "author once, reproduce forever" true: a
    // seeded RNG and deterministic action selection. (The signature primitives
    // are pinned in the reproit-tui-sig crate the runner and SDKs share.)

    #[test]
    fn rng_is_reproducible_and_seed_sensitive() {
        let (mut a, mut b) = (Rng::new(42), Rng::new(42));
        for _ in 0..256 {
            assert_eq!(a.step(), b.step(), "same seed must yield the same stream");
        }
        assert_ne!(Rng::new(42).step(), Rng::new(43).step());
    }

    #[test]
    fn ucb_pick_is_deterministic() {
        let actions: Vec<String> = ["key:Down", "key:Up", "key:Enter"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let bound: BTreeSet<String> = actions.iter().cloned().collect();
        let lv = BTreeMap::new();
        let ar = BTreeMap::new();
        let sp = BTreeMap::new();
        let pick = |seed| {
            let mut rng = Rng::new(seed);
            ucb_pick(&actions, &bound, "sig0", &lv, &ar, &sp, None, 0.5, &mut rng)
        };
        assert_eq!(pick(9), pick(9), "same seed + same state -> same action");
    }

    #[test]
    fn path_to_frontier_crosses_cycles_to_untried_state() {
        let mut actions_by_state = BTreeMap::new();
        actions_by_state.insert("home".into(), vec!["key:Down".into(), "key:Enter".into()]);
        actions_by_state.insert(
            "settings".into(),
            vec!["key:Esc".into(), "key:Enter".into()],
        );
        actions_by_state.insert("help".into(), vec!["key:Esc".into()]);

        let tried = BTreeSet::from([
            edge_key("home", "key:Down"),
            edge_key("home", "key:Enter"),
            edge_key("settings", "key:Esc"),
        ]);
        let mut graph = BTreeMap::new();
        remember_edge(&mut graph, "home", "key:Down", "settings");
        remember_edge(&mut graph, "settings", "key:Esc", "home");
        remember_edge(&mut graph, "settings", "key:Enter", "help");

        assert_eq!(
            path_to_frontier(&graph, &actions_by_state, &tried, "home"),
            Some(vec!["key:Down".into()]),
            "home is exhausted, so walk the known cycle to settings"
        );
        assert_eq!(
            first_untried_action(&actions_by_state, &tried, "settings"),
            Some("key:Enter".into())
        );
    }

    #[test]
    fn action_space_is_full_alphabet_with_known_keymap_bound() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let (all, bound) = action_space("jless data.json", &parser);
        assert_eq!(all.len(), KEYS.len(), "full alphabet always reachable");
        assert!(bound.contains("key:j") && bound.contains("key:dollar"));
        assert!(
            bound.contains("key:CtrlC"),
            "universal crash key always bound"
        );
        let (all2, bound2) = action_space("totally-unknown-app", &parser);
        assert_eq!(all2.len(), KEYS.len());
        assert!(bound2.contains("key:Down") && bound2.contains("key:Esc"));
        assert!(
            !bound2.contains("key:j"),
            "no keymap, blank screen -> j not bound"
        );
    }

    // ── Operability signals (EXPLORE:GROUNDTRUTH) ─────────────────────────

    fn grid(rows: &[&str]) -> Vec<Vec<char>> {
        rows.iter().map(|r| r.chars().collect()).collect()
    }

    #[test]
    fn count_full_erases_counts_only_full_screen_clears() {
        // CSI 2 J (erase all) and CSI 3 J (erase all + scrollback) are full
        // repaints; partial erases (0J/1J) and a bare J are not.
        assert_eq!(count_full_erases(b"\x1b[2J"), 1);
        assert_eq!(count_full_erases(b"\x1b[3J"), 1);
        assert_eq!(count_full_erases(b"\x1b[2J\x1b[H\x1b[2J"), 2, "two clears");
        assert_eq!(count_full_erases(b"\x1b[0J"), 0, "erase-to-end is partial");
        assert_eq!(count_full_erases(b"\x1b[J"), 0, "bare J is not 2J/3J");
        assert_eq!(count_full_erases(b"hello world"), 0, "no escape");
    }

    #[test]
    fn churned_chrome_rows_flags_unchanged_box_rows_only() {
        // A box-drawing border row that is byte-identical across the transition
        // is churned chrome (rebuilt unchanged after a full erase); a plain text
        // row is not chrome, and a row that actually changed is not churn.
        let pre = grid(&["\u{2500}\u{2500}\u{2500}", "abc", "\u{2502} x \u{2502}"]);
        let mut post = pre.clone();
        post[1] = "abd".chars().collect(); // text row changed -> not chrome anyway
        let churned = churned_chrome_rows(&pre, &post, 16);
        assert_eq!(
            churned,
            vec!["row:0".to_string(), "row:2".to_string()],
            "the two unchanged box rows are churn; the text row never is"
        );
        // A chrome row that genuinely changed is NOT churn (real update).
        let mut post2 = pre.clone();
        post2[0] = "\u{250c}\u{2500}\u{2510}".chars().collect();
        assert_eq!(
            churned_chrome_rows(&pre, &post2, 16),
            vec!["row:2".to_string()],
            "a changed chrome row is a real update, not churn"
        );
        // Cap bounds the output.
        let wide = grid(&["\u{2500}", "\u{2500}", "\u{2500}"]);
        assert_eq!(churned_chrome_rows(&wide, &wide, 2).len(), 2, "capped");
    }

    #[test]
    fn content_bugs_catch_the_web_artifact_classes_with_stable_positions() {
        // The same broken-content classes the web classifier catches, scanned off
        // the settled cell grid and keyed by `pos:R,C`. First-match-wins per the
        // shared precedence; the output is sorted by (key, reason).
        let g = grid(&[
            "Name: [object Object]",
            "Hi {{ user.name }} welcome",
            "path is ${HOME}/x",
        ]);
        let bugs = detect_content_bugs(&g);
        let got: Vec<(&str, &str)> = bugs.iter().map(|b| (b.key.as_str(), b.reason)).collect();
        assert!(got.contains(&("pos:0,6", "object-object")));
        assert!(got.contains(&("pos:1,3", "unrendered-template")));
        assert!(got.contains(&("pos:2,8", "unrendered-template")));
        // Deterministic: same grid -> identical findings (run-to-run / replay).
        let again = detect_content_bugs(&g);
        let keys = |v: &[ContentBug]| -> Vec<String> {
            v.iter()
                .map(|b| format!("{}|{}", b.key, b.reason))
                .collect()
        };
        assert_eq!(keys(&bugs), keys(&again));
    }

    #[test]
    fn content_bugs_do_not_flag_ordinary_prose_or_clean_screens() {
        // The bare-value classes require WHOLE-WORD boundaries, so a word that
        // merely CONTAINS the token ("Cancellation" ~ null, "Null Island" as a
        // proper noun is flagged only when standalone) is left alone. A clean
        // screen yields nothing (the control stays silent -> no marker).
        let prose = grid(&[
            "Cancellation policy applies",
            "undefinedValue is a name",
            "the NaNobot is friendly",
            "Settings  Profile  Logout",
        ]);
        assert!(
            detect_content_bugs(&prose).is_empty(),
            "substrings inside words are not artifacts"
        );
        let data = grid(&[
            r#"{"next": null, "total": NaN}"#,
            "const value = undefined;",
            "status: null",
        ]);
        assert!(
            detect_content_bugs(&data).is_empty(),
            "valid data/code scalars are not artifacts"
        );
    }

    #[test]
    fn content_bugs_do_not_flag_path_embedded_null() {
        // A path segment `null` (git diff headers, file paths) is NOT a content
        // bug: `/` is not a word boundary in the desktop backends' guard, so the
        // token is not standalone. The old "any non-word char is a boundary" rule
        // flagged `--- /dev/null` (measured FP); the aligned rule must not.
        let diff = grid(&[
            "diff --git a/foo.txt b/foo.txt",
            "--- /dev/null",
            "+++ b/foo.txt",
            "content path foo/null/bar here",
        ]);
        assert!(
            detect_content_bugs(&diff).is_empty(),
            "path-embedded null (/dev/null, foo/null/bar) is not a content bug"
        );
        assert!(detect_content_bugs(&grid(&["Price: null", "value (null)", "null"])).is_empty());
    }

    #[test]
    fn tofu_fires_on_a_rendered_replacement_char_and_stays_silent_on_clean() {
        // A cell rendering U+FFFD is broken text encoding: flagged with a
        // stable pos key and a clipped excerpt around the char.
        let g = grid(&["Files", "name: gl\u{FFFD}tch here"]);
        let tofu = detect_tofu(&g);
        assert_eq!(tofu.len(), 1);
        assert_eq!(tofu[0].0, "pos:1,8");
        assert_eq!(tofu[0].1, "name: gl\u{FFFD}tch here");
        // Deterministic: same grid -> identical findings (run-to-run / replay).
        assert_eq!(detect_tofu(&g), tofu);
        // Clean screens (plain, box-drawing, and non-ASCII text) yield nothing:
        // U+FFFD is the only tofu signal, a wide glyph never is.
        let clean = grid(&[
            "\u{250c}\u{2500}\u{2510}",
            "caf\u{e9} \u{4f60}\u{597d}",
            "Save",
        ]);
        assert!(detect_tofu(&clean).is_empty(), "no U+FFFD, no finding");
    }

    #[test]
    fn blank_screen_fires_only_after_content_was_seen() {
        let blank = grid(&["    ", "    ", "    "]);
        let painted = grid(&["    ", " ok ", "    "]);
        // Before the app ever painted content, a blank screen is a slow boot,
        // not the bug: the seen_content guard keeps it silent.
        assert_eq!(blank_screen_item(&blank, false), None);
        // Once content was seen, an all-whitespace screen in a non-zero PTY is
        // the blank-screen bug, carrying the grid size.
        assert_eq!(blank_screen_item(&blank, true), Some((4, 3)));
        // A screen showing anything is never blank, guard or not.
        assert_eq!(blank_screen_item(&painted, true), None);
        // A zero-sized grid has no viewport to be blank in.
        assert_eq!(blank_screen_item(&grid(&[]), true), None);
        // And the guard's content test: ink is any non-whitespace cell.
        assert!(screen_has_ink(&painted));
        assert!(!screen_has_ink(&blank));
    }

    #[test]
    fn blank_screen_requires_persistence_across_a_resample() {
        let blank = grid(&["    ", "    ", "    "]);
        let painted = grid(&["    ", " ok ", "    "]);
        // Both samples blank -> a genuine blank screen, carrying the grid size.
        assert_eq!(
            blank_screen_persisted(&blank, &blank, true),
            Some((4, 3)),
            "persistently blank fires"
        );
        // The first sample caught an all-whitespace transient, but the re-sample
        // has ink: an Ink-style clear+repaint gap, not a blank screen -> silent.
        assert_eq!(
            blank_screen_persisted(&blank, &painted, true),
            None,
            "ink on the re-sample means the first was a transient"
        );
        // A first sample that already has content never reaches the re-sample.
        assert_eq!(blank_screen_persisted(&painted, &blank, true), None);
        // The seen_content guard still applies (a slow boot is never blank).
        assert_eq!(blank_screen_persisted(&blank, &blank, false), None);
    }
    #[test]
    fn groundtruth_emits_only_grounded_keyboard_gaps() {
        let mut gt = Groundtruth::new();
        assert!(gt.record(
            "sig",
            GtElement {
                id: "bracket:2,4".into(),
                gesture_kind: "mouse",
                keyboard_operable: false,
            },
        ));
        assert!(!gt.record(
            "sig",
            GtElement {
                id: "bracket:2,4".into(),
                gesture_kind: "mouse",
                keyboard_operable: false,
            },
        ));
        let element = gt.by_state["sig"].values().next().unwrap();
        assert!(!element.keyboard_operable);
    }

    #[test]
    fn parse_invariant_marker_reads_violations_and_ignores_noise() {
        // A well-formed marker yields the SDK sig + the violated (id, message).
        let (sig, items) = parse_invariant_marker(
            r#"REPROIT_INVARIANT {"sig":"abc","items":[{"id":"cart-total","message":"went negative"}]}"#,
        )
        .expect("a marker line parses");
        assert_eq!(sig, "abc");
        assert_eq!(items, vec![("cart-total".into(), "went negative".into())]);
        // message is optional (empty allowed).
        let (_, items) =
            parse_invariant_marker(r#"noise REPROIT_INVARIANT {"sig":"","items":[{"id":"x"}]}"#)
                .unwrap();
        assert_eq!(items, vec![("x".into(), String::new())]);
        // A non-marker line, a malformed body, and an empty item list are all
        // silent (a clean settle emits no marker, so None is the clean direction).
        assert!(parse_invariant_marker("just a rendered frame").is_none());
        assert!(parse_invariant_marker("REPROIT_INVARIANT {not json").is_none());
        assert!(
            parse_invariant_marker(r#"REPROIT_INVARIANT {"sig":"a","items":[]}"#).is_none(),
            "empty items => nothing to report"
        );
    }

    #[test]
    fn invariant_scrape_dedups_per_state_and_matches_sig() {
        let path =
            std::env::temp_dir().join(format!("reproit-inv-test-{}.ndjson", std::process::id()));
        std::fs::write(
            &path,
            "REPROIT_INVARIANT {\"sig\":\"s1\",\"items\":[{\"id\":\"inv\",\"message\":\"boom\"}]}\n",
        )
        .unwrap();
        let mut scr = InvariantScrape::new(&path.to_string_lossy());
        // Violating state s1 reports once, keyed by the SDK sig; a clean state s2
        // reports nothing; re-visiting s1 is de-duped (no repeat every settle).
        assert_eq!(
            scr.pending_for("s1"),
            Some(vec![("inv".into(), "boom".into())]),
            "violating state fires"
        );
        assert_eq!(scr.pending_for("s2"), None, "clean state is silent");
        assert_eq!(scr.pending_for("s1"), None, "same state does not repeat");
        // An empty-sig marker is attributed to the runner's next observed state.
        std::fs::write(
            &path,
            "REPROIT_INVARIANT {\"sig\":\"\",\"items\":[{\"id\":\"g\",\"message\":\"\"}]}\n",
        )
        .unwrap();
        scr.offset = 0; // re-read the rewritten file from the top
        assert_eq!(
            scr.pending_for("s9"),
            Some(vec![("g".into(), String::new())]),
            "empty-sig marker lands on the current runner sig"
        );
        let _ = std::fs::remove_file(&path);
    }
}
