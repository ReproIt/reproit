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
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ROWS: u16 = 40;
const COLS: u16 = 120;
const ACTION_BUDGET: u32 = 36;
const MAX_LABELS: usize = 24;

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

/// FNV-1a over a single pre-serialized string. The hashing primitive; the
/// caller decides WHAT to feed it (see `structural_sig`).
///
/// This is the SAME FNV-1a 32-bit primitive (offset basis 0x811c9dc5, prime
/// 0x01000193, 8-char zero-padded lowercase hex) as the canonical oracle's
/// `fnv1a32_hex` in `crates/reproit/src/model/signature.rs`. A terminal has no
/// accessibility tree, so the TUI backend does not build the canonical `Node`
/// descriptor; it derives the descriptor from the screen layout skeleton instead
/// (see `skeleton_of` / `structural_sig`). The descriptor SOURCE differs, but the
/// hash family is identical, so every TUI signature lives in the same 8-hex
/// namespace as the a11y signatures. This is the "Terminal and instrumented
/// surfaces" sub-contract in docs/signature.md; TUI signatures are therefore NOT
/// expected to match the a11y golden vectors in signature_vectors.json.
fn sig_of(s: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// Reduce one screen cell character to a locale-INVARIANT structural class.
///
/// The whole point of language-independence (docs/cli.md "Language-independence"
/// hard invariant): "Welcome" and "Begruessungsbildschirm" must hash the same.
/// So any run of natural-language letters collapses to a single placeholder
/// ('W'): a word is "a word" regardless of which language fills it. What we DO
/// keep is everything that is stable across locales and carries the layout:
///   - box-drawing / block glyphs (U+2500..U+259F): borders, panel extents.
///     Normalized to one marker ('#') so a single/double/rounded border edge
///     reads as the same structural edge.
///   - digits: collapsed to one marker ('9'). Numbers are not translated, but
///     their VALUES churn (counters, clocks), so we keep "a number is here"
///     positionally without pinning the value.
///   - ASCII punctuation / symbols (':', '[', ']', '/', '$', ...): kept verbatim.
///     These are the non-localized tokens, bracketed hotkeys and field markers,
///     that genuinely distinguish layouts.
///   - spaces: kept verbatim, they ARE the layout (gaps set column/field extents).
///
/// Tradeoff (documented): a TUI is inherently text, so we cannot drop characters
/// entirely without losing the row/column geometry that discriminates screens.
/// We therefore keep POSITIONS for every cell but erase the localized IDENTITY
/// of word characters. The skeleton (borders, gaps, symbol/number positions)
/// survives; the words do not. Same layout in two languages -> same skeleton.
fn structural_class(c: char) -> char {
    if ('\u{2500}'..='\u{259f}').contains(&c) {
        '#' // box-drawing / block -> a structural edge marker
    } else if c.is_ascii_digit() {
        '9' // a number lives here (value-agnostic)
    } else if c.is_alphanumeric() {
        'W' // any word character (any language, incl. CJK) -> placeholder
    } else if c == ' ' || c == '\n' || c.is_ascii_punctuation() {
        c // layout whitespace, or a non-localized symbol/hotkey/field marker, kept
    } else if c.is_whitespace() {
        ' ' // other whitespace -> space
    } else {
        'W' // anything else exotic -> treat as a word glyph
    }
}

/// Serialize the screen into its locale-invariant LAYOUT SKELETON, then collapse
/// each maximal run of the SAME class char to a length-prefixed token. The
/// length keeps the EXTENTS (a 20-wide border vs a 4-wide one differ; a
/// long word-field vs a short one differ) while the per-char identity of words
/// is already gone. This is the signature input: structure, not words.
fn skeleton_of(contents: &str) -> String {
    let mut out = String::new();
    let classed: String = contents.chars().map(structural_class).collect();
    let mut chars = classed.chars().peekable();
    while let Some(c) = chars.next() {
        let mut run = 1usize;
        while chars.peek() == Some(&c) {
            chars.next();
            run += 1;
        }
        // newline runs delimit rows; emit them literally so row count/positions
        // are preserved without a noisy length prefix.
        if c == '\n' {
            for _ in 0..run {
                out.push('\n');
            }
        } else {
            out.push(c);
            // a leading run-length captures the extent (border width, gap width,
            // word-field width) which is structural, while the value/identity is
            // not. Single cells need no length.
            if run > 1 {
                out.push_str(&run.to_string());
            }
        }
    }
    out
}

/// Cap on the number of numeric value-classes folded into the TUI signature, so
/// an adversarial number generator (a screen densely tiled with changing numbers)
/// cannot explode the value-class section. Mirrors the oracle's per-node hard cap
/// of 8 distinct value-class combinations (docs/signature.md Layer 2), applied
/// here as a per-screen bound on the count of numeric tokens that contribute.
const MAX_VALUE_CLASSES: usize = 8;

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

/// The signature input: the screen's structural skeleton, a bounded numeric
/// value-class section, and the cursor cell (which interactive field/row is
/// focused is structure, not text). Split out so it is unit-testable without a
/// live PTY/parser.
///
/// The value-class section is the TUI analogue of the oracle's Layer 2 (canonical
/// bounded value-class identity). The skeleton maps every digit to '9', so a
/// value-state app has a frozen skeleton; folding a bounded set of numeric
/// value-classes back in gives it a few distinct states (a counter at 0, 1, 12
/// land in ZERO, POS1, POS2) while two values in the same bucket (3 and 7, both
/// POS1) still collapse, exactly as the a11y oracle buckets node values.
fn structural_sig(contents: &str, cursor: (u16, u16)) -> String {
    let skeleton = skeleton_of(contents);
    let vclasses = numeric_value_classes(contents);
    // cursor row/col is the "which field/element is active" structural signal.
    // The "V:" section folds the bounded numeric value-classes into the identity;
    // it is empty (and so byte-identical to a skeleton-only sig) when the screen
    // carries no numeric tokens, preserving the locale/word invariants.
    let input = format!(
        "{skeleton}\x1ecur={},{}\x1eV:{}",
        cursor.0,
        cursor.1,
        vclasses.join(",")
    );
    sig_of(&input)
}

/// Extract the screen's numeric tokens and map each to the SAME value-class
/// buckets the canonical oracle uses (ZERO / NEG / POS1..POSL via the strict
/// decimal rule in `value_class`), then return a BOUNDED, SORTED multiset of
/// those buckets for folding into the TUI signature (Layer 2 analogue).
///
/// A "numeric token" is a maximal run of characters that can appear in a strict
/// decimal literal (digits, a leading sign, an internal period). Each token is
/// classified by the shared `value_class` bucketer, so tokens outside the strict
/// grammar (e.g. `1,234`, `12:34` split on the colon) bucket as the oracle would
/// (`NONEMPTY`, or the per-part numeric class). Buckets are sorted for
/// determinism and the count is capped at `MAX_VALUE_CLASSES`: once a screen has
/// more numeric tokens than the cap, we keep the first `MAX_VALUE_CLASSES` in
/// sorted order, bounding the contribution so an adversarial number-dense screen
/// cannot explode the graph.
fn numeric_value_classes(contents: &str) -> Vec<String> {
    let chars: Vec<char> = contents.chars().collect();
    let mut classes: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        // A token starts at a digit, or a sign/period immediately followed by a
        // digit (so a lone '-' or '.' is not a token).
        let c = chars[i];
        let starts = c.is_ascii_digit()
            || ((c == '+' || c == '-' || c == '.')
                && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()));
        if !starts {
            i += 1;
            continue;
        }
        let start = i;
        // consume the optional leading sign / period already matched above.
        if chars[i] == '+' || chars[i] == '-' || chars[i] == '.' {
            i += 1;
        }
        while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
            i += 1;
        }
        let token: String = chars[start..i].iter().collect();
        classes.push(value_class(&token).to_string());
    }
    classes.sort();
    classes.truncate(MAX_VALUE_CLASSES);
    classes
}

/// Map a numeric value string to the SAME bounded value-class token the canonical
/// oracle uses (`crates/reproit/src/model/signature.rs::value_class`). Reproduced
/// here (not imported) so the TUI backend stays self-contained, but the buckets,
/// the strict period-decimal grammar, and the locale-safe fallback are identical:
/// the binding guarantee for terminal surfaces is the same value-class family,
/// deterministic and bounded, not a shared `Node` tree.
fn value_class(s: &str) -> &'static str {
    let t = s.trim();
    if t.is_empty() {
        return "EMPTY";
    }
    if is_strict_decimal(t) {
        // Parse is safe: the grammar is a subset of f64's accepted syntax.
        let n: f64 = t.parse().unwrap_or(f64::NAN);
        let a = n.abs();
        if n == 0.0 {
            "ZERO"
        } else if n < 0.0 {
            "NEG"
        } else if a < 10.0 {
            "POS1"
        } else if a < 100.0 {
            "POS2"
        } else if a < 1000.0 {
            "POS3"
        } else {
            "POSL"
        }
    } else {
        "NONEMPTY"
    }
}

/// Strict `^[+-]?[0-9]+(\.[0-9]+)?$`: optional sign, one or more ASCII digits,
/// optionally a period followed by one or more ASCII digits. No grouping
/// separators, no exponent, no leading/trailing dot. Byte-for-byte the same rule
/// as the oracle's `is_strict_decimal`, so a TUI numeric token buckets exactly as
/// an a11y node value would.
fn is_strict_decimal(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == int_start {
        return false; // need at least one integer digit
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == frac_start {
            return false; // a trailing dot with no fraction digits is not allowed
        }
    }
    i == bytes.len()
}

/// A runner-local CONTENT FINGERPRINT over the FULL screen text (the actual
/// rendered cells, digits and words verbatim) plus the cursor cell. This is the
/// TUI analogue of Layer 1 effect detection (docs/signature.md): unlike the
/// skeleton signature, which maps every digit to '9' and every word to a
/// placeholder, this hashes the raw content, so it changes whenever ANY on-screen
/// value changes, even when the skeleton is byte-identical (a counter ticking
/// 0 -> 1 -> 2). It is EPHEMERAL and runner-local: it carries raw localized text
/// and MUST NOT enter the canonical state set, exactly as the a11y Layer-1
/// fingerprint must not enter the canonical graph key.
fn content_fingerprint(contents: &str, cursor: (u16, u16)) -> String {
    let input = format!("{contents}\x1ecur={},{}", cursor.0, cursor.1);
    sig_of(&input)
}

/// Display-only word set (the `labels` field). Unchanged from the original
/// tokenizer: blank box/block glyphs, split on whitespace, strip surrounding
/// punctuation, cap token width. Human display only, never the signature.
fn labels_of(contents: &str) -> Vec<String> {
    let cleaned: String = contents
        .chars()
        .map(|c| {
            if ('\u{2500}'..='\u{259f}').contains(&c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut set = BTreeSet::new();
    for raw in cleaned.split_whitespace() {
        let t = raw.trim_matches(|c: char| !c.is_alphanumeric());
        if !t.is_empty() && t.chars().count() <= 40 {
            set.insert(t.to_string());
        }
    }
    set.into_iter().take(MAX_LABELS).collect()
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

type Session = (
    Box<dyn portable_pty::MasterPty + Send>,
    Box<dyn portable_pty::Child + Send + Sync>,
    Arc<Mutex<vt100::Parser>>,
    Arc<Mutex<Box<dyn Write + Send>>>,
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
    {
        let parser = parser.clone();
        let writer = writer.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                parser.lock().unwrap().process(&buf[..n]);
                answer_queries(&buf[..n], &parser, &writer);
            }
        });
    }
    Ok((pair.master, child, parser, writer))
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
        }
        (sig, fp, is_new)
    };

    // Outer loop: (re)launch the app and fuzz until the action budget is spent.
    // A clean app exit (a quit key like `q`) is NOT a bug and is NOT the end of
    // fuzzing, relaunch and keep going. Only a crash (panic / non-zero exit)
    // stops the run.
    'fuzz: while i < budget {
        sessions += 1;
        let (master, mut child, parser, writer) = match spawn_session(&cmdline) {
            Ok(s) => s,
            Err(e) => {
                emit(&format!("JOURNEY[a] step: launch failed: {e}"));
                break;
            }
        };
        std::thread::sleep(Duration::from_millis(if sessions == 1 { 900 } else { 450 }));
        let (mut cur_sig, mut cur_fp, _) = emit_state(&parser, &mut seen);
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
        let mut stuck = 0;

        while i < budget && stuck < 14 {
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
            // `stuck` is the no-progress counter that ends a session. An action
            // with ANY effect (a new node, or just a value tick) resets it, so a
            // value-state app does not get abandoned as stalled.
            if effective {
                stuck = 0;
            } else {
                stuck += 1;
            }
            cur_sig = next_sig;
            cur_fp = next_fp;
        }
        let _ = child.kill();
        drop(master);
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

    // Property tests (Hegel): hold the determinism invariants for ANY input,
    // not just the hand-picked examples below.

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
        // Replacing any run of ASCII letters with a DIFFERENT word (a stand-in
        // for translating the UI) must not move the signature, for any input:
        // the localized identity of words is excluded by construction.
        let base: String = tc.draw(hegel::generators::text());
        // "Translate": swap each ascii-alphabetic char for another fixed letter.
        // Width is preserved on purpose (a faithful per-glyph swap); the point is
        // that letter IDENTITY is what must not matter.
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

    // Determinism is the product promise. These pin the primitives that make
    // "author once, reproduce forever" true: a seeded RNG, a stable state
    // signature, and deterministic action selection.

    #[test]
    fn rng_is_reproducible_and_seed_sensitive() {
        let (mut a, mut b) = (Rng::new(42), Rng::new(42));
        for _ in 0..256 {
            assert_eq!(a.step(), b.step(), "same seed must yield the same stream");
        }
        // a different seed yields a different stream (overwhelmingly likely).
        assert_ne!(Rng::new(42).step(), Rng::new(43).step());
    }

    #[test]
    fn sig_of_is_a_stable_pure_hash() {
        // The FNV primitive: same string -> same hash, different -> different.
        assert_eq!(sig_of("abc"), sig_of("abc"));
        assert_ne!(sig_of("abc"), sig_of("abd"));
    }

    // The "Terminal and instrumented surfaces" sub-contract (docs/signature.md):
    // the TUI descriptor SOURCE differs from the canonical a11y Node descriptor,
    // but the HASH FAMILY is identical. Pin the canonical FNV-1a 32-bit known
    // values so any drift in this backend's primitive away from the oracle's
    // `fnv1a32_hex` is caught here.
    #[test]
    fn sig_of_is_the_canonical_fnv1a_family() {
        // FNV-1a 32-bit offset basis for the empty string, and the known value
        // for "a", matching model/signature.rs::tests::fnv1a_known_value.
        assert_eq!(sig_of(""), "811c9dc5");
        assert_eq!(sig_of("a"), "e40c292c");
    }

    // The headline language-independence invariant (docs/cli.md): the SAME TUI
    // layout rendered in two different languages must hash to the SAME node,
    // while a genuinely different layout must hash differently.
    //
    // This is the contract the TUI backend is bound by: not the a11y golden
    // vectors (a terminal has no accessibility tree), but the "Terminal and
    // instrumented surfaces" sub-contract in docs/signature.md, hashed by the
    // same FNV-1a primitive (`sig_of`). These tests pin it directly.
    #[test]
    fn signature_is_locale_invariant_for_the_same_layout() {
        // A boxed login screen. Box-drawing borders, the gap structure, the
        // ':' field markers and the [x]/[y] hotkeys are identical across
        // locales; only the words differ. (Widths are kept equal here so the
        // test isolates the property under test: identity, not extent.)
        let en = "\
\u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}\n\
\u{2502} Login    \u{2502}\n\
\u{2502} User:    \u{2502}\n\
\u{2502} Pass:    \u{2502}\n\
\u{2502} [o] Okay \u{2502}\n\
\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}\n";
        // German rendering of the very same boxed screen: same borders, same
        // ':' markers, same [o] hotkey, same column layout, only words change.
        let de = "\
\u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}\n\
\u{2502} Anmel    \u{2502}\n\
\u{2502} Nutz:    \u{2502}\n\
\u{2502} Pass:    \u{2502}\n\
\u{2502} [o] Okay \u{2502}\n\
\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}\n";
        assert_eq!(
            structural_sig(en, (2, 8)),
            structural_sig(de, (2, 8)),
            "same layout in two languages must hash to the same node"
        );

        // A genuinely different layout (no box, different geometry) must differ.
        let other = "\
Some plain text screen\n\
with no borders at all\n\
and a totally different shape here\n";
        assert_ne!(
            structural_sig(en, (2, 8)),
            structural_sig(other, (0, 0)),
            "a different layout must hash differently"
        );

        // The cursor cell is a structural signal (which field is focused), so a
        // different focused field on the SAME screen is a different state.
        assert_ne!(
            structural_sig(en, (2, 8)),
            structural_sig(en, (3, 8)),
            "a different focused field must hash differently"
        );

        // Sanity: the original word-anchored approach WOULD have differed here.
        // (en/de share no full words beyond Okay/Pass), proving the win is real.
        assert_ne!(
            labels_of(en),
            labels_of(de),
            "labels still differ across locales (display-only, as intended)"
        );
    }

    // Value-state (Layer 2 analogue): the skeleton maps every digit to '9', so a
    // counter has a FROZEN skeleton. Folding a bounded numeric value-class set
    // into the TUI signature gives it a few distinct states, while two values in
    // the SAME bucket still collapse, mirroring the a11y oracle's buckets.
    #[test]
    fn counter_value_classes_split_distinct_buckets_but_collapse_within_a_bucket() {
        // A counter screen at 0, 1, and 12. Same skeleton ("a number is here"),
        // but the value-classes are ZERO, POS1, POS2 -> three distinct sigs.
        let s0 = "Count: 0\n";
        let s1 = "Count: 1\n";
        let s12 = "Count: 12\n";
        let cur = (0, 8);
        let a = structural_sig(s0, cur);
        let b = structural_sig(s1, cur);
        let c = structural_sig(s12, cur);
        assert_ne!(a, b, "0 (ZERO) vs 1 (POS1) must be distinct TUI states");
        assert_ne!(b, c, "1 (POS1) vs 12 (POS2) must be distinct TUI states");
        assert_ne!(a, c, "0 (ZERO) vs 12 (POS2) must be distinct TUI states");

        // Two different POS1 values (3 and 7) bucket the SAME -> same signature,
        // exactly as the oracle collapses 3 and 7 (both POS1). This is what keeps
        // the value-class bounded: a counter yields a few states, not infinitely
        // many.
        let s3 = "Count: 3\n";
        let s7 = "Count: 7\n";
        assert_eq!(
            structural_sig(s3, cur),
            structural_sig(s7, cur),
            "3 and 7 are both POS1 and must collapse to one TUI state"
        );
    }

    // Layer 1 analogue (effect detection): the runner-local content fingerprint
    // hashes the FULL raw screen text, so it differs when only digits change even
    // when the skeleton signature is byte-identical. This is what stops the
    // explorer stalling on a value-only update.
    #[test]
    fn content_fingerprint_differs_when_only_digits_change() {
        // A wide-number counter: 100 -> 101 -> 102 stay in the SAME value-class
        // bucket (all POS3) AND share the same skeleton, so the structural sig is
        // identical, but the fingerprint must still move (the action did
        // something), so the explorer keeps going.
        let a = "Hits: 100\n";
        let b = "Hits: 101\n";
        let cur = (0, 9);
        assert_eq!(
            structural_sig(a, cur),
            structural_sig(b, cur),
            "same skeleton + same POS3 bucket -> identical structural signature"
        );
        assert_ne!(
            content_fingerprint(a, cur),
            content_fingerprint(b, cur),
            "the content fingerprint must change when on-screen digits change"
        );

        // And the fingerprint is value-sensitive in general: any digit change
        // moves it even when the skeleton is frozen.
        assert_ne!(
            content_fingerprint("x = 5", (0, 0)),
            content_fingerprint("x = 6", (0, 0)),
            "fingerprint tracks the actual rendered value"
        );
    }

    // The numeric value-class extraction must match the canonical oracle buckets
    // (strict period-decimal grammar; locale-safe NONEMPTY fallback) and stay
    // bounded so an adversarial number-dense screen cannot explode the graph.
    #[test]
    fn numeric_value_classes_match_oracle_buckets_and_are_bounded() {
        // ZERO / NEG / POS1..POSL via the strict decimal rule.
        assert_eq!(numeric_value_classes("0"), vec!["ZERO"]);
        assert_eq!(numeric_value_classes("-3"), vec!["NEG"]);
        assert_eq!(numeric_value_classes("9"), vec!["POS1"]);
        assert_eq!(numeric_value_classes("42"), vec!["POS2"]);
        assert_eq!(numeric_value_classes("100"), vec!["POS3"]);
        assert_eq!(numeric_value_classes("1000"), vec!["POSL"]);
        // Locale-safe: grouping is ambiguous, so the colon/comma split yields the
        // per-part numeric classes, never a guessed grouped number. The contract
        // is the same value-class FAMILY, not the a11y Node golden vectors.
        assert_eq!(value_class("1,234"), "NONEMPTY");

        // Sorted, deterministic multiset.
        assert_eq!(
            numeric_value_classes("a 7 b 0 c 50"),
            vec!["POS1", "POS2", "ZERO"]
        );

        // Bounded: a screen tiled with many numbers contributes at most
        // MAX_VALUE_CLASSES tokens, so the value-class section cannot blow up.
        let many: String = (0..50).map(|n| format!("{n} ")).collect();
        let got = numeric_value_classes(&many);
        assert_eq!(
            got.len(),
            MAX_VALUE_CLASSES,
            "numeric value-class set is capped"
        );

        // A screen with NO numeric tokens contributes an empty set, so its sig is
        // unaffected by this layer (the locale/word invariants are preserved).
        assert!(numeric_value_classes("no numbers here").is_empty());
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
        // a known app: its keymap keys are bound, plus the universal nav/crash set.
        let (all, bound) = action_space("jless data.json", &parser);
        assert_eq!(all.len(), KEYS.len(), "full alphabet always reachable");
        assert!(bound.contains("key:j") && bound.contains("key:dollar"));
        assert!(
            bound.contains("key:CtrlC"),
            "universal crash key always bound"
        );
        // an unknown app on a blank screen: bound is just the universal set.
        let (all2, bound2) = action_space("totally-unknown-app", &parser);
        assert_eq!(all2.len(), KEYS.len());
        assert!(bound2.contains("key:Down") && bound2.contains("key:Esc"));
        assert!(
            !bound2.contains("key:j"),
            "no keymap, blank screen -> j not bound"
        );
    }
}
