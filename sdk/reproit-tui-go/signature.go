// Package reproittui is the production SDK a Go terminal-UI application embeds to
// report sessions, coverage edges and crash signatures to the reproit cloud, and
// to compute the SAME canonical TUI screen signature the fuzz runner computes.
//
// THE CONTRACT (do not drift): the signature core in this file is a byte-for-byte
// port of crates/reproit/src/backends/tui.rs. A TUI has no accessibility tree, so
// it lives in a SEPARATE namespace from the a11y golden vectors in
// signature_vectors.json: the descriptor SOURCE is the rendered terminal screen
// (the VT cell grid), not a Node role tree. What is shared with every other
// surface is the HASH FAMILY: FNV-1a 32-bit (offset basis 0x811c9dc5, prime
// 0x01000193, 8-char zero-padded lowercase hex) and the value_class buckets with
// the strict period-decimal grammar. So a TUI signature is in the same 8-hex
// namespace as an a11y signature, it just is not expected to equal an a11y vector.
//
// The functions below mirror tui.rs one-to-one (same names, same logic):
//
//	SigOf                -> sig_of            (the FNV-1a primitive)
//	structuralClass      -> structural_class  (per-cell locale-invariant class)
//	skeletonOf           -> skeleton_of       (run-length layout skeleton)
//	numericValueClasses  -> numeric_value_classes (bounded Layer-2 value set)
//	valueClass           -> value_class       (the bucketer)
//	isStrictDecimal      -> is_strict_decimal  (the grammar)
//	contentFingerprint   -> content_fingerprint (Layer-1 effect token)
//	StructuralSig        -> structural_sig    (the state signature)
//	labelsOf             -> labels_of         (display-only word set)
//	ScreenContents       -> the vt100 screen().contents() text model
//
// No em dashes anywhere in this package, per project rules.
package reproittui

import (
	"sort"
	"strconv"
	"strings"
	"unicode"
)

// maxValueClasses mirrors tui.rs MAX_VALUE_CLASSES: a per-screen cap on numeric
// tokens folded into the signature, so an adversarial number-dense screen cannot
// explode the value-class section. Same value (8) as the oracle's per-node cap.
const maxValueClasses = 8

// maxLabels mirrors tui.rs MAX_LABELS (display-only label set cap).
const maxLabels = 24

// SigOf is FNV-1a over a single pre-serialized string: the hashing primitive, the
// caller decides WHAT to feed it. This is the SAME FNV-1a 32-bit primitive (offset
// basis 0x811c9dc5, prime 0x01000193, 8-char zero-padded lowercase hex) as
// tui.rs::sig_of and the canonical oracle's fnv1a32_hex. Operates over the UTF-8
// BYTES of the string, exactly as the Rust `for b in s.bytes()` loop does.
func SigOf(s string) string {
	var h uint32 = 0x811c9dc5
	for i := 0; i < len(s); i++ { // range over bytes, not runes, to match s.bytes()
		h ^= uint32(s[i])
		h *= 0x01000193 // wrapping_mul: uint32 overflow wraps in Go, matching Rust
	}
	// 8-char zero-padded lowercase hex, matching Rust's format!("{h:08x}").
	return zeroPadHex8(h)
}

func zeroPadHex8(h uint32) string {
	const hexd = "0123456789abcdef"
	var b [8]byte
	for i := 7; i >= 0; i-- {
		b[i] = hexd[h&0xf]
		h >>= 4
	}
	return string(b[:])
}

// structuralClass reduces one screen cell rune to a locale-INVARIANT structural
// class, byte-for-byte the same decision tree as tui.rs::structural_class:
//
//   - box-drawing / block glyphs (U+2500..U+259F) -> '#' (a structural edge)
//   - ASCII digit                                 -> '9' (a number lives here)
//   - any other alphanumeric (any language, CJK)  -> 'W' (a word placeholder)
//   - space, newline, or ASCII punctuation        -> kept verbatim (layout/symbol)
//   - any other whitespace                         -> ' '
//   - anything else exotic                          -> 'W'
//
// IMPORTANT ordering note: tui.rs checks is_ascii_digit BEFORE is_alphanumeric and
// is_ascii_punctuation BEFORE the generic whitespace fallback, so this function
// must keep that exact order. Go's unicode helpers are reimplemented inline below
// to match Rust's char predicates precisely (see the helpers).
func structuralClass(c rune) rune {
	switch {
	case c >= 0x2500 && c <= 0x259f:
		return '#'
	case isASCIIDigit(c):
		return '9'
	case isAlphanumeric(c):
		return 'W'
	case c == ' ' || c == '\n' || isASCIIPunctuation(c):
		return c
	case isWhitespace(c):
		return ' '
	default:
		return 'W'
	}
}

// skeletonOf serializes the screen into its locale-invariant LAYOUT SKELETON, then
// collapses each maximal run of the SAME class char to a length-prefixed token.
// Byte-for-byte the same as tui.rs::skeleton_of:
//   - newline runs are emitted literally (no length prefix), preserving row count;
//   - any other run emits the class char, then the run length IF run > 1.
func skeletonOf(contents string) string {
	classed := make([]rune, 0, len(contents))
	for _, c := range contents {
		classed = append(classed, structuralClass(c))
	}
	var out strings.Builder
	i := 0
	for i < len(classed) {
		c := classed[i]
		run := 1
		for i+run < len(classed) && classed[i+run] == c {
			run++
		}
		i += run
		if c == '\n' {
			for k := 0; k < run; k++ {
				out.WriteByte('\n')
			}
		} else {
			out.WriteRune(c)
			if run > 1 {
				out.WriteString(strconv.Itoa(run))
			}
		}
	}
	return out.String()
}

// StructuralSig is the screen's state signature: skeleton + bounded numeric
// value-class section + the cursor cell, hashed by SigOf. Byte-for-byte the same
// serialization as tui.rs::structural_sig:
//
//	"{skeleton}\x1ecur={row},{col}\x1eV:{classes joined by ,}"
//
// cursorRow/cursorCol are the parser's 0-based cursor (row, col), the same
// (cursor.0, cursor.1) tuple tui.rs reads from screen().cursor_position(). The V:
// section is empty (byte-identical to a skeleton-only sig) when the screen has no
// numeric tokens, preserving the locale and word invariants.
func StructuralSig(contents string, cursorRow, cursorCol uint16) string {
	skeleton := skeletonOf(contents)
	vclasses := numericValueClasses(contents)
	input := skeleton + "\x1ecur=" +
		strconv.FormatUint(uint64(cursorRow), 10) + "," +
		strconv.FormatUint(uint64(cursorCol), 10) +
		"\x1eV:" + strings.Join(vclasses, ",")
	return SigOf(input)
}

// numericValueClasses extracts the screen's numeric tokens and maps each to the
// SAME value-class bucket the oracle uses, then returns a BOUNDED, SORTED slice of
// those buckets. Byte-for-byte the same scan as tui.rs::numeric_value_classes:
// a token starts at a digit, or at a sign/period immediately followed by a digit;
// it then consumes digits and periods; the run is classified by valueClass.
// Buckets are sorted (string sort) and capped at maxValueClasses.
//
// NOTE on iteration: tui.rs collects contents.chars() into a Vec<char> and indexes
// by rune position, so this walks a []rune (not bytes) to keep token boundaries
// identical for multi-byte input.
func numericValueClasses(contents string) []string {
	chars := []rune(contents)
	classes := make([]string, 0)
	i := 0
	for i < len(chars) {
		c := chars[i]
		startsTok := isASCIIDigit(c) ||
			((c == '+' || c == '-' || c == '.') &&
				i+1 < len(chars) && isASCIIDigit(chars[i+1]))
		if !startsTok {
			i++
			continue
		}
		start := i
		if chars[i] == '+' || chars[i] == '-' || chars[i] == '.' {
			i++
		}
		for i < len(chars) && (isASCIIDigit(chars[i]) || chars[i] == '.') {
			i++
		}
		token := string(chars[start:i])
		classes = append(classes, valueClass(token))
	}
	sort.Strings(classes)
	if len(classes) > maxValueClasses {
		classes = classes[:maxValueClasses]
	}
	return classes
}

// valueClass maps a numeric value string to the SAME bounded value-class token the
// oracle uses. Byte-for-byte the same buckets and the same strict period-decimal
// grammar as tui.rs::value_class (which reproduces the oracle's value_class):
// EMPTY / ZERO / NEG / POS1 / POS2 / POS3 / POSL, with NONEMPTY as the locale-safe
// fallback for anything outside the strict grammar.
func valueClass(s string) string {
	t := strings.TrimSpace(s)
	if t == "" {
		return "EMPTY"
	}
	if isStrictDecimal(t) {
		n, err := strconv.ParseFloat(t, 64)
		if err != nil {
			// Unreachable for strict-decimal input (a subset of float syntax),
			// but match the Rust unwrap_or(NaN): NaN compares false everywhere,
			// landing in the final POSL bucket, so mirror that.
			return "POSL"
		}
		a := n
		if a < 0 {
			a = -a
		}
		switch {
		case n == 0.0:
			return "ZERO"
		case n < 0.0:
			return "NEG"
		case a < 10.0:
			return "POS1"
		case a < 100.0:
			return "POS2"
		case a < 1000.0:
			return "POS3"
		default:
			return "POSL"
		}
	}
	return "NONEMPTY"
}

// isStrictDecimal is the strict ^[+-]?[0-9]+(\.[0-9]+)?$ grammar: optional sign,
// one or more ASCII digits, optionally a period followed by one or more ASCII
// digits. No grouping separators, no exponent, no leading/trailing dot. Byte-for-
// byte the same as tui.rs::is_strict_decimal, operating over UTF-8 bytes (ASCII
// digits are single-byte, so byte indexing is exact).
func isStrictDecimal(s string) bool {
	b := []byte(s)
	i := 0
	if i < len(b) && (b[i] == '+' || b[i] == '-') {
		i++
	}
	intStart := i
	for i < len(b) && b[i] >= '0' && b[i] <= '9' {
		i++
	}
	if i == intStart {
		return false // need at least one integer digit
	}
	if i < len(b) && b[i] == '.' {
		i++
		fracStart := i
		for i < len(b) && b[i] >= '0' && b[i] <= '9' {
			i++
		}
		if i == fracStart {
			return false // a trailing dot with no fraction digits is not allowed
		}
	}
	return i == len(b)
}

// contentFingerprint is the runner-local CONTENT FINGERPRINT over the FULL raw
// screen text plus the cursor cell. Byte-for-byte the same as
// tui.rs::content_fingerprint: hash of "{contents}\x1ecur={row},{col}". This is
// the Layer-1 effect-detection token: it changes whenever ANY on-screen value
// changes, even when the skeleton signature is frozen. It is EPHEMERAL and must
// NEVER enter the canonical state set; the SDK uses it only to decide whether an
// action did anything. Exported as ContentFingerprint for callers who want it.
func contentFingerprint(contents string, cursorRow, cursorCol uint16) string {
	input := contents + "\x1ecur=" +
		strconv.FormatUint(uint64(cursorRow), 10) + "," +
		strconv.FormatUint(uint64(cursorCol), 10)
	return SigOf(input)
}

// ContentFingerprint is the exported Layer-1 effect token (see contentFingerprint).
func ContentFingerprint(contents string, cursorRow, cursorCol uint16) string {
	return contentFingerprint(contents, cursorRow, cursorCol)
}

// labelsOf is the display-only word set (the `labels` field), byte-for-byte the
// same as tui.rs::labels_of: blank box/block glyphs, split on whitespace, strip
// surrounding non-alphanumerics, cap token rune-width at 40, dedup, sort, take
// maxLabels. Never enters the signature.
func labelsOf(contents string) []string {
	var cleaned strings.Builder
	for _, c := range contents {
		if c >= 0x2500 && c <= 0x259f {
			cleaned.WriteByte(' ')
		} else {
			cleaned.WriteRune(c)
		}
	}
	set := map[string]struct{}{}
	for _, raw := range strings.Fields(cleaned.String()) {
		t := strings.TrimFunc(raw, func(c rune) bool { return !isAlphanumeric(c) })
		if t != "" && len([]rune(t)) <= 40 {
			set[t] = struct{}{}
		}
	}
	out := make([]string, 0, len(set))
	for k := range set {
		out = append(out, k)
	}
	sort.Strings(out) // BTreeSet iterates in sorted order; match that
	if len(out) > maxLabels {
		out = out[:maxLabels]
	}
	return out
}

// LabelsOf is the exported display-only label set (see labelsOf).
func LabelsOf(contents string) []string { return labelsOf(contents) }

// ---- Rust char-predicate parity helpers -----------------------------------
//
// These reimplement the exact Rust `char` predicates used in tui.rs so the
// structural classification matches byte-for-byte. We do NOT use unicode.IsDigit
// /unicode.IsLetter blindly because Rust's is_ascii_digit and char::is_alphanumeric
// have specific definitions we must mirror.

// isASCIIDigit mirrors Rust char::is_ascii_digit: '0'..='9' only.
func isASCIIDigit(c rune) bool { return c >= '0' && c <= '9' }

// isASCIIPunctuation mirrors Rust char::is_ascii_punctuation: the ASCII punctuation
// ranges !"#$%&'()*+,-./ : ; < = > ? @ [ \ ] ^ _ ` { | } ~.
func isASCIIPunctuation(c rune) bool {
	return (c >= '!' && c <= '/') ||
		(c >= ':' && c <= '@') ||
		(c >= '[' && c <= '`') ||
		(c >= '{' && c <= '~')
}

// isWhitespace mirrors Rust char::is_whitespace (the Unicode White_Space property).
// The set is small and fixed; enumerate it by code point to match Rust exactly
// rather than rely on Go's unicode.IsSpace, which can diverge on edge cases. This
// is the full Unicode White_Space set as of the Rust stdlib used by tui.rs.
func isWhitespace(c rune) bool {
	switch c {
	case 0x0009, 0x000A, 0x000B, 0x000C, 0x000D, // \t \n VT FF \r
		0x0020, // space
		0x0085, // NEL
		0x00A0, // no-break space
		0x1680, // ogham space mark
		0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007,
		0x2008, 0x2009, 0x200A, // en/em/thin/etc. spaces
		0x2028, 0x2029, // line/paragraph separator
		0x202F, // narrow no-break space
		0x205F, // medium mathematical space
		0x3000: // ideographic space
		return true
	}
	return false
}

// isAlphanumeric mirrors Rust char::is_alphanumeric == is_alphabetic || is_numeric.
// Rust uses the Unicode Alphabetic property and the Numeric type (Nd|Nl|No). Go's
// unicode.IsLetter covers L* (matching Alphabetic for the common case) and
// unicode.IsNumber covers N* (Nd|Nl|No), matching Rust's is_numeric. We OR them.
// This is exact for every glyph a terminal renders that reaches this branch:
// ASCII digits are already handled before structuralClass calls this, and any
// word glyph collapses to 'W' either way.
func isAlphanumeric(c rune) bool {
	return unicode.IsLetter(c) || unicode.IsNumber(c)
}
