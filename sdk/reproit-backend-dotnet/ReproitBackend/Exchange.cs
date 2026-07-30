// Bounded dependency-exchange values for reproit-backend-dotnet.
//
// An exchange is the request the app sent to a dependency plus the response that dependency
// returned. It is what hermetic replay serves, so responses are recorded verbatim up to a
// fixed inline budget; an over-budget body keeps only its byte count and sha256 and is marked
// truncated, and replay fails closed on it with a named reason instead of guessing.
//
// Bounds are byte-identical to the Node, Rust, and Java SDKs so one replay engine consumes
// every backend capture.

using System.Security.Cryptography;
using System.Text;

namespace ReproitBackend;

public static class Exchange
{
    // Inline body budget per exchange side; beyond it only provable identity remains.
    public const int MaxExchangeBodyBytes = 8 * 1024;
    // Recorded headers are capped to keep events bounded.
    public const int MaxExchangeHeaders = 32;
    // Rows recorded per db result; beyond it the result is marked truncated.
    public const int MaxDbRows = 64;

    // Bound one exchange body: within budget it is recorded verbatim (JSON parsed when the
    // content type declares it), beyond it only byte count, sha256, and the truncated marker.
    internal static Dictionary<string, object?> BoundedBody(byte[]? body, string? contentType)
    {
        var fields = new Dictionary<string, object?>();
        if (body == null || body.Length == 0) return fields;
        if (body.Length > MaxExchangeBodyBytes)
        {
            fields["bodyBytes"] = (long)body.Length;
            fields["bodySha256"] = Sha256Hex(body);
            fields["truncated"] = true;
            return fields;
        }
        var text = Encoding.UTF8.GetString(body);
        if (contentType != null && contentType.Contains("application/json", StringComparison.Ordinal))
        {
            try
            {
                fields["body"] = Json.Parse(text);
                return fields;
            }
            catch (Exception)
            {
                // Declared JSON that does not parse is recorded as text below.
            }
        }
        fields["body"] = text;
        return fields;
    }

    // Lowercased header names, capped; absent when empty.
    internal static Dictionary<string, object?> BoundedHeaders(
        IEnumerable<KeyValuePair<string, string>>? headers)
    {
        var fields = new Dictionary<string, object?>();
        if (headers == null) return fields;
        var capped = new Dictionary<string, object?>();
        foreach (var (name, value) in headers)
        {
            if (capped.Count >= MaxExchangeHeaders) break;
            if (name == null || value == null) continue;
            capped[name.ToLowerInvariant()] = value;
        }
        if (capped.Count > 0) fields["headers"] = capped;
        return fields;
    }

    // The recorded shape of one HTTP exchange.
    internal static Dictionary<string, object?> Http(
        string method,
        string url,
        IEnumerable<KeyValuePair<string, string>>? requestHeaders,
        byte[]? requestBody,
        string? requestContentType,
        int status,
        IEnumerable<KeyValuePair<string, string>>? responseHeaders,
        byte[]? responseBody,
        string? responseContentType)
    {
        var request = new Dictionary<string, object?>
        {
            ["method"] = method,
            ["url"] = url,
        };
        foreach (var (key, value) in BoundedHeaders(requestHeaders)) request[key] = value;
        foreach (var (key, value) in BoundedBody(requestBody, requestContentType))
        {
            request[key] = value;
        }
        var response = new Dictionary<string, object?> { ["status"] = (long)status };
        foreach (var (key, value) in BoundedHeaders(responseHeaders)) response[key] = value;
        foreach (var (key, value) in BoundedBody(responseBody, responseContentType))
        {
            response[key] = value;
        }
        return new Dictionary<string, object?>
        {
            ["protocol"] = "http",
            ["request"] = request,
            ["response"] = response,
        };
    }

    // The recorded shape of one database exchange.
    internal static Dictionary<string, object?> Db(
        string? text, IReadOnlyList<object?>? values, Dictionary<string, object?> outcome)
    {
        var request = new Dictionary<string, object?> { ["text"] = text ?? string.Empty };
        if (values is { Count: > 0 }) request["values"] = values.ToList();
        return new Dictionary<string, object?>
        {
            ["protocol"] = "pg",
            ["request"] = request,
            ["response"] = outcome,
        };
    }

    // Rows beyond the cap are dropped and the outcome is marked truncated.
    internal static Dictionary<string, object?> DbOutcome(
        string? command, long rowCount, IReadOnlyList<object?>? rows)
    {
        var kept = rows ?? Array.Empty<object?>();
        var outcome = new Dictionary<string, object?>
        {
            ["command"] = command,
            ["rowCount"] = rowCount,
            ["rows"] = kept.Take(MaxDbRows).ToList(),
        };
        if (kept.Count > MaxDbRows) outcome["truncated"] = true;
        return outcome;
    }

    internal static Dictionary<string, object?> DbError(string message, string? code) =>
        new()
        {
            ["error"] = new Dictionary<string, object?>
            {
                ["message"] = message,
                ["code"] = code,
            },
        };

    // Effect kind for a SQL statement: reads stay reads so state oracles keep their meaning;
    // everything else is a write.
    internal static string DbEffectKind(string? text)
    {
        var verb = (text ?? string.Empty).TrimStart();
        verb = verb[..Math.Min(verb.Length, 8)].ToUpperInvariant();
        return verb.StartsWith("SELECT", StringComparison.Ordinal)
            || verb.StartsWith("SHOW", StringComparison.Ordinal)
            ? "read" : "write";
    }

    internal static string Sha256Hex(byte[] bytes) =>
        Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant();
}
