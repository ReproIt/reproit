use super::*;

/// The action alphabet: the keys a fuzzer presses, and the bytes they send.
/// Covers navigation + confirm + the common vim/less/q vocabulary.
pub(super) const KEYS: &[(&str, &str)] = &[
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
pub(super) const UNIVERSAL: &[&str] = &[
    "Down", "Up", "Right", "Left", "Enter", "Tab", "Esc", "Space", "slash", "CtrlC", "CtrlD",
];

pub(super) fn char_to_keyname(c: char) -> Option<String> {
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
pub(super) fn app_keymap(cmdline: &str) -> Option<&'static [&'static str]> {
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
pub(super) fn scrape_hint_keys(parser: &Arc<Mutex<vt100::Parser>>) -> BTreeSet<String> {
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
pub(super) fn action_space(
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
pub(super) fn ucb_pick(
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

pub(super) fn edge_key(sig: &str, action: &str) -> String {
    format!("{sig}|{action}")
}

pub(super) fn ordered_actions(space: &[String], bound: &BTreeSet<String>) -> Vec<String> {
    space
        .iter()
        .filter(|o| bound.contains(*o))
        .chain(space.iter().filter(|o| !bound.contains(*o)))
        .cloned()
        .collect()
}

pub(super) fn is_crash_trigger(action: &str) -> bool {
    matches!(action, "key:CtrlC" | "key:CtrlD")
}

/// The byte sequence a `key:<Name>` action sends. Arrow keys honor the app's
/// cursor-key mode (DECCKM): SS3 (`ESC O B`) when the app called keypad()/smkx,
/// else CSI (`ESC [ B`). Unknown names yield no bytes (the caller decides
/// whether that is a MISS). Shared by the fuzz loop and the scenario actor so
/// both press keys identically.
pub(super) fn bytes_for_key(parser: &Arc<Mutex<vt100::Parser>>, key_name: &str) -> Vec<u8> {
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

pub(super) fn remember_actions(
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

pub(super) fn first_untried_action(
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

pub(super) fn has_frontier(
    actions_by_state: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
) -> bool {
    actions_by_state
        .keys()
        .any(|sig| first_untried_action(actions_by_state, tried, sig).is_some())
}

pub(super) fn remember_edge(
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

pub(super) fn path_to_frontier(
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
