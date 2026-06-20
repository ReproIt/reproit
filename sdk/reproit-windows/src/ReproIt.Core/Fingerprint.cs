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

namespace ReproIt.Core
{
    public static class Fingerprint
    {
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
            // Dictionary<string, object> preserves insertion order in practice on the
            // runtimes we target; the field order below mirrors the Kotlin LinkedHashMap.
            var outMap = new Dictionary<string, object>();
            outMap["len"] = len;
            outMap["charset"] = charset;
            outMap["hasEmoji"] = HasEmoji(codePoints);
            outMap["isEmpty"] = isEmpty;
            outMap["isRtl"] = IsRtl(codePoints);
            return outMap;
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
