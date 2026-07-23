use super::*;

pub(super) fn snapshot(parser: &Arc<Mutex<vt100::Parser>>) -> (String, String, Vec<String>) {
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

// Operability / accessibility signals (EXPLORE:GROUNDTRUTH).
//
// A TUI has ONE input channel: keystrokes. So the operability "graph 1" (what a
// user can actually do) and the keyboard/a11y "graph 2" coincide for the normal
// case. A grounded gap appears only when an SGR mouse-operable region cannot be
// reached through the keyboard. Missing nearby text is not evidence of a
// missing role or accessible name and is deliberately ignored.
//
//   Mouse-only signal (gated by REPROIT_TUI_MOUSE=1): we drive SGR mouse
//   clicks at deterministic hotspots (bracketed `[ Save ]`, reverse-video runs,
//   footer hint tokens). A state reached by a click but by NO keystroke is
//   mouse-only / not keyboard-operable: operable:true, a11y.inTabOrder:false +
//   keyboardActivatable:false (the engine counts these -> keyboard_unreachable
// + pointer_only).

/// A snapshot of the visible cell grid as a row-major char matrix (one char per
/// cell; wide-char continuations and empty cells render as a space). Used to
/// locate the DIFF RECTANGLE between two frames and to scan a sub-region for
/// word runs, both of which need cell coordinates that `contents()` (a single
/// newline-joined string with trailing blanks trimmed) does not preserve.
pub(super) fn grid_of(parser: &Arc<Mutex<vt100::Parser>>) -> Vec<Vec<char>> {
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
pub(super) struct ContentBug {
    /// `pos:R,C` of the match start (0-based row, col). Stable for a fixed
    /// settled screen, so the finding id is the same across runs and replays.
    pub(super) key: String,
    /// The high-confidence artifact class: `object-object` or
    /// `unrendered-template`.
    pub(super) reason: &'static str,
    /// The clipped offending text (human detail; key+reason are the identity).
    pub(super) text: String,
}

/// CONTENT-BUG oracle (deterministic, settled-screen text scan). The TUI
/// analogue of the web runner's `detectContentBugs`, restricted to artifacts
/// that remain unambiguous without DOM/accessibility semantics:
///   - `[object Object]`      : an object coerced to a string (the canonical
///     bug)
///   - `{{ ... }}` / `${ ... }`: an unrendered template placeholder (binding
///     never ran)
/// Bare `undefined`, `null`, and `NaN` are valid data/code values in JSON
/// viewers, logs, editors, and dashboards. A terminal grid cannot determine
/// their origin, so treating them as defects creates deterministic false
/// positives. We scan the SETTLED cell grid row by row (each row is one logical
/// text run, so a wrapped artifact is not stitched across rows, matching how a
/// TUI paints), and key each finding by the `pos:R,C` of the match start,
/// deduped by (key, reason). Pure function of the grid, so the same settled
/// screen yields the same findings on every run and on replay (no timing, no
/// pixels). A clean screen renders none of these, so the control stays silent
/// (no marker). The bracketed/`{{}}`/`${}` classes are matched as substrings.
pub(super) fn detect_content_bugs(grid: &[Vec<char>]) -> Vec<ContentBug> {
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
pub(super) fn detect_tofu(grid: &[Vec<char>]) -> Vec<(String, String)> {
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

/// One cell of the color-aware settled-screen snapshot: the glyph, the
/// RESOLVED foreground and background RGB (inverse video already swapped, so
/// this is what the viewer sees), and whether the cell is in an emphasis
/// context (inverse video, or an explicitly styled non-default background).
/// The resolution reuses `shot.rs::resolve` byte for byte, so oracle equality
/// is exactly render equality.
pub(super) struct ColorCell {
    pub(super) ch: char,
    pub(super) fg: [u8; 3],
    pub(super) bg: [u8; 3],
    pub(super) emphasized: bool,
}

/// Color-aware sibling of `grid_of`: the settled cell grid with resolved
/// per-cell colors. Only the zero-contrast oracle pays for this extraction.
pub(super) fn color_grid_of(parser: &Arc<Mutex<vt100::Parser>>) -> Vec<Vec<ColorCell>> {
    let p = parser.lock().unwrap();
    let screen = p.screen();
    let (rows, cols) = screen.size();
    let blank = || ColorCell {
        ch: ' ',
        fg: shot::DEFAULT_FG,
        bg: shot::DEFAULT_BG,
        emphasized: false,
    };
    let mut grid: Vec<Vec<ColorCell>> = (0..rows)
        .map(|_| (0..cols).map(|_| blank()).collect())
        .collect();
    for r in 0..rows {
        for c in 0..cols {
            if let Some(cell) = screen.cell(r, c) {
                let ch = cell.contents().chars().next().unwrap_or(' ');
                let fg = shot::resolve(cell.fgcolor(), shot::DEFAULT_FG);
                let bg = shot::resolve(cell.bgcolor(), shot::DEFAULT_BG);
                let styled_bg = !matches!(cell.bgcolor(), vt100::Color::Default);
                let (fg, bg) = if cell.inverse() { (bg, fg) } else { (fg, bg) };
                grid[r as usize][c as usize] = ColorCell {
                    ch,
                    fg,
                    bg,
                    emphasized: cell.inverse() || styled_bg,
                };
            }
        }
    }
    grid
}

/// ZERO-CONTRAST oracle (deterministic, settled-screen attribute scan). A run
/// of rendered glyphs whose RESOLVED foreground exactly equals its RESOLVED
/// background is invisible; in an emphasis context (a selected/highlighted row
/// or any explicitly styled background) invisibility is never intentional --
/// that is the lazygit/gitui theme-bug family. Guards that keep the zero-FP
/// bar:
///   - EXACT RGB equality only, no luminance thresholds (the WCAG-ratio
///     variant is a judgment call and stays out);
///   - the run must be EMPHASIZED (inverse video or explicit non-default
///     background). Hide-by-matching-the-terminal-default (e.g. a hidden
///     password echoed in the default background color) never fires because
///     the default background is excluded;
///   - >= ZERO_CONTRAST_MIN_RUN consecutive such cells with at least one
///     alphanumeric, so a decorative glyph or single artifact cell cannot
///     fire.
/// Pure function of the color grid: the same settled screen yields the same
/// findings on every run and on replay. Capped so one bad theme cannot flood
/// the marker.
const ZERO_CONTRAST_MIN_RUN: usize = 3;
const ZERO_CONTRAST_MAX_ITEMS: usize = 5;

pub(super) struct ZeroContrastRun {
    /// `pos:R,C` of the run start (0-based row, col). Stable for a fixed
    /// settled screen, so the finding id is the same across runs and replays.
    pub(super) key: String,
    /// The invisible text (clipped; human detail, key is the identity).
    pub(super) text: String,
    /// The shared resolved color both sides collapsed to, `rgb(r,g,b)`.
    pub(super) color: String,
}

pub(super) fn detect_zero_contrast(grid: &[Vec<ColorCell>]) -> Vec<ZeroContrastRun> {
    let mut out: Vec<ZeroContrastRun> = Vec::new();
    for (r, row) in grid.iter().enumerate() {
        let mut c = 0;
        while c < row.len() && out.len() < ZERO_CONTRAST_MAX_ITEMS {
            let invisible =
                |cell: &ColorCell| cell.ch != ' ' && cell.emphasized && cell.fg == cell.bg;
            if !invisible(&row[c]) {
                c += 1;
                continue;
            }
            let start = c;
            while c < row.len() && invisible(&row[c]) {
                c += 1;
            }
            let run = &row[start..c];
            if run.len() >= ZERO_CONTRAST_MIN_RUN
                && run.iter().any(|cell| cell.ch.is_alphanumeric())
            {
                let text: String = run.iter().map(|cell| cell.ch).take(40).collect();
                let [red, green, blue] = run[0].fg;
                out.push(ZeroContrastRun {
                    key: format!("pos:{r},{start}"),
                    text,
                    color: format!("rgb({red},{green},{blue})"),
                });
            }
        }
        if out.len() >= ZERO_CONTRAST_MAX_ITEMS {
            break;
        }
    }
    // Stable order: by key, so the marker is byte-identical run to run.
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

// DYNAMIC-TYPE and SCROLL-ROUND-TRIP are excluded on the TUI tier, no ground
// truth. dynamic-type: a terminal has a FIXED character-cell grid and no OS
// text scale to bump -- "larger text" is the user's terminal font, outside the
// app, with no per-app transform to drive. scroll-round-trip: a TUI scrolls
// only through app-defined key handling (there is no scroll viewport with an
// exact offset to jump to and restore), so the same-content-at-same-offset
// identity cannot be driven deterministically. Both carry on the web + Flutter
// tiers.

/// Does the grid render ANY non-whitespace cell? The blank-screen oracle's
/// content test, and the source of its `seen_content` guard: only a screen the
/// app has actually painted on counts as content.
pub(super) fn screen_has_ink(grid: &[Vec<char>]) -> bool {
    grid.iter().flatten().any(|c| !c.is_whitespace())
}

// SAFE-AREA oracle: EXCLUDED on the TUI/desktop runners. A terminal grid (and a
// desktop window) has NO device safe-area inset -- there is no notch, status
// bar, Dynamic Island, or home indicator, so there is no inset geometry to
// measure a control against. The oracle is native-mobile only.
//
// PERMISSION-WALK oracle: EXCLUDED on the TUI/desktop runners. A terminal app
// has no runtime OS permission the runner can DENY (no camera/location grant
// flow), so there is no permission-denial sweep to run.

/// BLANK-SCREEN oracle (EXPLORE:BLANKSCREEN): the settled screen renders ZERO
/// non-whitespace cells while the PTY has non-zero size, the TUI analogue of
/// the web white-screen-of-death (the app cleared the screen and painted
/// nothing back). Guarded by `seen_content`: the app must have painted at least
/// one non-blank screen earlier in the run, so an app that simply has not drawn
/// yet (a slow boot) never fires. Returns the `(w, h)` of the blank grid to
/// carry in the marker item, or None when the screen shows content, the PTY is
/// zero-sized, or no content was ever seen. Pure function of the grid + flag.
pub(super) fn blank_screen_item(grid: &[Vec<char>], seen_content: bool) -> Option<(i64, i64)> {
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

/// How long to wait before re-sampling a screen that looked blank, so a
/// whole-region clear+repaint (an Ink-style app that wipes then redraws every
/// frame) has time to paint the new frame we would otherwise mistake for a
/// blank screen.
pub(super) const BLANK_RESAMPLE_MS: u64 = 120;

/// BLANK-SCREEN with persistence: given the settled sample and a re-sample
/// taken a short delay later, the screen is blank ONLY if BOTH samples are
/// blank. Ink-style apps clear-and-repaint their whole region every frame, so a
/// single settled sample can land on the all-whitespace transient between the
/// clear and the repaint; the same state then showed up both as BLANKSCREEN and
/// as a GROUNDTRUTH with operable regions in one fuzz run (a measured FP). If
/// the re-sample has ANY ink we caught a repaint gap, not a genuinely blank
/// screen, so we stay silent. Returns the blank grid's `(w, h)` (from the first
/// sample) when both are blank, else None.
pub(super) fn blank_screen_persisted(
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
pub(super) fn is_chrome_row(row: &[char]) -> bool {
    row.iter()
        .any(|&ch| ('\u{2500}'..='\u{257f}').contains(&ch))
}

/// The persistent-chrome rows that survived a transition BYTE-IDENTICAL: rows
/// present and unchanged in both the pre- and post-action grids that carry
/// box-drawing chrome. When the app issued a full-screen erase on the action
/// (so it cleared and repainted everything), these unchanged chrome rows are
/// the ones it needlessly tore down and redrew, the VT analogue of the web
/// runner's reconciled-but-rebuilt anchors. Returns stable `row:R` keys (R is
/// the 0-based row), capped so a tall frame cannot flood the marker.
/// Deterministic: a pure function of the two grids.
pub(super) fn churned_chrome_rows(
    pre: &[Vec<char>],
    post: &[Vec<char>],
    cap: usize,
) -> Vec<String> {
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
