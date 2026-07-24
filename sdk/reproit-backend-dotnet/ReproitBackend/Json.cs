// Canonical JSON for the wire: compact, recursively sorted object keys, byte-identical to the
// Rust adapter's serde_json (BTreeMap) encoding of the same events (Node/Python/Go verified
// identical). Values use a plain object model: null, bool, long, double, string,
// IDictionary<string, object?> and IEnumerable<object?>. Parse() produces exactly that model.

using System.Globalization;
using System.Text;
using System.Text.Json;

namespace ReproitBackend;

public static class Json
{
    // Compact JSON with recursively sorted keys (ordinal order, matching JS sort() and
    // serde_json's BTreeMap for the ASCII keys the adapter emits).
    public static string Canonical(object? value)
    {
        var builder = new StringBuilder();
        WriteValue(builder, value);
        return builder.ToString();
    }

    public static byte[] CanonicalUtf8(object? value) => Encoding.UTF8.GetBytes(Canonical(value));

    // Unpadded base64url of the given bytes (the x-reproit-events header format).
    public static string Base64Url(byte[] bytes) =>
        Convert.ToBase64String(bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_');

    // Parse JSON text into the object model above. Throws JsonException on invalid input.
    public static object? Parse(string text)
    {
        using var document = JsonDocument.Parse(text);
        return FromElement(document.RootElement);
    }

    public static object? FromElement(JsonElement element)
    {
        switch (element.ValueKind)
        {
            case JsonValueKind.Object:
                var map = new Dictionary<string, object?>();
                foreach (var property in element.EnumerateObject())
                {
                    map[property.Name] = FromElement(property.Value);
                }
                return map;
            case JsonValueKind.Array:
                var list = new List<object?>();
                foreach (var item in element.EnumerateArray()) list.Add(FromElement(item));
                return list;
            case JsonValueKind.String:
                return element.GetString();
            case JsonValueKind.Number:
                // No ternary: it would unify long and double to double and box everything
                // as a double.
                if (element.TryGetInt64(out var integer)) return integer;
                return element.GetDouble();
            case JsonValueKind.True:
                return true;
            case JsonValueKind.False:
                return false;
            default:
                return null;
        }
    }

    private static void WriteValue(StringBuilder builder, object? value)
    {
        switch (value)
        {
            case null:
                builder.Append("null");
                return;
            case bool flag:
                builder.Append(flag ? "true" : "false");
                return;
            case string text:
                WriteString(builder, text);
                return;
            case int number:
                builder.Append(number.ToString(CultureInfo.InvariantCulture));
                return;
            case long number:
                builder.Append(number.ToString(CultureInfo.InvariantCulture));
                return;
            case double number:
                WriteDouble(builder, number);
                return;
            case IDictionary<string, object?> map:
                WriteObject(builder, map);
                return;
            case System.Collections.IEnumerable sequence:
                WriteArray(builder, sequence);
                return;
            default:
                throw new NotSupportedException(
                    "canonical json: unsupported value type " + value.GetType().Name);
        }
    }

    private static void WriteObject(StringBuilder builder, IDictionary<string, object?> map)
    {
        builder.Append('{');
        var first = true;
        foreach (var key in map.Keys.OrderBy(key => key, StringComparer.Ordinal))
        {
            if (!first) builder.Append(',');
            first = false;
            WriteString(builder, key);
            builder.Append(':');
            WriteValue(builder, map[key]);
        }
        builder.Append('}');
    }

    private static void WriteArray(StringBuilder builder, System.Collections.IEnumerable sequence)
    {
        builder.Append('[');
        var first = true;
        foreach (var item in sequence)
        {
            if (!first) builder.Append(',');
            first = false;
            WriteValue(builder, item);
        }
        builder.Append(']');
    }

    // serde_json / JSON.stringify escaping: two-char escapes for the usual controls, \u00xx
    // (lowercase hex) for the rest below 0x20, everything else raw (UTF-8 on the wire).
    private static void WriteString(StringBuilder builder, string text)
    {
        builder.Append('"');
        foreach (var ch in text)
        {
            switch (ch)
            {
                case '"': builder.Append("\\\""); break;
                case '\\': builder.Append("\\\\"); break;
                case '\b': builder.Append("\\b"); break;
                case '\t': builder.Append("\\t"); break;
                case '\n': builder.Append("\\n"); break;
                case '\f': builder.Append("\\f"); break;
                case '\r': builder.Append("\\r"); break;
                default:
                    if (ch < 0x20)
                    {
                        builder.Append("\\u00");
                        builder.Append("0123456789abcdef"[ch >> 4]);
                        builder.Append("0123456789abcdef"[ch & 0xf]);
                    }
                    else
                    {
                        builder.Append(ch);
                    }
                    break;
            }
        }
        builder.Append('"');
    }

    // JS number formatting (JSON.stringify): shortest round-trip digits, plain decimal for
    // point positions in (-6, 21], JS-style exponent notation outside. Non-finite is null.
    private static void WriteDouble(StringBuilder builder, double value)
    {
        if (double.IsNaN(value) || double.IsInfinity(value))
        {
            builder.Append("null");
            return;
        }
        var shortest = value.ToString("R", CultureInfo.InvariantCulture);
        var exponentAt = shortest.IndexOfAny(new[] { 'E', 'e' });
        if (exponentAt < 0)
        {
            builder.Append(shortest);
            return;
        }
        // Decompose <mantissa>E<exponent> into a digit string and the decimal point position.
        var mantissa = shortest[..exponentAt];
        var exponent = int.Parse(shortest[(exponentAt + 1)..], CultureInfo.InvariantCulture);
        var negative = mantissa.StartsWith('-');
        if (negative) mantissa = mantissa[1..];
        var pointAt = mantissa.Contains('.') ? mantissa.IndexOf('.') : mantissa.Length;
        var digits = mantissa.Replace(".", "").TrimEnd('0');
        if (digits.Length == 0) digits = "0";
        var pointExponent = exponent + pointAt;
        if (negative) builder.Append('-');
        if (pointExponent > 21 || pointExponent <= -6)
        {
            builder.Append(digits[0]);
            if (digits.Length > 1) builder.Append('.').Append(digits[1..]);
            builder.Append('e');
            var jsExponent = pointExponent - 1;
            builder.Append(jsExponent >= 0 ? "+" : "-");
            builder.Append(Math.Abs(jsExponent).ToString(CultureInfo.InvariantCulture));
        }
        else if (pointExponent >= digits.Length)
        {
            builder.Append(digits).Append(new string('0', pointExponent - digits.Length));
        }
        else if (pointExponent > 0)
        {
            builder.Append(digits[..pointExponent]).Append('.').Append(digits[pointExponent..]);
        }
        else
        {
            builder.Append("0.").Append(new string('0', -pointExponent)).Append(digits);
        }
    }
}
