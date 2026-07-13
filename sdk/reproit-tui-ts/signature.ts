// signature.ts is the canonical TUI screen signature for the TypeScript SDK.
//
// THE CONTRACT (do not drift): the functions in this file are a byte-for-byte
// port of the canonical Rust crate crates/tui-sig/src/lib.rs (which the runner
// crates/reproit/src/backends/tui.rs shares directly). A TUI has no
// accessibility tree, so it lives in a SEPARATE namespace from the a11y golden
// vectors in signature_vectors.json: the descriptor SOURCE is the rendered
// terminal screen (the VT cell grid), not a Node role tree. What is shared with
// every other reproit surface is the HASH FAMILY: FNV-1a 32-bit (offset basis
// 0x811c9dc5, prime 0x01000193, 8-char zero-padded lowercase hex) and the
// value_class buckets with the strict period-decimal grammar. So a TUI signature
// is in the same 8-hex namespace as an a11y signature, it just is not expected to
// equal an a11y vector.
//
// The functions below mirror lib.rs one-to-one (same names, same logic):
//
//   sigOf                -> sig_of               (the FNV-1a primitive)
//   structuralClass      -> structural_class     (per-cell locale-invariant class)
//   skeletonOf           -> skeleton_of          (run-length layout skeleton)
//   numericValueClasses  -> numeric_value_classes (bounded Layer-2 value set)
//   valueClass           -> value_class          (the bucketer)
//   isStrictDecimal      -> is_strict_decimal     (the grammar)
//   contentFingerprint   -> content_fingerprint   (Layer-1 effect token)
//   structuralSig        -> structural_sig        (the state signature)
//   labelsOf             -> labels_of             (display-only word set)
//
// Pure, zero runtime dependencies. No em dashes anywhere, per project rules.

// MAX_VALUE_CLASSES mirrors lib.rs: a per-screen cap on numeric tokens folded
// into the signature, so an adversarial number-dense screen cannot explode the
// value-class section. Same value (8) as the oracle's per-node cap.
export const MAX_VALUE_CLASSES = 8;

// MAX_LABELS mirrors lib.rs (display-only label set cap).
export const MAX_LABELS = 24;

// CRITICAL parity note: the Rust sig_of hashes over the UTF-8 BYTES of the input
// (`for b in s.bytes()`). The browser web SDK hashes over UTF-16 char codes,
// which is fine there because its descriptor is pure ASCII, but a TUI screen
// carries multi-byte content (box-drawing glyphs U+2500.., CJK words). So this
// port MUST hash UTF-8 bytes, exactly like the Go SDK's `for i := range len(s)`
// over `s[i]`. We encode to UTF-8 with TextEncoder and fold the byte stream.
const UTF8 = new TextEncoder();

// sigOf is FNV-1a over a single pre-serialized string, the hashing primitive: the
// caller decides WHAT to feed it. The SAME FNV-1a 32-bit primitive (offset basis
// 0x811c9dc5, prime 0x01000193, 8-char zero-padded lowercase hex) as
// lib.rs::sig_of and the canonical oracle's fnv1a32_hex. Operates over the UTF-8
// BYTES of the string, exactly as the Rust `for b in s.bytes()` loop does.
export function sigOf(s: string): string {
  const bytes = UTF8.encode(s);
  let h = 0x811c9dc5;
  for (let i = 0; i < bytes.length; i++) {
    h ^= bytes[i];
    // Math.imul does 32-bit multiply; >>> 0 keeps it unsigned, matching Rust's
    // wrapping_mul on u32.
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  // 8-char zero-padded lowercase hex, matching Rust's format!("{h:08x}").
  return ("0000000" + (h >>> 0).toString(16)).slice(-8);
}

// structuralClass reduces one screen cell char to a locale-INVARIANT structural
// class, the SAME decision tree as lib.rs::structural_class:
//
//   - box-drawing / block glyphs (U+2500..U+259F) -> '#' (a structural edge)
//   - ASCII digit                                  -> '9' (a number lives here)
//   - any other alphanumeric (any language, CJK)   -> 'W' (a word placeholder)
//   - space, newline, or ASCII punctuation         -> kept verbatim (layout/symbol)
//   - any other whitespace                          -> ' '
//   - anything else exotic                          -> 'W'
//
// IMPORTANT ordering: lib.rs checks is_ascii_digit BEFORE is_alphanumeric and
// is_ascii_punctuation BEFORE the generic whitespace fallback, so this keeps that
// exact order. The Rust char predicates are reimplemented inline below (see the
// helpers) to match byte-for-byte. `c` is a single Unicode code point as a string
// (JS iteration over a string yields code points, so astral glyphs stay whole).
export function structuralClass(c: string): string {
  const cp = c.codePointAt(0) as number;
  if (cp >= 0x2500 && cp <= 0x259f) {
    return "#"; // box-drawing / block -> a structural edge marker
  } else if (isAsciiDigit(cp)) {
    return "9"; // a number lives here (value-agnostic)
  } else if (isAlphanumeric(cp)) {
    return "W"; // any word character (any language, incl. CJK) -> placeholder
  } else if (c === " " || c === "\n" || isAsciiPunctuation(cp)) {
    return c; // layout whitespace, or a non-localized symbol/hotkey/field marker
  } else if (isWhitespace(cp)) {
    return " "; // other whitespace -> space
  } else {
    return "W"; // anything else exotic -> treat as a word glyph
  }
}

// skeletonOf serializes the screen into its locale-invariant LAYOUT SKELETON, then
// collapses each maximal run of the SAME class char to a length-prefixed token.
// The SAME logic as lib.rs::skeleton_of:
//   - newline runs are emitted literally (no length prefix), preserving row count;
//   - volatile digit/space runs omit length; other runs append it when > 1.
//
// NOTE on iteration: lib.rs does contents.chars(), iterating Unicode SCALAR
// VALUES, so we iterate code points (the for..of over a string), not UTF-16 code
// units, so a multi-byte glyph is one classed char (not two surrogate halves).
export function skeletonOf(contents: string): string {
  const classed: string[] = [];
  for (const ch of contents) {
    classed.push(structuralClass(ch));
  }
  let out = "";
  let i = 0;
  while (i < classed.length) {
    const c = classed[i];
    let run = 1;
    while (i + run < classed.length && classed[i + run] === c) {
      run++;
    }
    i += run;
    if (c === "\n") {
      // newline runs delimit rows; emit them literally so row count/positions are
      // preserved without a noisy length prefix.
      out += "\n".repeat(run);
    } else {
      // a leading run-length captures the extent (border width, gap width,
      // word-field width) which is structural; single cells need no length.
      out += c;
      if (run > 1 && c !== "9" && c !== " ") {
        out += String(run);
      }
    }
  }
  return out;
}

// structuralSig is the screen's state signature: skeleton + bounded numeric
// value-class section + the cursor cell, hashed by sigOf. The SAME serialization
// as lib.rs::structural_sig:
//
//   "{skeleton}\x1ecur={row},{col}\x1eV:{classes joined by ,}"
//
// cursorRow/cursorCol are the 0-based cursor (row, col), the same (cursor.0,
// cursor.1) tuple lib.rs reads from screen().cursor_position(). The V: section is
// empty (byte-identical to a skeleton-only sig) when the screen has no numeric
// tokens, preserving the locale and word invariants.
export function structuralSig(
  contents: string,
  cursorRow: number,
  cursorCol: number,
): string {
  const skeleton = skeletonOf(contents);
  const vclasses = numericValueClasses(contents);
  const input =
    skeleton +
    "\x1ecur=" +
    String(cursorRow >>> 0) +
    "," +
    String(cursorCol >>> 0) +
    "\x1eV:" +
    vclasses.join(",");
  return sigOf(input);
}

// numericValueClasses extracts the screen's numeric tokens and maps each to the
// SAME value-class bucket the oracle uses, then returns a BOUNDED, SORTED array of
// those buckets. The SAME scan as lib.rs::numeric_value_classes: a token starts at
// a digit, or at a sign/period immediately followed by a digit; it then consumes
// digits and periods; the run is classified by valueClass. Buckets are sorted
// (string sort) and capped at MAX_VALUE_CLASSES.
//
// NOTE on iteration: lib.rs collects contents.chars() into a Vec<char> and indexes
// by scalar position, so this walks an array of code points (Array.from(contents)),
// not UTF-16 units, to keep token boundaries identical for multi-byte input.
export function numericValueClasses(contents: string): string[] {
  const chars = Array.from(contents);
  const classes: string[] = [];
  let i = 0;
  while (i < chars.length) {
    const c = chars[i];
    const next = i + 1 < chars.length ? chars[i + 1] : "";
    const starts =
      isAsciiDigitStr(c) ||
      ((c === "+" || c === "-" || c === ".") && isAsciiDigitStr(next));
    if (!starts) {
      i++;
      continue;
    }
    const start = i;
    // consume the optional leading sign / period already matched above.
    if (chars[i] === "+" || chars[i] === "-" || chars[i] === ".") {
      i++;
    }
    while (i < chars.length && (isAsciiDigitStr(chars[i]) || chars[i] === ".")) {
      i++;
    }
    const token = chars.slice(start, i).join("");
    classes.push(valueClass(token));
  }
  const unique = [...new Set(classes)].sort();
  if (unique.length > MAX_VALUE_CLASSES) {
    unique.length = MAX_VALUE_CLASSES;
  }
  return unique;
}

// valueClass maps a numeric value string to the SAME bounded value-class token the
// oracle uses. The SAME buckets and the same strict period-decimal grammar as
// lib.rs::value_class: EMPTY / ZERO / NEG / POS1 / POS2 / POS3 / POSL, with
// NONEMPTY as the locale-safe fallback for anything outside the strict grammar.
export function valueClass(s: string): string {
  const t = s.trim();
  if (t.length === 0) {
    return "EMPTY";
  }
  if (isStrictDecimal(t)) {
    // Parse is safe: the grammar is a subset of JS number syntax.
    const n = Number(t);
    const a = Math.abs(n);
    if (n === 0) {
      return "ZERO";
    } else if (n < 0) {
      return "NEG";
    } else if (a < 10) {
      return "POS1";
    } else if (a < 100) {
      return "POS2";
    } else if (a < 1000) {
      return "POS3";
    } else {
      return "POSL";
    }
  }
  return "NONEMPTY";
}

// isStrictDecimal is the strict ^[+-]?[0-9]+(\.[0-9]+)?$ grammar: optional sign,
// one or more ASCII digits, optionally a period followed by one or more ASCII
// digits. No grouping separators, no exponent, no leading/trailing dot. The SAME
// rule as lib.rs::is_strict_decimal, operating over UTF-8 bytes (ASCII digits are
// single-byte, so we walk char codes which are exact for the ASCII subset here).
export function isStrictDecimal(s: string): boolean {
  let i = 0;
  const n = s.length;
  if (i < n && (s.charCodeAt(i) === 43 || s.charCodeAt(i) === 45)) {
    i++; // + or -
  }
  const intStart = i;
  while (i < n && s.charCodeAt(i) >= 48 && s.charCodeAt(i) <= 57) {
    i++;
  }
  if (i === intStart) {
    return false; // need at least one integer digit
  }
  if (i < n && s.charCodeAt(i) === 46) {
    // '.'
    i++;
    const fracStart = i;
    while (i < n && s.charCodeAt(i) >= 48 && s.charCodeAt(i) <= 57) {
      i++;
    }
    if (i === fracStart) {
      return false; // a trailing dot with no fraction digits is not allowed
    }
  }
  return i === n;
}

// contentFingerprint is the runner-local CONTENT FINGERPRINT over the FULL raw
// screen text plus the cursor cell. The SAME as lib.rs::content_fingerprint: hash
// of "{contents}\x1ecur={row},{col}". This is the Layer-1 effect-detection token:
// it changes whenever ANY on-screen value changes, even when the skeleton
// signature is frozen. It is EPHEMERAL and must NEVER enter the canonical state
// set; the SDK uses it only to decide whether an action did anything.
export function contentFingerprint(
  contents: string,
  cursorRow: number,
  cursorCol: number,
): string {
  const input =
    contents + "\x1ecur=" + String(cursorRow >>> 0) + "," + String(cursorCol >>> 0);
  return sigOf(input);
}

// labelsOf is the display-only word set (the `labels` field), the SAME as
// lib.rs::labels_of: blank box/block glyphs, split on whitespace, strip
// surrounding non-alphanumerics, cap token code-point width at 40, dedup, sort
// (a BTreeSet iterates sorted), take MAX_LABELS. Never enters the signature.
export function labelsOf(contents: string): string[] {
  let cleaned = "";
  for (const ch of contents) {
    const cp = ch.codePointAt(0) as number;
    if (cp >= 0x2500 && cp <= 0x259f) {
      cleaned += " ";
    } else {
      cleaned += ch;
    }
  }
  const set = new Set<string>();
  // Rust split_whitespace splits on the Unicode White_Space property; our
  // isWhitespace mirrors that set. Reuse it for the split.
  for (const raw of splitWhitespace(cleaned)) {
    const t = trimNonAlphanumeric(raw);
    if (t.length > 0 && Array.from(t).length <= 40) {
      set.add(t);
    }
  }
  const out = Array.from(set);
  out.sort();
  if (out.length > MAX_LABELS) {
    out.length = MAX_LABELS;
  }
  return out;
}

// ---- Rust char-predicate parity helpers -------------------------------------
//
// These reimplement the exact Rust `char` predicates used in lib.rs so the
// structural classification matches byte-for-byte. We do NOT use JS regex \w /
// \d / \s blindly because Rust's predicates have specific definitions we must
// mirror (and JS \d/\s defaults are ASCII-or-Unicode-flag dependent).

// isAsciiDigit mirrors Rust char::is_ascii_digit: '0'..='9' only. Code-point form.
function isAsciiDigit(cp: number): boolean {
  return cp >= 0x30 && cp <= 0x39;
}
// String convenience for the numeric scan (a 1-char code-point string).
function isAsciiDigitStr(c: string): boolean {
  if (c.length === 0) return false;
  return isAsciiDigit(c.codePointAt(0) as number);
}

// isAsciiPunctuation mirrors Rust char::is_ascii_punctuation: the four ASCII
// punctuation ranges !"#$%&'()*+,-./ : ; < = > ? @ [ \ ] ^ _ ` { | } ~.
function isAsciiPunctuation(cp: number): boolean {
  return (
    (cp >= 0x21 && cp <= 0x2f) || // ! .. /
    (cp >= 0x3a && cp <= 0x40) || // : .. @
    (cp >= 0x5b && cp <= 0x60) || // [ .. `
    (cp >= 0x7b && cp <= 0x7e) //   { .. ~
  );
}

// isWhitespace mirrors Rust char::is_whitespace (the Unicode White_Space
// property). The set is small and fixed; enumerate it by code point to match Rust
// exactly rather than rely on JS \s (which differs: \s includes U+FEFF BOM and
// excludes U+0085 NEL). This is the full Unicode White_Space set, the same code
// points the Go SDK pins.
function isWhitespace(cp: number): boolean {
  switch (cp) {
    case 0x0009: // \t
    case 0x000a: // \n
    case 0x000b: // VT
    case 0x000c: // FF
    case 0x000d: // \r
    case 0x0020: // space
    case 0x0085: // NEL
    case 0x00a0: // no-break space
    case 0x1680: // ogham space mark
    case 0x2000:
    case 0x2001:
    case 0x2002:
    case 0x2003:
    case 0x2004:
    case 0x2005:
    case 0x2006:
    case 0x2007:
    case 0x2008:
    case 0x2009:
    case 0x200a: // en/em/thin/etc. spaces
    case 0x2028: // line separator
    case 0x2029: // paragraph separator
    case 0x202f: // narrow no-break space
    case 0x205f: // medium mathematical space
    case 0x3000: // ideographic space
      return true;
    default:
      return false;
  }
}

// isAlphanumeric mirrors Rust char::is_alphanumeric == is_alphabetic ||
// is_numeric. Rust uses the Unicode Alphabetic property and the Numeric type
// (Nd|Nl|No). We use the JS Unicode property escapes \p{Alphabetic} and \p{Nd}
// /\p{Nl}/\p{No}, which map to those same Unicode properties. This is exact for
// every glyph a terminal renders that reaches this branch: ASCII digits are
// already handled before structuralClass calls this, and any word glyph collapses
// to 'W' either way, so only the structurally-irrelevant identity could ever
// theoretically diverge on an exotic code point.
const ALPHANUMERIC = /[\p{Alphabetic}\p{Nd}\p{Nl}\p{No}]/u;
function isAlphanumeric(cp: number): boolean {
  return ALPHANUMERIC.test(String.fromCodePoint(cp));
}

// splitWhitespace mirrors Rust str::split_whitespace: split on maximal runs of
// White_Space chars, dropping empty leading/trailing fields. Uses isWhitespace so
// the split set matches Rust exactly.
function splitWhitespace(s: string): string[] {
  const out: string[] = [];
  let cur = "";
  for (const ch of s) {
    if (isWhitespace(ch.codePointAt(0) as number)) {
      if (cur.length > 0) {
        out.push(cur);
        cur = "";
      }
    } else {
      cur += ch;
    }
  }
  if (cur.length > 0) out.push(cur);
  return out;
}

// trimNonAlphanumeric mirrors Rust trim_matches(|c| !c.is_alphanumeric()):
// strip leading and trailing code points that are not alphanumeric.
function trimNonAlphanumeric(s: string): string {
  const chars = Array.from(s);
  let lo = 0;
  let hi = chars.length;
  while (lo < hi && !isAlphanumeric(chars[lo].codePointAt(0) as number)) lo++;
  while (hi > lo && !isAlphanumeric(chars[hi - 1].codePointAt(0) as number)) hi--;
  return chars.slice(lo, hi).join("");
}
