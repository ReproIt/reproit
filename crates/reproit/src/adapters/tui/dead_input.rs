//! DEAD-INPUT oracle, TUI keystroke subset (EXPLORE:DEADINPUT).
//!
//! A printable keystroke that provably vanished inside an established
//! text-entry context. A raw "printable key, zero screen delta" predicate is
//! NOT zero-FP on a terminal (vim normal mode: `g` pends a chord with no
//! delta; motions at a boundary no-op legitimately), so the oracle requires
//! the full sandwich:
//!
//!   1. CONTEXT: >= APPEND_CONTEXT consecutive printable keys each appended
//!      EXACTLY (the typed glyph appeared at the pre-key cursor cell, the
//!      cursor advanced one column, everything else untouched modulo an
//!      insert shift). Command modes never establish this, so vim-normal
//!      no-ops can never arm the oracle.
//!   2. SWALLOW: the probed key produced a byte-identical grid AND an
//!      unmoved cursor. Any other effect (bell row, mode line, reflow)
//!      disarms.
//!   3. PROOF: the NEXT printable key appended ITS OWN glyph exactly. This
//!      kills the two legitimate swallow families: a full field (later keys
//!      fail too) and dead-key composition (the follow-up appends a COMBINED
//!      glyph, not its own).
//!
//! Pure functions over cell grids + cursor positions; the same key sequence
//! replays to the same verdict.

/// Consecutive exact appends required before a zero-delta key can be a
/// swallow candidate.
pub(super) const APPEND_CONTEXT: u32 = 2;

/// A confirmed swallowed keystroke: the key, and the `pos:R,C` cursor cell it
/// should have appended at (stable identity for the finding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Swallowed {
    pub(super) ch: char,
    pub(super) key: String,
}

/// The printable char a TUI action name types, if any. Mirrors the KEYS
/// alphabet: bare alphanumerics plus the named printable symbols.
pub(super) fn printable_char(key_name: &str) -> Option<char> {
    match key_name {
        "Space" => Some(' '),
        "slash" => Some('/'),
        "star" => Some('*'),
        "colon" => Some(':'),
        "dollar" => Some('$'),
        _ => {
            let mut chars = key_name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => Some(c),
                _ => None,
            }
        }
    }
}

/// Did this keystroke append EXACTLY `ch` at the pre-key cursor? True for
/// both overwrite (rest of row untouched) and insert (row suffix shifted
/// right by one) semantics. Every other row must be byte-identical and the
/// cursor must have advanced exactly one column on the same row.
pub(super) fn appended_exactly(
    pre: &[Vec<char>],
    post: &[Vec<char>],
    pre_cursor: (u16, u16),
    post_cursor: (u16, u16),
    ch: char,
) -> bool {
    let (row, col) = (pre_cursor.0 as usize, pre_cursor.1 as usize);
    if post_cursor != (pre_cursor.0, pre_cursor.1 + 1) {
        return false;
    }
    if pre.len() != post.len() || row >= pre.len() {
        return false;
    }
    for r in 0..pre.len() {
        if r != row && pre[r] != post[r] {
            return false;
        }
    }
    let (pre_row, post_row) = (&pre[row], &post[row]);
    if pre_row.len() != post_row.len() || col >= post_row.len() || post_row[col] != ch {
        return false;
    }
    if pre_row[..col] != post_row[..col] {
        return false;
    }
    let overwrite = pre_row[col + 1..] == post_row[col + 1..];
    let insert = post_row[col + 1..] == pre_row[col..pre_row.len() - 1];
    overwrite || insert
}

/// The append-context state machine. Feed it every printable keystroke's
/// before/after observation; call `reset()` for any other action. Returns the
/// confirmed swallow one keystroke AFTER it happened (the proof key).
#[derive(Default)]
pub(super) struct DeadInputTracker {
    appends: u32,
    pending: Option<Swallowed>,
}

impl DeadInputTracker {
    /// Consecutive exact appends observed so far (the context strength).
    pub(super) fn appends(&self) -> u32 {
        self.appends
    }

    pub(super) fn reset(&mut self) {
        self.appends = 0;
        self.pending = None;
    }

    pub(super) fn observe(
        &mut self,
        ch: char,
        pre: &[Vec<char>],
        pre_cursor: Option<(u16, u16)>,
        post: &[Vec<char>],
        post_cursor: Option<(u16, u16)>,
    ) -> Option<Swallowed> {
        let (Some(pre_cur), Some(post_cur)) = (pre_cursor, post_cursor) else {
            // A hidden cursor is not a text-entry context.
            self.reset();
            return None;
        };
        let appended = appended_exactly(pre, post, pre_cur, post_cur, ch);
        let zero = pre == post && pre_cur == post_cur;
        if let Some(pending) = self.pending.take() {
            if appended {
                // The proof key appended its own glyph: the pending key was
                // genuinely swallowed (field not full, no composition).
                self.appends += 1;
                return Some(pending);
            }
            self.appends = 0;
        }
        if appended {
            self.appends += 1;
        } else if zero && self.appends >= APPEND_CONTEXT {
            self.pending = Some(Swallowed {
                ch,
                key: format!("pos:{},{}", pre_cur.0, pre_cur.1),
            });
        } else {
            self.appends = 0;
        }
        None
    }
}
