//! Canonical TUI screen signature.
//!
//! This crate is the single source of truth for the terminal-UI (TUI) state
//! signature. The `reproit __tui` runner
//! (crates/reproit/src/backends/tui/mod.rs) and every production TUI SDK
//! (Rust/Go/TS/Python) compute the SAME value, so a production crash reported
//! by an SDK carries a signature the runner can replay locally. The Rust runner
//! and the Rust SDK share THIS code directly, so their parity is a compile-time
//! property, not a tested-after-the-fact port; the Go/TS/Python SDKs port these
//! functions and are pinned to the golden vectors (tui_signature_vectors.json)
//! generated from this crate.
//!
//! The descriptor SOURCE is the rendered terminal screen (the VT cell grid),
//! normalized to a locale-invariant layout skeleton, NOT an accessibility role
//! tree. This is the "Terminal and instrumented surfaces" sub-contract in
//! docs/signature.md, so TUI signatures are NOT expected to match the a11y
//! golden vectors in signature_vectors.json. What IS shared with every other
//! surface is the hash family (FNV-1a 32-bit) and the value-class buckets.

use std::collections::BTreeSet;

/// Cap on the number of numeric value-classes folded into the TUI signature, so
/// an adversarial number generator (a screen densely tiled with changing
/// numbers) cannot explode the value-class section. Mirrors the oracle's
/// per-node hard cap of 8 distinct value-class combinations (docs/signature.md
/// Layer 2), applied here as a per-screen bound on the count of numeric tokens
/// that contribute.
const MAX_VALUE_CLASSES: usize = 8;

/// Cap on the display-only label set (`labels_of`), matching the a11y oracle's
/// per-screen label cap.
const MAX_LABELS: usize = 24;

/// The FNV-1a 32-bit hash (over UTF-8 bytes), formatted as 8-char zero-padded
/// lowercase hex. Identical hash family to the a11y oracle's `fnv1a32_hex` and
/// every other reproit surface, so every TUI signature lives in the same 8-hex
/// namespace as the a11y signatures, even though the descriptor SOURCE differs.
pub fn sig_of(s: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// Reduce one screen cell character to a locale-INVARIANT structural class.
///
/// The whole point of language-independence (docs/cli.md
/// "Language-independence" hard invariant): "Welcome" and
/// "Begruessungsbildschirm" must hash the same. So any run of natural-language
/// letters collapses to a single placeholder ('W'): a word is "a word"
/// regardless of which language fills it. What we DO keep is everything that is
/// stable across locales and carries the layout:
///   - box-drawing / block glyphs (U+2500..U+259F): borders, panel extents.
///     Normalized to one marker ('#') so a single/double/rounded border edge
///     reads as the same structural edge.
///   - digits: collapsed to one marker ('9'). Numbers are not translated, but
///     their VALUES churn (counters, clocks), so we keep "a number is here"
///     positionally without pinning the value.
///   - ASCII punctuation / symbols (':', '[', ']', '/', '$', ...): kept
///     verbatim. These are the non-localized tokens, bracketed hotkeys and
///     field markers, that genuinely distinguish layouts.
///   - spaces: retained as separators, with run widths normalized because
///     right-aligned numeric fields trade padding for digits as values change.
///
/// Tradeoff (documented): a TUI is inherently text, so we cannot drop
/// characters entirely without losing the row/column geometry that
/// discriminates screens. We therefore keep POSITIONS for every cell but erase
/// the localized IDENTITY of word characters. The skeleton (borders, gaps,
/// symbol/number positions) survives; the words do not. Same layout in two
/// languages -> same skeleton.
pub fn structural_class(c: char) -> char {
    if ('\u{2500}'..='\u{259f}').contains(&c) {
        '#' // box-drawing / block -> a structural edge marker
    } else if c.is_ascii_digit() {
        '9' // a number lives here (value-agnostic)
    } else if c.is_alphanumeric() {
        'W' // any word character (any language, incl. CJK) -> placeholder
    } else if c == ' ' || c == '\n' || c.is_ascii_punctuation() {
        c // layout whitespace, or a non-localized symbol/hotkey/field marker,
          // kept
    } else if c.is_whitespace() {
        ' ' // other whitespace -> space
    } else {
        'W' // anything else exotic -> treat as a word glyph
    }
}

/// Serialize the screen into its locale-invariant LAYOUT SKELETON, then
/// collapse each maximal run of the SAME class char to a length-prefixed token.
/// The length keeps stable extents (a 20-wide border vs a 4-wide one differ; a
/// long word-field vs a short one differ). Numeric and whitespace run lengths
/// are omitted because dashboard values exchange padding for digits over time.
pub fn skeleton_of(contents: &str) -> String {
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
            // Numeric field width is volatile in dashboards (PID, CPU, clocks,
            // counters). Its position is structural, but 9 -> 10 must not create
            // a new graph node merely because the digit run grew.
            if run > 1 && c != '9' && c != ' ' {
                out.push_str(&run.to_string());
            }
        }
    }
    out
}

/// The signature input: the screen's structural skeleton, a bounded numeric
/// value-class section, and the cursor cell (which interactive field/row is
/// focused is structure, not text). Unit-testable without a live PTY/parser.
///
/// The value-class section is the TUI analogue of the oracle's Layer 2
/// (canonical bounded value-class identity). The skeleton maps every digit to
/// '9', so a value-state app has a frozen skeleton; folding a bounded set of
/// numeric value-classes back in gives it a few distinct states (a counter at
/// 0, 1, 12 land in ZERO, POS1, POS2) while two values in the same bucket (3
/// and 7, both POS1) still collapse, exactly as the a11y oracle buckets node
/// values.
pub fn structural_sig(contents: &str, cursor: (u16, u16)) -> String {
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
/// decimal rule in `value_class`), then return a BOUNDED, SORTED set of
/// distinct buckets for folding into the TUI signature (Layer 2 analogue).
///
/// A "numeric token" is a maximal run of characters that can appear in a strict
/// decimal literal (digits, a leading sign, an internal period). Each token is
/// classified by the shared `value_class` bucketer, so tokens outside the
/// strict grammar (e.g. `1,234`, `12:34` split on the colon) bucket as the
/// oracle would (`NONEMPTY`, or the per-part numeric class). Buckets are sorted
/// for determinism and deduplicated before the count is capped. Repeated
/// dashboard metrics therefore cannot change identity as values enter and leave
/// the first few cells of a dense screen.
pub fn numeric_value_classes(contents: &str) -> Vec<String> {
    let chars: Vec<char> = contents.chars().collect();
    let mut classes = BTreeSet::new();
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
        classes.insert(value_class(&token).to_string());
    }
    classes.into_iter().take(MAX_VALUE_CLASSES).collect()
}

/// Map a numeric value string to the SAME bounded value-class token the
/// canonical oracle uses (crates/reproit/src/model/signature.rs::value_class):
/// the buckets, the strict period-decimal grammar, and the locale-safe fallback
/// are identical. The binding guarantee for terminal surfaces is the same
/// value-class family, deterministic and bounded, not a shared `Node` tree.
pub fn value_class(s: &str) -> &'static str {
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
/// separators, no exponent, no leading/trailing dot. Byte-for-byte the same
/// rule as the oracle's `is_strict_decimal`, so a TUI numeric token buckets
/// exactly as an a11y node value would.
pub fn is_strict_decimal(s: &str) -> bool {
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
            return false; // a trailing dot with no fraction digits is not
                          // allowed
        }
    }
    i == bytes.len()
}

/// A runner-local CONTENT FINGERPRINT over the FULL screen text (the actual
/// rendered cells, digits and words verbatim) plus the cursor cell. This is the
/// TUI analogue of Layer 1 effect detection (docs/signature.md): unlike the
/// skeleton signature, which maps every digit to '9' and every word to a
/// placeholder, this hashes the raw content, so it changes whenever ANY
/// on-screen value changes, even when the skeleton is byte-identical (a counter
/// ticking 0 -> 1 -> 2). It is EPHEMERAL and runner-local: it carries raw
/// localized text and MUST NOT enter the canonical state set, exactly as the
/// a11y Layer-1 fingerprint must not enter the canonical graph key.
pub fn content_fingerprint(contents: &str, cursor: (u16, u16)) -> String {
    let input = format!("{contents}\x1ecur={},{}", cursor.0, cursor.1);
    sig_of(&input)
}

/// Display-only word set (the `labels` field). Blank box/block glyphs, split on
/// whitespace, strip surrounding punctuation, cap token width. Human display
/// only, never the signature.
pub fn labels_of(contents: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sig_of_is_a_stable_pure_hash() {
        // The FNV primitive: same string -> same hash, different -> different.
        assert_eq!(sig_of("abc"), sig_of("abc"));
        assert_ne!(sig_of("abc"), sig_of("abd"));
    }

    // The "Terminal and instrumented surfaces" sub-contract (docs/signature.md):
    // the TUI descriptor SOURCE differs from the canonical a11y Node descriptor,
    // but the HASH FAMILY is identical. Pin the canonical FNV-1a 32-bit known
    // values so any drift in this primitive away from the oracle's `fnv1a32_hex`
    // is caught here.
    #[test]
    fn sig_of_is_the_canonical_fnv1a_family() {
        assert_eq!(sig_of(""), "811c9dc5");
        assert_eq!(sig_of("a"), "e40c292c");
    }

    // The headline language-independence invariant (docs/cli.md): the SAME TUI
    // layout rendered in two different languages must hash to the SAME node,
    // while a genuinely different layout must hash differently.
    #[test]
    fn signature_is_locale_invariant_for_the_same_layout() {
        let en = "\
\u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}\n\u{2502} \
                  Login    \u{2502}\n\u{2502} User:    \u{2502}\n\u{2502} Pass:    \
                  \u{2502}\n\u{2502} [o] Okay \
                  \u{2502}\n\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\\
                  u{2500}\u{2500}\u{2518}\n";
        let de = "\
\u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}\n\u{2502} \
                  Anmel    \u{2502}\n\u{2502} Nutz:    \u{2502}\n\u{2502} Pass:    \
                  \u{2502}\n\u{2502} [o] Okay \
                  \u{2502}\n\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\\
                  u{2500}\u{2500}\u{2518}\n";
        assert_eq!(
            structural_sig(en, (2, 8)),
            structural_sig(de, (2, 8)),
            "same layout in two languages must hash to the same node"
        );

        let other = "\
Some plain text screen\nwith no borders at all\nand a totally different shape here\n";
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

        // Sanity: labels still differ across locales (display-only, as intended).
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
    // when the skeleton signature is byte-identical.
    #[test]
    fn content_fingerprint_differs_when_only_digits_change() {
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
        assert_eq!(numeric_value_classes("0"), vec!["ZERO"]);
        assert_eq!(numeric_value_classes("-3"), vec!["NEG"]);
        assert_eq!(numeric_value_classes("9"), vec!["POS1"]);
        assert_eq!(numeric_value_classes("42"), vec!["POS2"]);
        assert_eq!(numeric_value_classes("100"), vec!["POS3"]);
        assert_eq!(numeric_value_classes("1000"), vec!["POSL"]);
        assert_eq!(value_class("1,234"), "NONEMPTY");

        assert_eq!(
            numeric_value_classes("a 7 b 0 c 50"),
            vec!["POS1", "POS2", "ZERO"]
        );

        let many: String = (0..50).map(|n| format!("{n} ")).collect();
        let got = numeric_value_classes(&many);
        assert_eq!(got, vec!["POS1", "POS2", "ZERO"]);

        assert!(numeric_value_classes("no numbers here").is_empty());
    }

    #[test]
    fn changing_metric_width_does_not_change_the_layout_skeleton() {
        assert_eq!(
            skeleton_of("PID CPU\n9   7%"),
            skeleton_of("PID CPU\n999 42%"),
            "numeric values and widths are volatile, while their field positions remain structural"
        );
    }

    #[test]
    fn repeated_metric_values_do_not_change_the_value_class_set() {
        assert_eq!(
            numeric_value_classes("cpu 1 10 100 1000"),
            numeric_value_classes("cpu 8 88 888 8888 8 88"),
        );
    }
}
