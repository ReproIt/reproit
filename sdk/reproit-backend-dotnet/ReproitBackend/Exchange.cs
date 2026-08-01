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
    // Stream chunk boundaries recorded per exchange (SSE / chunked responses, the LLM
    // streaming shape). Beyond it the boundaries are marked truncated and replay fails
    // closed rather than serve a wrong stream shape.
    public const int MaxStreamChunks = 128;

    // Bound one exchange body: within budget it is recorded verbatim (JSON parsed when the
    // content type declares it), beyond it only byte count, sha256, and the truncated marker.
    internal static Dictionary<string, object?> BoundedBody(byte[]? body, string? contentType)
    {
        var fields = new Dictionary<string, object?>();
        if (body == null || body.Length == 0) return fields;
        if (body.Length > MaxExchangeBodyBytes)
        {
            Instrument.CountTruncatedBody();
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

    // Lowercased header names, capped over NAME SORTED order; absent when empty. Sorting
    // before the cap is the contract: a table iterated in arrival order records a different
    // subset per run, so two runs of one request disagree and the capsule stops matching.
    internal static Dictionary<string, object?> BoundedHeaders(
        IEnumerable<KeyValuePair<string, string>>? headers)
    {
        var fields = new Dictionary<string, object?>();
        if (headers == null) return fields;
        var sorted = new SortedDictionary<string, string>(StringComparer.Ordinal);
        foreach (var (name, value) in headers)
        {
            if (name == null || value == null) continue;
            sorted[name.ToLowerInvariant()] = value;
        }
        var capped = new Dictionary<string, object?>();
        foreach (var (name, value) in sorted)
        {
            if (capped.Count >= MaxExchangeHeaders) break;
            capped[name] = value;
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
        string? responseContentType) =>
        Http(
            method, url, requestHeaders, requestBody, requestContentType,
            status, responseHeaders,
            BoundedBody(responseBody, responseContentType), stream: null);

    // The recorded shape of one HTTP exchange whose response body fields were already
    // bounded (a streamed response collected by BodyCollector). `stream` carries the
    // observed chunk boundaries; a truncated inline body already fails closed at replay,
    // so boundaries are only kept for bodies recorded verbatim, exactly like the Node
    // reference.
    internal static Dictionary<string, object?> Http(
        string method,
        string url,
        IEnumerable<KeyValuePair<string, string>>? requestHeaders,
        byte[]? requestBody,
        string? requestContentType,
        int status,
        IEnumerable<KeyValuePair<string, string>>? responseHeaders,
        Dictionary<string, object?> responseBodyFields,
        Dictionary<string, object?>? stream)
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
        foreach (var (key, value) in responseBodyFields) response[key] = value;
        if (stream != null &&
            responseBodyFields.GetValueOrDefault("truncated") as bool? != true)
        {
            response["stream"] = stream;
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

// Collect a stream's chunks up to one byte past the inline budget; enough to know the true
// size class without holding unbounded memory. The sha256 runs over EVERY byte so truncated
// identity stays provable. Chunk boundaries are recorded as observed byte lengths, bounded
// by MaxStreamChunks; boundaries past the cap are counted, never guessed.
//
// .NET port of the Node reference's bodyCollector (instrument.js).
internal sealed class BodyCollector
{
    private readonly List<byte[]> _chunks = new();
    private readonly List<object?> _boundaries = new();
    private readonly IncrementalHash _hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
    private long _bytes;
    private int _droppedBoundaries;

    public void Push(ReadOnlySpan<byte> chunk)
    {
        _bytes += chunk.Length;
        _hash.AppendData(chunk);
        if (_boundaries.Count < Exchange.MaxStreamChunks) _boundaries.Add((long)chunk.Length);
        else _droppedBoundaries += 1;
        if (_bytes <= Exchange.MaxExchangeBodyBytes) _chunks.Add(chunk.ToArray());
    }

    // The collected body as bounded exchange fields: empty when nothing arrived, provable
    // identity when over budget, the verbatim body otherwise (JSON parsed when declared).
    public Dictionary<string, object?> ResultFields(string? contentType)
    {
        if (_bytes == 0) return new Dictionary<string, object?>();
        if (_bytes > Exchange.MaxExchangeBodyBytes)
        {
            Instrument.CountTruncatedBody();
            return new Dictionary<string, object?>
            {
                ["bodyBytes"] = _bytes,
                ["bodySha256"] = Convert.ToHexString(_hash.GetCurrentHash()).ToLowerInvariant(),
                ["truncated"] = true,
            };
        }
        var whole = new byte[_bytes];
        var offset = 0;
        foreach (var chunk in _chunks)
        {
            chunk.CopyTo(whole, offset);
            offset += chunk.Length;
        }
        return Exchange.BoundedBody(whole, contentType);
    }

    // Chunk boundaries as observed byte lengths. Recorded when the response is a stream
    // (SSE always; anything else only when it actually arrived in more than one chunk,
    // since a single-chunk body replays identically without them). Boundaries past the cap
    // are counted, never guessed.
    public Dictionary<string, object?>? Stream(bool isEventStream)
    {
        if (_boundaries.Count == 0) return null;
        if (!isEventStream && _boundaries.Count < 2 && _droppedBoundaries == 0) return null;
        var stream = new Dictionary<string, object?>
        {
            ["chunks"] = new List<object?>(_boundaries),
        };
        if (_droppedBoundaries > 0) stream["truncated"] = true;
        return stream;
    }
}
