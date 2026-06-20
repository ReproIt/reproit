package com.reproit.android

/**
 * PII-safe input fingerprinting (tier-3 on-error context).
 *
 * Some bugs only reproduce with a specific INPUT property: a 312-char name, an
 * emoji, a Turkish dotless "i", an empty field, an RTL string. To reproduce
 * those without storing PII we capture DERIVED FEATURES of on-screen text-field
 * values at error time, never the values themselves; the cloud turns these into
 * a property-matched replay fixture.
 *
 * [fingerprintValue] is the load-bearing pure function: pure Kotlin (NO
 * `android.*` imports), host-testable, identical shape and rules across all five
 * SDKs. It returns FEATURES only and NEVER includes the raw string.
 */
object Fingerprint {

    /**
     * Derived, PII-safe features of a single text value:
     *   len: Unicode code-point count (so "José🎉" -> 5)
     *   charset: "numeric" (all ASCII digits) | "ascii" | "unicode"
     *   hasEmoji / isEmpty / isRtl: Boolean flags
     */
    fun fingerprintValue(value: String): Map<String, Any> {
        val codePoints = value.codePoints().toArray()
        val len = codePoints.size
        val isEmpty = value.trim().isEmpty()
        var hasUnicode = false
        var allDigits = !isEmpty
        for (c in codePoints) {
            if (c > 0x7f) hasUnicode = true
            if (c < 0x30 || c > 0x39) allDigits = false
        }
        val charset = if (hasUnicode) "unicode" else if (allDigits) "numeric" else "ascii"
        val out = LinkedHashMap<String, Any>()
        out["len"] = len
        out["charset"] = charset
        out["hasEmoji"] = hasEmoji(codePoints)
        out["isEmpty"] = isEmpty
        out["isRtl"] = isRtl(codePoints)
        return out
    }

    /** Any code point in a strong RTL Unicode block (Arabic / Hebrew / ...). */
    private fun isRtl(codePoints: IntArray): Boolean {
        for (c in codePoints) {
            if ((c in 0x0590..0x05ff) || // Hebrew
                (c in 0x0600..0x06ff) || // Arabic
                (c in 0x0700..0x074f) || // Syriac
                (c in 0x0780..0x07bf) || // Thaana
                (c in 0x07c0..0x07ff) || // N'Ko
                (c in 0x08a0..0x08ff) || // Arabic Extended-A
                (c in 0xfb1d..0xfb4f) || // Hebrew presentation forms
                (c in 0xfb50..0xfdff) || // Arabic presentation forms-A
                (c in 0xfe70..0xfeff)    // Arabic presentation forms-B
            ) {
                return true
            }
        }
        return false
    }

    /** Common emoji / pictographic blocks + regional indicators (flags). */
    private fun hasEmoji(codePoints: IntArray): Boolean {
        for (c in codePoints) {
            if ((c in 0x1f000..0x1faff) || // pictographs, emoji, symbols
                (c in 0x1f1e6..0x1f1ff) || // regional indicators (flags)
                (c in 0x2600..0x27bf) ||   // misc symbols + dingbats
                c == 0x2764 ||              // heavy black heart
                c == 0xfe0f ||             // variation selector-16
                c == 0x200d                // zero-width joiner
            ) {
                return true
            }
        }
        return false
    }

    /**
     * Fingerprint a list of (field, value) pairs, discarding each value. The
     * Android layer supplies labels + values; raw values never escape.
     */
    fun fingerprintFields(fields: List<Pair<String, String>>): List<Map<String, Any>> =
        fields.map { (field, value) ->
            val out = LinkedHashMap<String, Any>()
            out["field"] = field
            out.putAll(fingerprintValue(value))
            out
        }
}
