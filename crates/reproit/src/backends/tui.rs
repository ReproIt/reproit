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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reproit_tui_sig::{content_fingerprint, labels_of, structural_sig};

// Screenshot capture: render the vt100 cell grid to a PNG store/doc image.
#[path = "tui_shot.rs"]
mod shot;

const ROWS: u16 = 40;
const COLS: u16 = 120;
const ACTION_BUDGET: u32 = 36;

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

fn emit(s: &str) {
    println!("{s}");
    let _ = std::io::stdout().flush();
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

struct Fuzz {
    seed: u32,
    budget: u32,
    replay: Option<Vec<String>>,
    prefix: Option<Vec<String>>,
    edge_weights: BTreeMap<String, BTreeMap<String, u64>>,
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
        replay: None,
        prefix: None,
        edge_weights: BTreeMap::new(),
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
        (s.contents(), s.cursor_position())
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
// case, and a gap only shows up at the two edges this section instruments:
//
//   SIGNAL A (unlabeled operability): a keystroke that is EFFECTIVE (changed the
//   structural sig or the content fingerprint) but whose CHANGED grid region
//   carries NO natural-language word run nearby is an UNLABELED control: it does
//   something, yet nothing on screen names it. That is the TUI analogue of an
//   "operable element with no accessible name". We emit it as operable:true with
//   a11y.namePresent:false (and rolePresent:false, the dimension the engine
//   actually counts -> `no_role`).
//
//   SIGNAL B (mouse-only, gated by REPROIT_TUI_MOUSE=1): we also drive SGR mouse
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

/// The bounding rectangle (r0..=r1, c0..=c1) of the cells that differ between two
/// grids, or None if they are identical. The rectangle is the smallest box that
/// contains every changed cell, the TUI analogue of "which region did this action
/// repaint". Grids of mismatched dimensions compare on the overlap.
fn diff_rect(a: &[Vec<char>], b: &[Vec<char>]) -> Option<(usize, usize, usize, usize)> {
    let rows = a.len().min(b.len());
    let (mut r0, mut c0, mut r1, mut c1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut any = false;
    for r in 0..rows {
        let cols = a[r].len().min(b[r].len());
        for c in 0..cols {
            if a[r][c] != b[r][c] {
                any = true;
                r0 = r0.min(r);
                c0 = c0.min(c);
                r1 = r1.max(r);
                c1 = c1.max(c);
            }
        }
    }
    if any {
        Some((r0, c0, r1, c1))
    } else {
        None
    }
}

/// Margin (in cells) grown around the diff rectangle before scanning for a word.
/// A label often sits just outside the cells that actually repaint (a menu row
/// whose `> ` marker moves while its text is static, a checkbox `[x]` whose label
/// is to the right), so we look a little wider than the literal diff.
const WORD_MARGIN: usize = 6;

/// Does the region (diff rectangle grown by `WORD_MARGIN`) of `grid` contain a
/// natural-language WORD RUN? A "word" is a run of >= 2 alphabetic chars after
/// blanking box/block glyphs, the same notion of "a human-readable label" as
/// `labels_of`, but scoped to a rectangle instead of the whole screen. We require
/// length >= 2 so a lone marker letter (`x` in `[x]`, a `>` cursor) is not
/// mistaken for a label. Returns true if any such run exists in the region.
fn region_has_word(grid: &[Vec<char>], rect: (usize, usize, usize, usize)) -> bool {
    let (r0, c0, r1, c1) = rect;
    let rows = grid.len();
    let r_lo = r0.saturating_sub(WORD_MARGIN);
    let r_hi = (r1 + WORD_MARGIN).min(rows.saturating_sub(1));
    for row in grid.iter().take(r_hi + 1).skip(r_lo) {
        let cols = row.len();
        if cols == 0 {
            continue;
        }
        let c_lo = c0.saturating_sub(WORD_MARGIN);
        let c_hi = (c1 + WORD_MARGIN).min(cols - 1);
        let mut run = 0usize;
        for &ch in row.iter().take(c_hi + 1).skip(c_lo) {
            // box/block glyphs are not text (mirror labels_of's blanking).
            let is_box = ('\u{2500}'..='\u{259f}').contains(&ch);
            if ch.is_alphabetic() && !is_box {
                run += 1;
                if run >= 2 {
                    return true;
                }
            } else {
                run = 0;
            }
        }
    }
    false
}

/// One broken-content artifact found on the settled screen: the offending
/// position (a stable `pos:R,C` key), the artifact class, and the clipped text.
/// Serialized into the `items` array of an `EXPLORE:CONTENTBUG` line.
struct ContentBug {
    /// `pos:R,C` of the match start (0-based row, col). Stable for a fixed
    /// settled screen, so the finding id is the same across runs and replays.
    key: String,
    /// The artifact class, byte-identical to the web runner's reasons:
    /// `object-object` / `unrendered-template` / `undefined` / `null` / `nan`.
    reason: &'static str,
    /// The clipped offending text (human detail; key+reason are the identity).
    text: String,
}

/// Does a char count as a WORD boundary for the bare-value artifacts
/// (`undefined`/`null`/`NaN`)? Mirrors the web classifier's `\b`-style guard so a
/// real label that merely contains the substring ("Cancellation", "Null Island")
/// is NOT flagged: the token must stand alone, bounded by start/end-of-line or a
/// non-alphanumeric, non-`_` char. We treat the same separators the web regex
/// allows (whitespace, `:>([,` before and whitespace, `.,!?)]<` after) as
/// boundaries, generalized to "not a word char" so the grid scan stays simple
/// and equally strict.
fn is_word_boundary(c: Option<char>) -> bool {
    match c {
        None => true,
        Some(ch) => !(ch.is_alphanumeric() || ch == '_'),
    }
}

/// Does `row` contain the bare value `word` as a WHOLE word starting at `col`?
/// Both neighbours must be word boundaries (mirrors the web `\b` guard).
fn whole_word_at(row: &[char], col: usize, word: &[char]) -> bool {
    if col + word.len() > row.len() {
        return false;
    }
    if row[col..col + word.len()] != *word {
        return false;
    }
    let before = if col == 0 { None } else { Some(row[col - 1]) };
    let after = row.get(col + word.len()).copied();
    is_word_boundary(before) && is_word_boundary(after)
}

/// CONTENT-BUG oracle (deterministic, settled-screen text scan). The TUI analogue
/// of the web runner's `detectContentBugs`: a rendered run of cells that is
/// clearly broken CONTENT, the literal artifacts a stringify/template bug leaks
/// onto the screen. The SAME classes and the SAME order/first-match-wins rule as
/// the web classifier, so the two surfaces agree byte-for-byte on what counts:
///   - `[object Object]`      : an object coerced to a string (the canonical bug)
///   - `{{ ... }}` / `${ ... }`: an unrendered template placeholder (binding never ran)
///   - whole-word `undefined` : a missing value coerced into the text as a word
///   - whole-word `null`      : same, a null coerced in
///   - whole-word `NaN`       : a number computation that went non-finite
/// We scan the SETTLED cell grid row by row (each row is one logical text run, so
/// a wrapped artifact is not stitched across rows, matching how a TUI paints), and
/// key each finding by the `pos:R,C` of the match start, deduped by (key, reason).
/// Pure function of the grid, so the same settled screen yields the same findings
/// on every run and on replay (no timing, no pixels). A clean screen renders none
/// of these, so the control stays silent (no marker). The bracketed/`{{}}`/`${}`
/// classes are matched as substrings; the bare values require whole-word
/// boundaries so ordinary prose is not flagged.
fn detect_content_bugs(grid: &[Vec<char>]) -> Vec<ContentBug> {
    const OBJ: &[char] = &[
        '[', 'o', 'b', 'j', 'e', 'c', 't', ' ', 'O', 'b', 'j', 'e', 'c', 't', ']',
    ];
    const UNDEFINED: &[char] = &['u', 'n', 'd', 'e', 'f', 'i', 'n', 'e', 'd'];
    const NULL: &[char] = &['n', 'u', 'l', 'l'];
    const NAN: &[char] = &['N', 'a', 'N'];
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
            if whole_word_at(row, c, UNDEFINED) {
                push(r, c, "undefined", snippet(row, c));
                c += UNDEFINED.len();
                continue;
            }
            if whole_word_at(row, c, NULL) {
                push(r, c, "null", snippet(row, c));
                c += NULL.len();
                continue;
            }
            if whole_word_at(row, c, NAN) {
                push(r, c, "nan", snippet(row, c));
                c += NAN.len();
                continue;
            }
            c += 1;
        }
    }
    // Stable order: by key then reason, so the marker is byte-identical run to run.
    out.sort_by(|a, b| a.key.cmp(&b.key).then(a.reason.cmp(b.reason)));
    out
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
    /// false => emit a11y.namePresent:false + rolePresent:false (Signal A).
    name_present: bool,
    /// false => emit a11y.inTabOrder:false + keyboardActivatable:false (Signal B).
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
                    || prev.name_present != el.name_present
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
                if !el.name_present {
                    // unlabeled control: no accessible name, and (the dimension
                    // the engine counts) no exposed role.
                    a11y.insert("namePresent".into(), serde_json::Value::Bool(false));
                    a11y.insert("rolePresent".into(), serde_json::Value::Bool(false));
                }
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
        if seen.insert(sig.clone()) {
            let labels = snapshot(&parser).2;
            let payload = serde_json::json!({ "sig": sig, "labels": labels });
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
                    name_present: true, // labeling is orthogonal here
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
    let mut cmd = CommandBuilder::new("sh");
    cmd.arg("-c");
    cmd.arg(cmdline);
    cmd.env("TERM", "xterm-256color");
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

pub fn run() -> Result<()> {
    let cmdline = std::env::var("REPROIT_TUI_CMD")
        .ok()
        .filter(|s| !s.is_empty())
        .context("REPROIT_TUI_CMD (terminal command to drive) required")?;
    let fuzz = load_fuzz();
    let key_bytes: BTreeMap<&str, &str> = KEYS.iter().cloned().collect();
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
    let budget = fuzz
        .replay
        .as_ref()
        .map(|r| r.len())
        .unwrap_or((fuzz.budget as usize) + longest_seed);
    // round-robin / least-used seed picker state.
    let mut seed_uses: Vec<u64> = vec![0; corpus.len()];

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut tried: BTreeSet<String> = BTreeSet::new();
    // Live coverage guidance: how many times we've taken each (state, action)
    // THIS run, keyed "sig|key:Name". Feeds the UCB explore term.
    let mut live_visits: BTreeMap<String, u64> = BTreeMap::new();
    // UCB bookkeeping: cumulative reward per arm (reward paid when an action
    // reveals a NEW state), and total pulls out of each state. Tabular, no ML.
    let mut arm_reward: BTreeMap<String, f64> = BTreeMap::new();
    let mut state_pulls: BTreeMap<String, u64> = BTreeMap::new();
    let mut announced_space = false;
    // A/B switch: REPROIT_TUI_UNIFORM=1 disables command-awareness (no bound
    // priority / bonus, full alphabet treated uniformly) so the legacy behavior
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
    // Operability/accessibility ground truth (EXPLORE:GROUNDTRUTH). Signal A
    // (unlabeled effective controls) is always on; Signal B (SGR mouse clicks ->
    // mouse-only controls) is gated behind REPROIT_TUI_MOUSE=1 because it sends
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

    // Returns (signature, content_fingerprint, was_this_state_newly_discovered).
    // The bool is the UCB reward signal. The fingerprint is the runner-local
    // Layer-1 effect-detection token (full screen text, value-sensitive); it is
    // NEVER inserted into `seen`, only compared step-to-step to decide whether an
    // action did anything (docs/signature.md "Terminal and instrumented surfaces").
    let emit_state = |parser: &Arc<Mutex<vt100::Parser>>,
                      seen: &mut BTreeSet<String>|
     -> (String, String, bool) {
        let (sig, fp, labels) = snapshot(parser);
        let is_new = seen.insert(sig.clone());
        if is_new {
            let payload = serde_json::json!({ "sig": sig, "labels": labels });
            emit(&format!("EXPLORE:STATE {payload}"));
            // CONTENT-BUG oracle (EXPLORE:CONTENTBUG): scan the SETTLED screen for
            // the same broken-content artifacts the web runner catches ([object
            // Object], unrendered {{...}}/${...}, whole-word undefined/null/NaN).
            // Emitted once per newly-seen state (keyed by the same sig as STATE so
            // the engine attributes it to this node), each item keyed by the
            // `pos:R,C` of the match. Pure function of the grid, so it re-confirms
            // on replay; a clean screen emits nothing.
            let bugs = detect_content_bugs(&grid_of(parser));
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
        std::thread::sleep(Duration::from_millis(if sessions == 1 { 900 } else { 450 }));
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
        let (mut cur_sig, mut cur_fp, _) = emit_state(&parser, &mut seen);
        // The start/launch state is reachable with NO input, so it can never be
        // a mouse-only state (Signal B).
        keyboard_reached.insert(cur_sig.clone());
        if frames_path.is_some() && frames.is_empty() {
            let scr = parser.lock().unwrap().screen().contents();
            frames.push(serde_json::json!({ "action": "(launch)", "screen": scr }));
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

        while i < budget && stuck < STUCK_FLOOR {
            // Command-aware action space for THIS screen: the app's bound keys
            // (keymap + advertised footer hints) ∪ universal nav/crash keys,
            // falling back to the full alphabet only when nothing app-specific
            // is known. Most TUI keys are no-ops; this is what stops us wasting
            // ~80% of presses.
            let (space, bound_raw) = action_space(&cmdline, &parser);
            // Uniform A/B: empty bound set => no key is prioritized or bonused,
            // so ucb_pick degrades to plain UCB1 over the full flat alphabet.
            let bound = if uniform { BTreeSet::new() } else { bound_raw };
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
            // Systematic (unseeded) order: sweep bound keys before unbound ones.
            let systematic = |cur: &str| -> Option<String> {
                space
                    .iter()
                    .filter(|o| bound.contains(*o))
                    .chain(space.iter().filter(|o| !bound.contains(*o)))
                    .find(|o| !tried.contains(&format!("{cur}|{o}")))
                    .cloned()
                    .or_else(|| Some("key:Down".to_string()))
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
            let Some(act) = act else { break 'fuzz };
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
            tried.insert(format!("{cur_sig}|{act}"));
            *live_visits.entry(format!("{cur_sig}|{act}")).or_insert(0) += 1;
            *state_pulls.entry(cur_sig.clone()).or_insert(0) += 1;

            let key_name = act.strip_prefix("key:").unwrap_or(&act);
            // Arrow keys depend on the app's cursor-key mode (DECCKM): SS3
            // (ESC O B) when the app called keypad()/smkx, else CSI (ESC [ B).
            let bytes: Vec<u8> = match key_name {
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
                _ => key_bytes
                    .get(key_name)
                    .map(|s| s.as_bytes().to_vec())
                    .unwrap_or_default(),
            };
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
            std::thread::sleep(Duration::from_millis(260));
            i += 1;
            if frames_path.is_some() {
                let scr = parser.lock().unwrap().screen().contents();
                frames.push(serde_json::json!({ "action": act, "screen": scr }));
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

            let (next_sig, next_fp, is_new) = emit_state(&parser, &mut seen);
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
            }
            // A keystroke reaching `next_sig` proves that state is keyboard-
            // operable (feeds Signal B's mouse-only test).
            keyboard_reached.insert(next_sig.clone());
            // SIGNAL A (unlabeled operability): this keystroke was EFFECTIVE, so a
            // control on the CURRENT screen does something. Find the rectangle it
            // repainted and check whether any natural-language word sits in (or
            // near) that rectangle. No word -> an UNLABELED control: operable, but
            // nothing on screen names it. Emit it on `cur_sig` (the screen the
            // control lives on) with namePresent:false (+rolePresent:false, the
            // dimension the engine counts as a gap).
            let post_grid = grid_of(&parser);
            if effective {
                if let Some(rect) = diff_rect(&pre_grid, &post_grid) {
                    if !region_has_word(&post_grid, rect) && !region_has_word(&pre_grid, rect) {
                        let (r0, c0, _, _) = rect;
                        gt.record(
                            &cur_sig,
                            GtElement {
                                // id: where the effect appeared, stable across runs
                                // for the same control (region anchor, not the key).
                                id: format!("region:{r0},{c0}"),
                                gesture_kind: "key",
                                name_present: false,
                                keyboard_operable: true,
                            },
                        );
                    }
                }
            }
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
            "Total: NaN items",
            "Hi {{ user.name }} welcome",
            "path is ${HOME}/x",
            "value: undefined here",
            "set to null now",
        ]);
        let bugs = detect_content_bugs(&g);
        let got: Vec<(&str, &str)> = bugs.iter().map(|b| (b.key.as_str(), b.reason)).collect();
        assert!(got.contains(&("pos:0,6", "object-object")));
        assert!(got.contains(&("pos:1,7", "nan")));
        assert!(got.contains(&("pos:2,3", "unrendered-template")));
        assert!(got.contains(&("pos:3,8", "unrendered-template")));
        assert!(got.contains(&("pos:4,7", "undefined")));
        assert!(got.contains(&("pos:5,7", "null")));
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
        // A standalone bare value IS flagged (proves the boundary rule fires).
        let bare = grid(&["status: null"]);
        assert_eq!(detect_content_bugs(&bare).len(), 1);
        assert_eq!(detect_content_bugs(&bare)[0].reason, "null");
    }

    #[test]
    fn diff_rect_finds_the_smallest_changed_box() {
        let a = grid(&["....", "....", "...."]);
        let mut b = a.clone();
        b[1][1] = 'X';
        b[1][2] = 'Y';
        assert_eq!(diff_rect(&a, &b), Some((1, 1, 1, 2)));
        assert_eq!(diff_rect(&a, &a), None, "identical grids => no diff");
    }

    #[test]
    fn region_has_word_distinguishes_labels_from_bare_markers() {
        // A region containing the word "Save" -> labeled (has a word run).
        let labeled = grid(&["[ Save ]"]);
        assert!(region_has_word(&labeled, (0, 0, 0, 7)));
        // A region with only single-char markers / digits / symbols -> unlabeled
        // (a toggled `*`, a moved `>` cursor, a `[x]` checkbox glyph).
        let unlabeled = grid(&["[*]  > 1"]);
        assert!(
            !region_has_word(&unlabeled, (0, 0, 0, 7)),
            "bare markers are not a word run"
        );
    }

    #[test]
    fn groundtruth_emits_only_known_false_a11y_dims() {
        let mut gt = Groundtruth::new();
        // Signal A element: unlabeled (namePresent:false) but keyboard-operable.
        let emitted = gt.record(
            "sigA",
            GtElement {
                id: "region:3,5".into(),
                gesture_kind: "key",
                name_present: false,
                keyboard_operable: true,
            },
        );
        assert!(emitted, "a new element triggers an emit");
        // Re-recording the same element is a no-op (no double count).
        assert!(
            !gt.record(
                "sigA",
                GtElement {
                    id: "region:3,5".into(),
                    gesture_kind: "key",
                    name_present: false,
                    keyboard_operable: true,
                },
            ),
            "an unchanged element does not re-emit"
        );

        // Reconstruct the engine's gap rule over what we'd serialize, to prove a
        // Signal-A element counts as a no_role gap (rolePresent:false) and a
        // Signal-B element counts as pointer_only + keyboard_unreachable.
        let count_gaps = |gt: &Groundtruth, sig: &str| -> (u32, u32, u32) {
            let map = gt.by_state.get(sig).unwrap();
            let (mut pointer_only, mut kb_unreach, mut no_role) = (0u32, 0u32, 0u32);
            for el in map.values() {
                if !el.name_present {
                    no_role += 1; // rolePresent:false emitted -> engine's no_role
                }
                if !el.keyboard_operable {
                    pointer_only += 1; // keyboardActivatable:false -> pointer_only
                    kb_unreach += 1; // inTabOrder:false -> keyboard_unreachable
                }
            }
            (pointer_only, kb_unreach, no_role)
        };
        assert_eq!(
            count_gaps(&gt, "sigA"),
            (0, 0, 1),
            "unlabeled => one no_role"
        );

        // Signal B element: mouse-only (keyboard_operable:false), labeled.
        gt.record(
            "sigB",
            GtElement {
                id: "bracket:2,4".into(),
                gesture_kind: "mouse",
                name_present: true,
                keyboard_operable: false,
            },
        );
        assert_eq!(
            count_gaps(&gt, "sigB"),
            (1, 1, 0),
            "mouse-only => pointer_only + keyboard_unreachable"
        );
    }
}
