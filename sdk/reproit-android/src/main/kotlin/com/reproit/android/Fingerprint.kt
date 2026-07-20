package com.reproit.android

/**
 * PII-safe input fingerprinting (tier-3 on-error context).
 *
 * Some bugs only reproduce with a specific INPUT property: a 312-char name, an emoji, a Turkish
 * dotless "i", an empty field, an RTL string. To reproduce those without storing PII we capture
 * DERIVED FEATURES of on-screen text-field values at error time, never the values themselves; the
 * cloud turns these into a property-matched replay fixture.
 *
 * [fingerprintValue] is the load-bearing pure function: pure Kotlin (NO `android.*` imports),
 * host-testable, identical shape and rules across all five SDKs. It returns FEATURES only and NEVER
 * includes the raw string.
 */
object Fingerprint {

  /**
   * Fingerprint schema version, stamped into the on-error context alongside the fingerprint array.
   * Bump when the FEATURE set changes. Identical across SDKs.
   */
  const val FP_VERSION = 2

  /**
   * Derived, PII-safe features of a single text value: len: Unicode code-point count (so "José🎉"
   * -> 5) bytes: UTF-8 byte length charset: "numeric" (all ASCII digits) | "ascii" | "unicode"
   * scripts: sorted unique Unicode script buckets present (mixed-script bidi) hasEmoji / isEmpty /
   * isRtl: Boolean flags hasCombiningMarks / hasZeroWidth / hasNewline / leadingTrailingWhitespace
   */
  fun fingerprintValue(value: String): Map<String, Any> {
    val codePoints = codePoints(value)
    val len = codePoints.size
    val isEmpty = value.trim().isEmpty()
    var hasUnicode = false
    var allDigits = !isEmpty
    for (c in codePoints) {
      if (c > 0x7f) hasUnicode = true
      if (c < 0x30 || c > 0x39) allDigits = false
    }
    val charset = if (hasUnicode) "unicode" else if (allDigits) "numeric" else "ascii"
    // Edge whitespace: a fixed whitespace set (parity-safe, not locale trim).
    val edgeWs = value.isNotEmpty() && (isWs(value[0].code) || isWs(value[value.length - 1].code))
    val out = LinkedHashMap<String, Any>()
    out["len"] = len
    out["bytes"] = value.toByteArray(Charsets.UTF_8).size
    out["graphemes"] = graphemeCount(codePoints)
    out["charset"] = charset
    out["scripts"] = scripts(value)
    out["hasEmoji"] = hasEmoji(codePoints)
    out["isEmpty"] = isEmpty
    out["isRtl"] = isRtl(codePoints)
    out["hasCombiningMarks"] = hasCombining(value)
    out["hasZeroWidth"] = hasZeroWidth(value)
    out["hasNewline"] = hasNewline(value)
    out["leadingTrailingWhitespace"] = edgeWs
    return out
  }

  private fun codePoints(value: String): IntArray {
    val points = IntArray(value.codePointCount(0, value.length))
    var charOffset = 0
    var pointOffset = 0
    while (charOffset < value.length) {
      val point = Character.codePointAt(value, charOffset)
      points[pointOffset] = point
      pointOffset += 1
      charOffset += Character.charCount(point)
    }
    return points
  }

  /** Fixed whitespace set (parity-safe across SDKs, not locale-dependent). */
  private fun isWs(cc: Int): Boolean =
    cc == 0x09 || cc == 0x0a || cc == 0x0b || cc == 0x0c || cc == 0x0d || cc == 0x20 || cc == 0xa0

  /** Contains LF (0x0A) or CR (0x0D). */
  private fun hasNewline(str: String): Boolean {
    for (ch in str) {
      val c = ch.code
      if (c == 0x0a || c == 0x0d) return true
    }
    return false
  }

  /** Zero-width / invisible code points (injection + normalization breakers). */
  private fun hasZeroWidth(str: String): Boolean {
    for (ch in str) {
      val c = ch.code
      if (c == 0x200b || c == 0x200c || c == 0x200d || c == 0x2060 || c == 0xfeff) {
        return true
      }
    }
    return false
  }

  /**
   * Combining marks (a base char + combining accent renders differently than a precomposed one; a
   * classic normalization/layout breaker).
   */
  private fun hasCombining(str: String): Boolean {
    for (ch in str) {
      val c = ch.code
      if (
        (c in 0x0300..0x036f) ||
          (c in 0x1ab0..0x1aff) ||
          (c in 0x1dc0..0x1dff) ||
          (c in 0x20d0..0x20ff) ||
          (c in 0xfe20..0xfe2f)
      ) {
        return true
      }
    }
    return false
  }

  private fun isCombiningCp(c: Int): Boolean =
    (c in 0x0300..0x036f) ||
      (c in 0x1ab0..0x1aff) ||
      (c in 0x1dc0..0x1dff) ||
      (c in 0x20d0..0x20ff) ||
      (c in 0xfe20..0xfe2f)

  private fun graphemeCount(codePoints: IntArray): Int {
    var n = 0
    var joined = false
    for (c in codePoints) {
      if (c == 0x200d) {
        joined = true
        continue
      }
      if (isCombiningCp(c) || (c in 0xfe00..0xfe0f)) continue
      if (joined) {
        joined = false
        continue
      }
      n += 1
    }
    return n
  }

  /**
   * The Unicode SCRIPTS present, as a sorted unique list of coarse bucket names. Mixed-script (e.g.
   * ["Arabic","Latin"]) is what bidi bugs need, which `isRtl` alone can't express. Ranges are fixed
   * and shared verbatim across all SDKs.
   */
  private fun scripts(str: String): List<String> {
    val found = LinkedHashSet<String>()
    for (ch in str) {
      val c = ch.code
      if ((c in 0x41..0x5a) || (c in 0x61..0x7a) || (c in 0xc0..0x24f) || (c in 0x1e00..0x1eff))
        found.add("Latin")
      else if (c in 0x370..0x3ff) found.add("Greek")
      else if (c in 0x400..0x4ff) found.add("Cyrillic")
      else if (c in 0x590..0x5ff) found.add("Hebrew")
      else if ((c in 0x600..0x6ff) || (c in 0x750..0x77f) || (c in 0x8a0..0x8ff))
        found.add("Arabic")
      else if (c in 0x900..0x97f) found.add("Devanagari")
      else if (c in 0xe00..0xe7f) found.add("Thai")
      else if (
        (c in 0x3040..0x30ff) ||
          (c in 0x3400..0x9fff) ||
          (c in 0xac00..0xd7a3) ||
          (c in 0xf900..0xfaff)
      )
        found.add("CJK")
    }
    return found.sorted()
  }

  /** Any code point in a strong RTL Unicode block (Arabic / Hebrew / ...). */
  private fun isRtl(codePoints: IntArray): Boolean {
    for (c in codePoints) {
      if (
        (c in 0x0590..0x05ff) || // Hebrew
          (c in 0x0600..0x06ff) || // Arabic
          (c in 0x0700..0x074f) || // Syriac
          (c in 0x0780..0x07bf) || // Thaana
          (c in 0x07c0..0x07ff) || // N'Ko
          (c in 0x08a0..0x08ff) || // Arabic Extended-A
          (c in 0xfb1d..0xfb4f) || // Hebrew presentation forms
          (c in 0xfb50..0xfdff) || // Arabic presentation forms-A
          (c in 0xfe70..0xfeff) // Arabic presentation forms-B
      ) {
        return true
      }
    }
    return false
  }

  /** Common emoji / pictographic blocks + regional indicators (flags). */
  private fun hasEmoji(codePoints: IntArray): Boolean {
    for (c in codePoints) {
      if (
        (c in 0x1f000..0x1faff) || // pictographs, emoji, symbols
          (c in 0x1f1e6..0x1f1ff) || // regional indicators (flags)
          (c in 0x2600..0x27bf) || // misc symbols + dingbats
          c == 0x2764 || // heavy black heart
          c == 0xfe0f || // variation selector-16
          c == 0x200d // zero-width joiner
      ) {
        return true
      }
    }
    return false
  }

  /**
   * Fingerprint a list of (field, value) pairs, discarding each value. The Android layer supplies
   * labels + values; raw values never escape.
   */
  fun fingerprintFields(fields: List<Pair<String, String>>): List<Map<String, Any>> =
    fields.map { (field, value) ->
      val out = LinkedHashMap<String, Any>()
      out["field"] = field
      out.putAll(fingerprintValue(value))
      out
    }
}
