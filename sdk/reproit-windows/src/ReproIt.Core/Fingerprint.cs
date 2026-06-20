// PII-safe input fingerprinting (tier-3 on-error context).
//
// Some bugs only reproduce with a specific INPUT property: a 312-char name, an
// emoji, a Turkish dotless "i", an empty field, an RTL string. To reproduce
// those without storing PII we capture DERIVED FEATURES of on-screen text-field
// values at error time, never the values themselves; the cloud turns these into
// a property-matched replay fixture.
//
// FingerprintValue is the load-bearing pure function: pure C# (no WPF / WinUI
// imports), host-testable, identical shape and rules across all SDKs. It returns
// FEATURES only and NEVER includes the raw string. Mirrors the Kotlin
// `Fingerprint.kt` and the iOS / RN / web fingerprinters.

using System.Collections.Generic;
using System.Linq;
using System.Text;

namespace ReproIt.Core
{
    public static class Fingerprint
    {
        /// <summary>Fingerprint schema version. Bumped to 2 for the byte/script/
        /// combining/zero-width/newline/edge-whitespace features; the cloud reads
        /// it to stay backward-compatible with v1 fingerprints. Stamp this as
        /// `fpVersion` into the emitted on-error context alongside the array.</summary>
        public const int FpVersion = 2;

        /// <summary>Derived, PII-safe features of a single text value:
        /// len: Unicode code-point count (so "Jose<emoji>" counts the emoji as one);
        /// charset: "numeric" (all ASCII digits) | "ascii" | "unicode";
        /// hasEmoji / isEmpty / isRtl: bool flags. The map is insertion-ordered to
        /// match the wire shape of the other SDKs.</summary>
        public static Dictionary<string, object> FingerprintValue(string value)
        {
            int[] codePoints = ToCodePoints(value ?? string.Empty);
            int len = codePoints.Length;
            bool isEmpty = (value ?? string.Empty).Trim().Length == 0;
            bool hasUnicode = false;
            bool allDigits = !isEmpty;
            foreach (int c in codePoints)
            {
                if (c > 0x7f)
                {
                    hasUnicode = true;
                }
                if (c < 0x30 || c > 0x39)
                {
                    allDigits = false;
                }
            }
            string charset = hasUnicode ? "unicode" : (allDigits ? "numeric" : "ascii");
            string s = value ?? string.Empty;
            bool hasNewline = codePoints.Any(c => c == 0x0a || c == 0x0d);
            bool edgeWs = s.Length > 0 && (IsWs(s[0]) || IsWs(s[s.Length - 1]));
            // Dictionary<string, object> preserves insertion order in practice on the
            // runtimes we target; the field order below mirrors the Kotlin LinkedHashMap.
            var outMap = new Dictionary<string, object>();
            outMap["len"] = len;
            outMap["bytes"] = Encoding.UTF8.GetByteCount(s);
            outMap["charset"] = charset;
            outMap["scripts"] = Scripts(codePoints);
            outMap["hasEmoji"] = HasEmoji(codePoints);
            outMap["isEmpty"] = isEmpty;
            outMap["isRtl"] = IsRtl(codePoints);
            outMap["hasCombiningMarks"] = HasCombining(codePoints);
            outMap["hasZeroWidth"] = HasZeroWidth(codePoints);
            outMap["hasNewline"] = hasNewline;
            outMap["leadingTrailingWhitespace"] = edgeWs;
            return outMap;
        }

        /// <summary>Fixed whitespace set for the edge-whitespace check (parity-safe,
        /// not a locale-dependent Trim).</summary>
        private static bool IsWs(char c)
        {
            return c == 0x09 || c == 0x0a || c == 0x0b || c == 0x0c || c == 0x0d || c == 0x20 || c == 0xa0;
        }

        /// <summary>Sorted unique Unicode SCRIPT buckets present. Mixed-script
        /// (e.g. ["Arabic","Latin"]) is what bidi bugs need, which isRtl can't
        /// express. Ranges are fixed and shared verbatim across all SDKs.</summary>
        private static List<string> Scripts(int[] codePoints)
        {
            var found = new SortedSet<string>(System.StringComparer.Ordinal);
            foreach (int c in codePoints)
            {
                if ((c >= 0x41 && c <= 0x5a) || (c >= 0x61 && c <= 0x7a) ||
                    (c >= 0xc0 && c <= 0x24f) || (c >= 0x1e00 && c <= 0x1eff)) found.Add("Latin");
                else if (c >= 0x370 && c <= 0x3ff) found.Add("Greek");
                else if (c >= 0x400 && c <= 0x4ff) found.Add("Cyrillic");
                else if (c >= 0x590 && c <= 0x5ff) found.Add("Hebrew");
                else if ((c >= 0x600 && c <= 0x6ff) || (c >= 0x750 && c <= 0x77f) ||
                         (c >= 0x8a0 && c <= 0x8ff)) found.Add("Arabic");
                else if (c >= 0x900 && c <= 0x97f) found.Add("Devanagari");
                else if (c >= 0xe00 && c <= 0xe7f) found.Add("Thai");
                else if ((c >= 0x3040 && c <= 0x30ff) || (c >= 0x3400 && c <= 0x9fff) ||
                         (c >= 0xac00 && c <= 0xd7a3) || (c >= 0xf900 && c <= 0xfaff)) found.Add("CJK");
            }
            return found.ToList();
        }

        /// <summary>Combining marks (decomposed accents): a normalization/layout breaker.</summary>
        private static bool HasCombining(int[] codePoints)
        {
            foreach (int c in codePoints)
            {
                if ((c >= 0x300 && c <= 0x36f) || (c >= 0x1ab0 && c <= 0x1aff) ||
                    (c >= 0x1dc0 && c <= 0x1dff) || (c >= 0x20d0 && c <= 0x20ff) ||
                    (c >= 0xfe20 && c <= 0xfe2f))
                {
                    return true;
                }
            }
            return false;
        }

        /// <summary>Zero-width / invisible code points (injection + normalization breakers).</summary>
        private static bool HasZeroWidth(int[] codePoints)
        {
            foreach (int c in codePoints)
            {
                if (c == 0x200b || c == 0x200c || c == 0x200d || c == 0x2060 || c == 0xfeff)
                {
                    return true;
                }
            }
            return false;
        }

        /// <summary>Fingerprint a list of (field, value) pairs, discarding each value.
        /// The platform layer supplies labels + values; raw values never escape.</summary>
        public static List<Dictionary<string, object>> FingerprintFields(IEnumerable<KeyValuePair<string, string>> fields)
        {
            var outList = new List<Dictionary<string, object>>();
            foreach (var f in fields)
            {
                var entry = new Dictionary<string, object>();
                entry["field"] = f.Key;
                foreach (var kv in FingerprintValue(f.Value))
                {
                    entry[kv.Key] = kv.Value;
                }
                outList.Add(entry);
            }
            return outList;
        }

        /// <summary>Expand a UTF-16 string into Unicode code points (surrogate pairs
        /// collapse to a single code point, so an astral emoji counts as len 1).</summary>
        private static int[] ToCodePoints(string s)
        {
            var cps = new List<int>(s.Length);
            for (int i = 0; i < s.Length; i++)
            {
                char c = s[i];
                if (char.IsHighSurrogate(c) && i + 1 < s.Length && char.IsLowSurrogate(s[i + 1]))
                {
                    cps.Add(char.ConvertToUtf32(c, s[i + 1]));
                    i++;
                }
                else
                {
                    cps.Add(c);
                }
            }
            return cps.ToArray();
        }

        /// <summary>Any code point in a strong RTL Unicode block (Arabic / Hebrew / ...).</summary>
        private static bool IsRtl(int[] codePoints)
        {
            foreach (int c in codePoints)
            {
                if ((c >= 0x0590 && c <= 0x05ff) || // Hebrew
                    (c >= 0x0600 && c <= 0x06ff) || // Arabic
                    (c >= 0x0700 && c <= 0x074f) || // Syriac
                    (c >= 0x0780 && c <= 0x07bf) || // Thaana
                    (c >= 0x07c0 && c <= 0x07ff) || // N'Ko
                    (c >= 0x08a0 && c <= 0x08ff) || // Arabic Extended-A
                    (c >= 0xfb1d && c <= 0xfb4f) || // Hebrew presentation forms
                    (c >= 0xfb50 && c <= 0xfdff) || // Arabic presentation forms-A
                    (c >= 0xfe70 && c <= 0xfeff))   // Arabic presentation forms-B
                {
                    return true;
                }
            }
            return false;
        }

        /// <summary>Common emoji / pictographic blocks + regional indicators (flags).</summary>
        private static bool HasEmoji(int[] codePoints)
        {
            foreach (int c in codePoints)
            {
                if ((c >= 0x1f000 && c <= 0x1faff) || // pictographs, emoji, symbols
                    (c >= 0x1f1e6 && c <= 0x1f1ff) || // regional indicators (flags)
                    (c >= 0x2600 && c <= 0x27bf) ||   // misc symbols + dingbats
                    c == 0x2764 ||                    // heavy black heart
                    c == 0xfe0f ||                    // variation selector-16
                    c == 0x200d)                      // zero-width joiner
                {
                    return true;
                }
            }
            return false;
        }
    }
}
