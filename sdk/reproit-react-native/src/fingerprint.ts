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

/** Derived, PII-safe features of a single text value. */
export interface ValueFingerprint {
  /** Unicode code-point count (so "José🎉" -> 5, not UTF-16 length 6). */
  len: number;
  /** "numeric" (all ASCII digits) | "ascii" (all < 0x80) | "unicode". */
  charset: 'ascii' | 'numeric' | 'unicode';
  /** Contains an emoji / pictographic code point. */
  hasEmoji: boolean;
  /** Empty or whitespace-only. */
  isEmpty: boolean;
  /** Contains a right-to-left script character (Arabic / Hebrew / ...). */
  isRtl: boolean;
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
 * Fingerprint a single value into PII-safe features. Captures FEATURES, never
 * the value. `null`/`undefined` are treated as an empty string.
 */
export function fingerprintValue(value: string | null | undefined): ValueFingerprint {
  const s = value == null ? '' : String(value);
  const len = Array.from(s).length; // code-point count
  const isEmpty = s.trim().length === 0;
  let hasUnicode = false;
  let allDigits = !isEmpty;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c > 0x7f) hasUnicode = true;
    if (c < 0x30 || c > 0x39) allDigits = false;
  }
  const charset: ValueFingerprint['charset'] = hasUnicode
    ? 'unicode'
    : allDigits
      ? 'numeric'
      : 'ascii';
  return { len, charset, hasEmoji: hasEmoji(s), isEmpty, isRtl: isRtl(s) };
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
