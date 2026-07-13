"""Canonical TUI screen signature for terminal-UI apps (Textual / Rich / urwid /
prompt_toolkit).

THE CONTRACT (do not drift): this module is a faithful Python port of the Rust
crate crates/tui-sig/src/lib.rs (the source of truth the runner
crates/reproit/src/backends/tui.rs shares directly). It is a SIBLING port of the
Go SDK (sdk/reproit-tui-go/signature.go) and the Rust/TS TUI SDKs, and it matches
the Python a11y signature ports' style (runners/linux-atspi.py,
sdk/reproit-linux/reproit_linux/signature.py) for reproducing Rust char predicates
with exact code-point ranges.

CRITICAL: TUI signatures live in a SEPARATE NAMESPACE from the accessibility-tree
(a11y) golden vectors in signature_vectors.json. The descriptor SOURCE here is the
rendered terminal screen (the VT cell grid), normalized to a locale-invariant
layout skeleton, NOT a Node role tree. The parity target is
tui_signature_vectors.json at the repo root. What IS shared with every other
reproit surface is the HASH FAMILY: FNV-1a 32-bit (offset basis 0x811c9dc5, prime
0x01000193, 8-char zero-padded lowercase hex) and the value-class buckets with the
strict period-decimal grammar. So a TUI signature is in the same 8-hex namespace
as an a11y signature, it just is not expected to equal an a11y vector.

The functions below mirror crates/tui-sig/src/lib.rs one-to-one (same names,
same logic):

    sig_of                 -> sig_of               (the FNV-1a primitive)
    structural_class       -> structural_class     (per-cell locale-invariant class)
    skeleton_of            -> skeleton_of           (run-length layout skeleton)
    numeric_value_classes  -> numeric_value_classes (bounded Layer-2 value set)
    value_class            -> value_class           (the bucketer)
    is_strict_decimal      -> is_strict_decimal     (the grammar)
    content_fingerprint    -> content_fingerprint   (Layer-1 effect token)
    structural_sig         -> structural_sig        (the state signature)
    labels_of              -> labels_of             (display-only word set)

Pure stdlib, no third-party dependency, so the parity test runs on any host with
no Textual/Rich/urwid installed.

No em dashes anywhere, per project rules.
"""

import unicodedata

# Cap on numeric value-classes folded into the signature, so an adversarial
# number-dense screen cannot explode the value-class section. Mirrors
# crates/tui-sig MAX_VALUE_CLASSES (the oracle's per-node cap of 8).
MAX_VALUE_CLASSES = 8

# Cap on the display-only label set, mirroring crates/tui-sig MAX_LABELS.
MAX_LABELS = 24


def sig_of(s):
    """FNV-1a, 32-bit, over the UTF-8 BYTES of `s`; 8-char zero-padded lowercase
    hex. Identical primitive to crates/tui-sig::sig_of and the a11y oracle's
    fnv1a32_hex (offset basis 0x811c9dc5, prime 0x01000193). Hashing the UTF-8
    bytes matches the Rust `for b in s.bytes()` loop exactly."""
    h = 0x811C9DC5
    for b in s.encode("utf-8"):
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return "%08x" % h


# ---- Rust char-predicate parity helpers ------------------------------------
#
# These reimplement the exact Rust `char` predicates used in crates/tui-sig so the
# structural classification matches byte-for-byte. We do NOT lean on Python's
# str.isalnum()/isspace() blindly because Rust's is_ascii_digit,
# char::is_alphanumeric, char::is_ascii_punctuation and char::is_whitespace have
# specific definitions we must mirror (this is the same care the Go SDK takes in
# its "Rust char-predicate parity helpers" section and the same style the Linux
# Python signature port uses for ASCII-range tests).

def _is_ascii_digit(c):
    """Rust char::is_ascii_digit: '0'..='9' only."""
    return "0" <= c <= "9"


def _is_ascii_punctuation(c):
    """Rust char::is_ascii_punctuation: the ASCII punctuation ranges
    !"#$%&'()*+,-./ : ; < = > ? @ [ \\ ] ^ _ ` { | } ~ (and nothing else)."""
    o = ord(c)
    return (
        (0x21 <= o <= 0x2F)  # ! " # $ % & ' ( ) * + , - . /
        or (0x3A <= o <= 0x40)  # : ; < = > ? @
        or (0x5B <= o <= 0x60)  # [ \ ] ^ _ `
        or (0x7B <= o <= 0x7E)  # { | } ~
    )


# Rust char::is_whitespace is the Unicode White_Space property. The set is small
# and fixed; enumerate it by code point to match Rust exactly rather than rely on
# str.isspace(), which diverges (it treats some non-White_Space separators and the
# FS/GS/RS/US controls as whitespace, which Rust does NOT). This is the full
# Unicode White_Space set, matching the Go SDK's isWhitespace.
_WHITESPACE = frozenset(
    [
        0x0009, 0x000A, 0x000B, 0x000C, 0x000D,  # \t \n VT FF \r
        0x0020,  # space
        0x0085,  # NEL
        0x00A0,  # no-break space
        0x1680,  # ogham space mark
        0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007,
        0x2008, 0x2009, 0x200A,  # en/em/thin/etc. spaces
        0x2028, 0x2029,  # line/paragraph separator
        0x202F,  # narrow no-break space
        0x205F,  # medium mathematical space
        0x3000,  # ideographic space
    ]
)


def _is_whitespace(c):
    """Rust char::is_whitespace (Unicode White_Space)."""
    return ord(c) in _WHITESPACE


def _is_alphanumeric(c):
    """Rust char::is_alphanumeric == char::is_alphabetic || char::is_numeric.
    Rust's is_alphabetic is the Unicode Alphabetic property; is_numeric is the
    Numeric_Type / general categories Nd, Nl, No. Python's str.isalpha() covers
    Unicode letters and str.isdigit()/category tests cover numbers. To match Rust
    precisely we test:
      - is_alphabetic: unicodedata category starts with 'L' (Lu/Ll/Lt/Lm/Lo), OR
        the char has the Alphabetic property via str.isalpha() (covers Nl letters
        like Roman numerals that Rust's is_alphabetic also accepts). We use the
        broader of the two, OR'd with the numeric test below.
      - is_numeric: general category in {Nd, Nl, No}.
    For every glyph a terminal renders that reaches this branch the result is
    exact: ASCII digits are handled BEFORE this is called, and any word glyph
    collapses to 'W' either way, so the only thing that matters is letter-vs-symbol
    which both libraries agree on for real screen text."""
    cat = unicodedata.category(c)
    if cat[0] == "L":  # Lu Ll Lt Lm Lo -> alphabetic
        return True
    if cat in ("Nd", "Nl", "No"):  # numeric types Rust's is_numeric accepts
        return True
    # Rust is_alphabetic also accepts a few Other_Alphabetic code points that are
    # not category L (e.g. some combining marks / letter-like symbols); Python's
    # str.isalpha() likewise reflects the Alphabetic property, so fall back to it.
    return c.isalpha()


def structural_class(c):
    """Reduce one screen cell character to a locale-INVARIANT structural class.
    Byte-for-byte the same decision tree as crates/tui-sig::structural_class:

      - box-drawing / block glyphs (U+2500..U+259F) -> '#' (a structural edge)
      - ASCII digit                                  -> '9' (a number lives here)
      - any other alphanumeric (any language, CJK)   -> 'W' (a word placeholder)
      - space, newline, or ASCII punctuation         -> kept verbatim
      - any other whitespace                          -> ' '
      - anything else exotic                          -> 'W'

    IMPORTANT ordering: is_ascii_digit is checked BEFORE is_alphanumeric, and
    ascii-punctuation BEFORE the generic whitespace fallback, exactly as the Rust
    if/else-if chain does."""
    if "─" <= c <= "▟":
        return "#"  # box-drawing / block -> a structural edge marker
    if _is_ascii_digit(c):
        return "9"  # a number lives here (value-agnostic)
    if _is_alphanumeric(c):
        return "W"  # any word character (any language, incl. CJK) -> placeholder
    if c == " " or c == "\n" or _is_ascii_punctuation(c):
        return c  # layout whitespace, or a non-localized symbol/hotkey, kept
    if _is_whitespace(c):
        return " "  # other whitespace -> space
    return "W"  # anything else exotic -> treat as a word glyph


def skeleton_of(contents):
    """Serialize the screen into its locale-invariant LAYOUT SKELETON, then
    collapse each maximal run of the SAME class char to a length-prefixed token.
    Byte-for-byte the same as crates/tui-sig::skeleton_of:
      - newline runs are emitted literally (no length prefix), preserving rows;
      - digit and space widths omit their volatile run length;
      - other runs append their length when greater than one."""
    classed = [structural_class(c) for c in contents]
    out = []
    i = 0
    n = len(classed)
    while i < n:
        c = classed[i]
        run = 1
        while i + run < n and classed[i + run] == c:
            run += 1
        i += run
        if c == "\n":
            out.append("\n" * run)
        else:
            out.append(c)
            if run > 1 and c != "9" and c != " ":
                out.append(str(run))
    return "".join(out)


def is_strict_decimal(s):
    r"""Strict `^[+-]?[0-9]+(\.[0-9]+)?$`: optional sign, one or more ASCII
    digits, optionally a period plus one or more ASCII digits. No grouping
    separators, no exponent, no leading/trailing dot, ASCII digits only.
    Byte-for-byte the same as crates/tui-sig::is_strict_decimal (and the a11y
    oracle's). ASCII digits are single code points, so character indexing is
    exact."""
    i = 0
    n = len(s)
    if i < n and (s[i] == "+" or s[i] == "-"):
        i += 1
    int_start = i
    while i < n and "0" <= s[i] <= "9":
        i += 1
    if i == int_start:
        return False  # need at least one integer digit
    if i < n and s[i] == ".":
        i += 1
        frac_start = i
        while i < n and "0" <= s[i] <= "9":
            i += 1
        if i == frac_start:
            return False  # a trailing dot with no fraction digits is not allowed
    return i == n


def value_class(s):
    r"""Map a numeric value string to the SAME bounded value-class token the
    canonical oracle uses (crates/tui-sig::value_class): EMPTY / ZERO / NEG /
    POS1 / POS2 / POS3 / POSL via the strict period-decimal grammar, with
    NONEMPTY as the locale-safe fallback for anything outside it. Identical
    buckets and grammar to the a11y oracle's value_class."""
    t = s.strip()
    if t == "":
        return "EMPTY"
    if is_strict_decimal(t):
        # Parse is safe: the grammar is a subset of float's accepted syntax.
        n = float(t)
        a = abs(n)
        if n == 0.0:
            return "ZERO"
        if n < 0.0:
            return "NEG"
        if a < 10.0:
            return "POS1"
        if a < 100.0:
            return "POS2"
        if a < 1000.0:
            return "POS3"
        return "POSL"
    return "NONEMPTY"


def numeric_value_classes(contents):
    """Extract the screen's numeric tokens and map each to the SAME value-class
    bucket the oracle uses, then return a BOUNDED, SORTED list of those buckets.
    Byte-for-byte the same scan as crates/tui-sig::numeric_value_classes: a token
    starts at a digit, or at a sign/period immediately followed by a digit; it
    then consumes digits and periods; the run is classified by value_class.
    Buckets are sorted (string sort) and truncated to MAX_VALUE_CLASSES.

    NOTE on iteration: the Rust code collects contents.chars() into a Vec<char>
    and indexes by char position, so this walks the list of characters (code
    points), keeping token boundaries identical for multi-byte input."""
    chars = list(contents)
    classes = []
    i = 0
    n = len(chars)
    while i < n:
        c = chars[i]
        starts = _is_ascii_digit(c) or (
            c in ("+", "-", ".")
            and i + 1 < n
            and _is_ascii_digit(chars[i + 1])
        )
        if not starts:
            i += 1
            continue
        start = i
        # consume the optional leading sign / period already matched above.
        if chars[i] in ("+", "-", "."):
            i += 1
        while i < n and (_is_ascii_digit(chars[i]) or chars[i] == "."):
            i += 1
        token = "".join(chars[start:i])
        classes.append(value_class(token))
    return sorted(set(classes))[:MAX_VALUE_CLASSES]


def structural_sig(contents, cursor):
    """THE canonical TUI state signature: skeleton + bounded numeric value-class
    section + the cursor cell, hashed by sig_of. Byte-for-byte the same
    serialization as crates/tui-sig::structural_sig:

        "{skeleton}\\x1ecur={row},{col}\\x1eV:{classes joined by ,}"

    `cursor` is a (row, col) tuple of 0-based ints, the same (cursor.0, cursor.1)
    the runner reads from the parser's cursor position. The V: section is empty
    (byte-identical to a skeleton-only sig) when the screen has no numeric tokens,
    preserving the locale and word invariants."""
    skeleton = skeleton_of(contents)
    vclasses = numeric_value_classes(contents)
    row, col = cursor
    input_str = "%s\x1ecur=%d,%d\x1eV:%s" % (skeleton, row, col, ",".join(vclasses))
    return sig_of(input_str)


def content_fingerprint(contents, cursor):
    """The runner-local CONTENT FINGERPRINT over the FULL raw screen text plus the
    cursor cell. Byte-for-byte the same as crates/tui-sig::content_fingerprint:
    sig_of("{contents}\\x1ecur={row},{col}"). This is the Layer-1 effect token: it
    changes whenever ANY on-screen value changes, even when the skeleton signature
    is frozen (a counter ticking 0 -> 1 -> 2). It is EPHEMERAL and must NEVER enter
    the canonical state set; the SDK uses it only to decide whether an action did
    anything."""
    row, col = cursor
    input_str = "%s\x1ecur=%d,%d" % (contents, row, col)
    return sig_of(input_str)


def labels_of(contents):
    """Display-only word set (the `labels` field). Byte-for-byte the same as
    crates/tui-sig::labels_of: blank box/block glyphs, split on whitespace, strip
    surrounding non-alphanumerics, cap token width at 40 chars, dedup, sort, take
    MAX_LABELS. Human display only, never the signature."""
    cleaned = "".join(
        " " if "─" <= c <= "▟" else c for c in contents
    )
    found = set()
    for raw in cleaned.split():
        # strip surrounding non-alphanumerics (Rust trim_matches(!is_alphanumeric))
        start = 0
        end = len(raw)
        while start < end and not _is_alphanumeric(raw[start]):
            start += 1
        while end > start and not _is_alphanumeric(raw[end - 1]):
            end -= 1
        t = raw[start:end]
        if t != "" and len(t) <= 40:
            found.add(t)
    return sorted(found)[:MAX_LABELS]
