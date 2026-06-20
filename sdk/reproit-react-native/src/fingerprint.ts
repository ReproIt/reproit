/**
 * PII-safe input fingerprinting (tier-3 on-error context).
 *
 * Some bugs only reproduce with a specific INPUT property: a 312-char name, an
 * emoji, a Turkish dotless "i", an empty field, an RTL string. To reproduce
 * those without storing PII we capture DERIVED FEATURES of on-screen text-field
 * values at error time, never the values themselves. The reproit cloud turns
 * these features into a property-matched replay fixture.
 *
 * {@link fingerprintValue} is the load-bearing pure function: identical shape
 * and rules across all five SDKs (web/Flutter/RN/iOS/Android) and host-unit-
 * tested in each. It NEVER returns the raw string, only features.
 */

/**
 * Fingerprint schema version. Bumped to 2 for the byte/script/combining/
 * zero-width/newline/edge-whitespace features below; the cloud reads it to stay
 * backward-compatible with v1 fingerprints (len/charset/emoji/rtl/empty).
 */
export const FP_VERSION = 2;

/** Derived, PII-safe features of a single text value. */
export interface ValueFingerprint {
  /** Unicode code-point count (so "José🎉" -> 5, not UTF-16 length 6). */
  len: number;
  /** UTF-8 byte length (catches DB varchar byte-limit overflow that `len` misses). */
  bytes: number;
  /** "numeric" (all ASCII digits) | "ascii" (all < 0x80) | "unicode". */
  charset: 'ascii' | 'numeric' | 'unicode';
  /** Sorted unique Unicode script buckets present (mixed-script bidi). */
  scripts: string[];
  /** Contains an emoji / pictographic code point. */
  hasEmoji: boolean;
  /** Empty or whitespace-only. */
  isEmpty: boolean;
  /** Contains a right-to-left script character (Arabic / Hebrew / ...). */
  isRtl: boolean;
  /** Contains a combining mark (decomposed accent; normalization breaker). */
  hasCombiningMarks: boolean;
  /** Contains a zero-width / invisible code point (injection breaker). */
  hasZeroWidth: boolean;
  /** Contains a newline (LF 0x0A or CR 0x0D). */
  hasNewline: boolean;
  /** Length > 0 and first or last code unit is whitespace. */
  leadingTrailingWhitespace: boolean;
}

/** One on-screen field's fingerprint, keyed by a stable label or index. */
export interface FieldFingerprint extends ValueFingerprint {
  field: string;
}

/** Any char in a strong RTL Unicode block marks the string right-to-left. */
function isRtl(str: string): boolean {
  for (let i = 0; i < str.length; i++) {
    const c = str.charCodeAt(i);
    if (
      (c >= 0x0590 && c <= 0x05ff) || // Hebrew
      (c >= 0x0600 && c <= 0x06ff) || // Arabic
      (c >= 0x0700 && c <= 0x074f) || // Syriac
      (c >= 0x0780 && c <= 0x07bf) || // Thaana
      (c >= 0x07c0 && c <= 0x07ff) || // N'Ko
      (c >= 0x08a0 && c <= 0x08ff) || // Arabic Extended-A
      (c >= 0xfb1d && c <= 0xfb4f) || // Hebrew presentation forms
      (c >= 0xfb50 && c <= 0xfdff) || // Arabic presentation forms-A
      (c >= 0xfe70 && c <= 0xfeff) // Arabic presentation forms-B
    ) {
      return true;
    }
  }
  return false;
}

/** Scan code points for the common emoji / pictographic blocks + flags. */
function hasEmoji(str: string): boolean {
  for (let i = 0; i < str.length; i++) {
    const c = str.codePointAt(i)!;
    if (c > 0xffff) i++; // skip the low surrogate of an astral code point
    if (
      (c >= 0x1f000 && c <= 0x1faff) || // pictographs, emoji, symbols
      (c >= 0x1f1e6 && c <= 0x1f1ff) || // regional indicators (flags)
      (c >= 0x2600 && c <= 0x27bf) || // misc symbols + dingbats
      c === 0x2764 || // heavy black heart
      c === 0xfe0f || // variation selector-16 (emoji style)
      c === 0x200d // zero-width joiner (emoji sequences)
    ) {
      return true;
    }
  }
  return false;
}

/**
 * UTF-8 byte length, computed per code point so it's identical across SDKs
 * regardless of the host's native string encoding.
 */
function byteLen(str: string): number {
  let bytes = 0;
  for (let i = 0; i < str.length; i++) {
    const c = str.codePointAt(i)!;
    if (c > 0xffff) i++; // astral: skip the low surrogate
    if (c < 0x80) bytes += 1;
    else if (c < 0x800) bytes += 2;
    else if (c < 0x10000) bytes += 3;
    else bytes += 4;
  }
  return bytes;
}

/** Zero-width / invisible code points (injection + normalization breakers). */
function hasZeroWidth(str: string): boolean {
  for (let i = 0; i < str.length; i++) {
    const c = str.charCodeAt(i);
    if (c === 0x200b || c === 0x200c || c === 0x200d || c === 0x2060 || c === 0xfeff) {
      return true;
    }
  }
  return false;
}

/** Combining marks (a base char + combining accent; a normalization breaker). */
function hasCombining(str: string): boolean {
  for (let i = 0; i < str.length; i++) {
    const c = str.charCodeAt(i);
    if (
      (c >= 0x0300 && c <= 0x036f) ||
      (c >= 0x1ab0 && c <= 0x1aff) ||
      (c >= 0x1dc0 && c <= 0x1dff) ||
      (c >= 0x20d0 && c <= 0x20ff) ||
      (c >= 0xfe20 && c <= 0xfe2f)
    ) {
      return true;
    }
  }
  return false;
}

/**
 * The Unicode SCRIPTS present, as a sorted unique list of coarse bucket names.
 * Mixed-script (e.g. ["Arabic","Latin"]) is what bidi bugs need. Ranges are
 * fixed and shared verbatim across all SDKs.
 */
function scriptsOf(str: string): string[] {
  const found: Record<string, 1> = {};
  for (let i = 0; i < str.length; i++) {
    const c = str.charCodeAt(i);
    if (
      (c >= 0x41 && c <= 0x5a) ||
      (c >= 0x61 && c <= 0x7a) ||
      (c >= 0xc0 && c <= 0x24f) ||
      (c >= 0x1e00 && c <= 0x1eff)
    )
      found['Latin'] = 1;
    else if (c >= 0x370 && c <= 0x3ff) found['Greek'] = 1;
    else if (c >= 0x400 && c <= 0x4ff) found['Cyrillic'] = 1;
    else if (c >= 0x590 && c <= 0x5ff) found['Hebrew'] = 1;
    else if ((c >= 0x600 && c <= 0x6ff) || (c >= 0x750 && c <= 0x77f) || (c >= 0x8a0 && c <= 0x8ff))
      found['Arabic'] = 1;
    else if (c >= 0x900 && c <= 0x97f) found['Devanagari'] = 1;
    else if (c >= 0xe00 && c <= 0xe7f) found['Thai'] = 1;
    else if (
      (c >= 0x3040 && c <= 0x30ff) ||
      (c >= 0x3400 && c <= 0x9fff) ||
      (c >= 0xac00 && c <= 0xd7a3) ||
      (c >= 0xf900 && c <= 0xfaff)
    )
      found['CJK'] = 1;
  }
  return Object.keys(found).sort();
}

/**
 * Fingerprint a single value into PII-safe features. Captures FEATURES, never
 * the value. `null`/`undefined` are treated as an empty string.
 */
export function fingerprintValue(value: string | null | undefined): ValueFingerprint {
  const s = value == null ? '' : String(value);
  const len = Array.from(s).length; // code-point count
  const isEmpty = s.trim().length === 0;
  let hasUnicode = false;
  let allDigits = !isEmpty;
  let hasNewline = false;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c > 0x7f) hasUnicode = true;
    if (c < 0x30 || c > 0x39) allDigits = false;
    if (c === 0x0a || c === 0x0d) hasNewline = true;
  }
  const charset: ValueFingerprint['charset'] = hasUnicode
    ? 'unicode'
    : allDigits
      ? 'numeric'
      : 'ascii';
  // Edge whitespace: a fixed whitespace set (parity-safe, not locale trim).
  const isWs = (cc: number): boolean =>
    cc === 0x09 ||
    cc === 0x0a ||
    cc === 0x0b ||
    cc === 0x0c ||
    cc === 0x0d ||
    cc === 0x20 ||
    cc === 0xa0;
  const edgeWs =
    s.length > 0 && (isWs(s.charCodeAt(0)) || isWs(s.charCodeAt(s.length - 1)));
  return {
    len,
    bytes: byteLen(s),
    charset,
    scripts: scriptsOf(s),
    hasEmoji: hasEmoji(s),
    isEmpty,
    isRtl: isRtl(s),
    hasCombiningMarks: hasCombining(s),
    hasZeroWidth: hasZeroWidth(s),
    hasNewline,
    leadingTrailingWhitespace: edgeWs,
  };
}

/**
 * Fingerprint a list of {field, value} pairs, discarding each value. The caller
 * (the fiber snapshot) supplies the on-screen field labels + values; this never
 * retains a raw value. Returns one {field, ...features} per input.
 */
export function fingerprintFields(
  fields: ReadonlyArray<{ field: string; value: string | null | undefined }>
): FieldFingerprint[] {
  return fields.map((f) => ({ field: f.field, ...fingerprintValue(f.value) }));
}
