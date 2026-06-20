// Minimal JSON encoder + decoder for the event payloads and the parity gate.
//
// Pure C# (no System.Text.Json / Newtonsoft), so the signature core stays a
// dependency-free netstandard2.0 library: the parity test reads
// signature_vectors.json with this decoder on any host, and the event encoder
// produces the exact byte shape the cloud expects, identical to the Kotlin
// `Json.kt` and the web SDK. Only the value types the event model uses are
// supported: string, int, long, double, bool, null, list (IEnumerable),
// and map (ordered dictionary).

using System;
using System.Collections;
using System.Collections.Generic;
using System.Globalization;
using System.Text;

namespace ReproIt.Core
{
    public static class Json
    {
        // ---- encode -------------------------------------------------------------

        /// <summary>Encode a value graph to a compact JSON string. Null map values
        /// are OMITTED (matching the optional "from"/"labels" fields and the Kotlin
        /// encoder), and integral doubles are emitted without a trailing ".0" so
        /// timestamps and counts read as plain integers across SDKs.</summary>
        public static string Encode(object value)
        {
            var sb = new StringBuilder();
            Write(sb, value);
            return sb.ToString();
        }

        private static void Write(StringBuilder sb, object value)
        {
            switch (value)
            {
                case null:
                    sb.Append("null");
                    break;
                case string s:
                    WriteString(sb, s);
                    break;
                case bool b:
                    sb.Append(b ? "true" : "false");
                    break;
                case int i:
                    sb.Append(i.ToString(CultureInfo.InvariantCulture));
                    break;
                case long l:
                    sb.Append(l.ToString(CultureInfo.InvariantCulture));
                    break;
                case double d:
                    // Emit integral doubles without a trailing ".0" so timestamps and
                    // counts read as plain integers, matching the other SDKs.
                    if (d == Math.Floor(d) && !double.IsInfinity(d))
                    {
                        sb.Append(((long)d).ToString(CultureInfo.InvariantCulture));
                    }
                    else
                    {
                        sb.Append(d.ToString("R", CultureInfo.InvariantCulture));
                    }
                    break;
                case IDictionary<string, object> map:
                    WriteMap(sb, map);
                    break;
                case IEnumerable seq:
                    WriteArray(sb, seq);
                    break;
                default:
                    WriteString(sb, value.ToString());
                    break;
            }
        }

        private static void WriteMap(StringBuilder sb, IDictionary<string, object> map)
        {
            sb.Append('{');
            bool first = true;
            foreach (var kv in map)
            {
                if (kv.Value == null)
                {
                    continue; // omit null fields (matches `from?`/`labels?`)
                }
                if (!first)
                {
                    sb.Append(',');
                }
                first = false;
                WriteString(sb, kv.Key);
                sb.Append(':');
                Write(sb, kv.Value);
            }
            sb.Append('}');
        }

        private static void WriteArray(StringBuilder sb, IEnumerable seq)
        {
            sb.Append('[');
            bool first = true;
            foreach (var v in seq)
            {
                if (!first)
                {
                    sb.Append(',');
                }
                first = false;
                Write(sb, v);
            }
            sb.Append(']');
        }

        private static void WriteString(StringBuilder sb, string s)
        {
            sb.Append('"');
            foreach (char c in s)
            {
                switch (c)
                {
                    case '"':
                        sb.Append("\\\"");
                        break;
                    case '\\':
                        sb.Append("\\\\");
                        break;
                    case '\n':
                        sb.Append("\\n");
                        break;
                    case '\r':
                        sb.Append("\\r");
                        break;
                    case '\t':
                        sb.Append("\\t");
                        break;
                    case '\b':
                        sb.Append("\\b");
                        break;
                    case '\f':
                        sb.Append("\\f");
                        break;
                    default:
                        if (c < ' ')
                        {
                            sb.Append("\\u");
                            sb.Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                        }
                        else
                        {
                            sb.Append(c);
                        }
                        break;
                }
            }
            sb.Append('"');
        }

        // ---- decode -------------------------------------------------------------

        /// <summary>Minimal recursive-descent JSON decoder. Returns the usual object
        /// graph: Dictionary&lt;string, object&gt; (insertion-ordered), List&lt;object&gt;,
        /// string, double, bool, or null. Sufficient for the golden-vector schema; not
        /// a full validator. Mirrors the Kotlin decoder used by that parity gate.</summary>
        public static object Decode(string text)
        {
            var p = new Parser(text);
            p.SkipWs();
            object v = p.ParseValue();
            p.SkipWs();
            if (!p.AtEnd())
            {
                throw new FormatException("trailing JSON at index " + p.Pos);
            }
            return v;
        }

        private sealed class Parser
        {
            private readonly string _s;
            public int Pos;

            public Parser(string s)
            {
                _s = s;
                Pos = 0;
            }

            public bool AtEnd()
            {
                return Pos >= _s.Length;
            }

            public void SkipWs()
            {
                while (Pos < _s.Length)
                {
                    char c = _s[Pos];
                    if (c == ' ' || c == '\t' || c == '\n' || c == '\r')
                    {
                        Pos++;
                    }
                    else
                    {
                        break;
                    }
                }
            }

            public object ParseValue()
            {
                SkipWs();
                if (Pos >= _s.Length)
                {
                    throw new FormatException("unexpected end of JSON");
                }
                char c = _s[Pos];
                switch (c)
                {
                    case '{':
                        return ParseObject();
                    case '[':
                        return ParseArray();
                    case '"':
                        return ParseString();
                    case 't':
                    case 'f':
                        return ParseBool();
                    case 'n':
                        return ParseNull();
                    default:
                        return ParseNumber();
                }
            }

            private Dictionary<string, object> ParseObject()
            {
                Expect('{');
                var outMap = new Dictionary<string, object>(StringComparer.Ordinal);
                SkipWs();
                if (Peek() == '}')
                {
                    Pos++;
                    return outMap;
                }
                while (true)
                {
                    SkipWs();
                    string key = ParseString();
                    SkipWs();
                    Expect(':');
                    object value = ParseValue();
                    outMap[key] = value;
                    SkipWs();
                    char c = Next();
                    if (c == ',')
                    {
                        continue;
                    }
                    if (c == '}')
                    {
                        break;
                    }
                    throw new FormatException("expected ',' or '}' but got '" + c + "' at " + (Pos - 1));
                }
                return outMap;
            }

            private List<object> ParseArray()
            {
                Expect('[');
                var outList = new List<object>();
                SkipWs();
                if (Peek() == ']')
                {
                    Pos++;
                    return outList;
                }
                while (true)
                {
                    outList.Add(ParseValue());
                    SkipWs();
                    char c = Next();
                    if (c == ',')
                    {
                        continue;
                    }
                    if (c == ']')
                    {
                        break;
                    }
                    throw new FormatException("expected ',' or ']' but got '" + c + "' at " + (Pos - 1));
                }
                return outList;
            }

            private string ParseString()
            {
                Expect('"');
                var sb = new StringBuilder();
                while (true)
                {
                    char c = Next();
                    if (c == '"')
                    {
                        break;
                    }
                    if (c == '\\')
                    {
                        char e = Next();
                        switch (e)
                        {
                            case '"':
                                sb.Append('"');
                                break;
                            case '\\':
                                sb.Append('\\');
                                break;
                            case '/':
                                sb.Append('/');
                                break;
                            case 'n':
                                sb.Append('\n');
                                break;
                            case 'r':
                                sb.Append('\r');
                                break;
                            case 't':
                                sb.Append('\t');
                                break;
                            case 'b':
                                sb.Append('\b');
                                break;
                            case 'f':
                                sb.Append('\f');
                                break;
                            case 'u':
                                string hex = _s.Substring(Pos, 4);
                                Pos += 4;
                                sb.Append((char)int.Parse(hex, NumberStyles.HexNumber, CultureInfo.InvariantCulture));
                                break;
                            default:
                                throw new FormatException("bad escape '\\" + e + "' at " + (Pos - 1));
                        }
                    }
                    else
                    {
                        sb.Append(c);
                    }
                }
                return sb.ToString();
            }

            private bool Matches(string literal)
            {
                if (Pos + literal.Length > _s.Length)
                {
                    return false;
                }
                for (int k = 0; k < literal.Length; k++)
                {
                    if (_s[Pos + k] != literal[k])
                    {
                        return false;
                    }
                }
                return true;
            }

            private bool ParseBool()
            {
                if (Matches("true"))
                {
                    Pos += 4;
                    return true;
                }
                if (Matches("false"))
                {
                    Pos += 5;
                    return false;
                }
                throw new FormatException("invalid literal at " + Pos);
            }

            private object ParseNull()
            {
                if (!Matches("null"))
                {
                    throw new FormatException("invalid literal at " + Pos);
                }
                Pos += 4;
                return null;
            }

            private double ParseNumber()
            {
                int start = Pos;
                while (Pos < _s.Length && "-+.eE0123456789".IndexOf(_s[Pos]) >= 0)
                {
                    Pos++;
                }
                return double.Parse(_s.Substring(start, Pos - start), CultureInfo.InvariantCulture);
            }

            private char Peek()
            {
                return _s[Pos];
            }

            private char Next()
            {
                return _s[Pos++];
            }

            private void Expect(char c)
            {
                if (Pos >= _s.Length || _s[Pos] != c)
                {
                    throw new FormatException("expected '" + c + "' at " + Pos);
                }
                Pos++;
            }
        }
    }
}
