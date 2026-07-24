// reproit-backend-dotnet, experimental backend trace adapter (v0.0.0)
//
// .NET port of sdk/reproit-backend-rs. Scan-time: services activate this adapter only when a
// trusted request carries `x-reproit-trace`. The resulting response header
// (`x-reproit-events`) contains bounded, trace-bound, structurally redacted events.
// Production: the optional, config-gated capture mode (Capture.cs) self-samples finished
// traces (always on 5xx / failure, optional healthy baseline) and posts them to Cloud ingest.
// It is not a public compatibility surface while backend contracts remain experimental.
//
// Wire parity with the Rust adapter: events serialize as compact JSON with recursively sorted
// keys (serde_json's BTreeMap order), and the header is unpadded base64url of that encoding.

using System.Security.Cryptography;
using System.Text;

namespace ReproitBackend;

public sealed class TraceException : Exception
{
    // InvalidOperation | AlreadyFinished | TooManyEvents | HeaderTooLarge
    public string Code { get; }

    public TraceException(string code) : base("reproit trace rejected input: " + code)
    {
        Code = code;
    }
}

// Parsed trusted trace headers (scan-time) or a synthesized capture-mode context.
public sealed class TraceContext
{
    public required string TraceId { get; init; }
    public string? Actor { get; init; }
    public long ActionIndex { get; init; }
    public string? Build { get; init; }
    public string? ConfigContract { get; init; }
}

public sealed class BeginOptions
{
    public string? SpanId { get; init; }
    public string? Tenant { get; init; }
    public string? IdempotencyKey { get; init; }
    public object? Input { get; init; }
    public IReadOnlyList<Dictionary<string, object?>>? Selections { get; init; }
}

public sealed class EffectOptions
{
    public string? Resource { get; init; }
    public string? Key { get; init; }
    public string? Tenant { get; init; }
    public string? Event { get; init; }
    public object? Detail { get; init; }
}

// Module-level helpers shared by the trace core, capture mode, and framework adapters.
public static class Reproit
{
    public const int MaxEvents = 256;
    public const int MaxHeaderBytes = 60000;

    public static readonly IReadOnlyList<string> EffectKinds =
        new[] { "read", "write", "delete", "emit", "call" };

    private static readonly string[] SecretParts =
    {
        "password", "passwd", "secret", "token", "authorization", "cookie", "email", "phone",
        "apikey", "publishablekey", "privatekey", "accesskey", "signingkey", "idempotencykey",
    };

    // `get(name)` returns the request header value (or null). Returns null when no valid
    // `x-reproit-trace` is present: the adapter stays inert.
    public static TraceContext? TraceContextFromHeaders(Func<string, string?> get)
    {
        var traceId = Bounded(get("x-reproit-trace"), 128);
        if (traceId == null) return null;
        long actionIndex = 0;
        var rawAction = get("x-reproit-action");
        if (rawAction != null && long.TryParse(rawAction.Trim(), out var parsed) &&
            parsed >= 0 && parsed <= 0xffffffffL)
        {
            actionIndex = parsed;
        }
        return new TraceContext
        {
            TraceId = traceId,
            Actor = Bounded(get("x-reproit-actor"), 32),
            ActionIndex = actionIndex,
            Build = Bounded(get("x-reproit-build"), 128),
            ConfigContract = Bounded(get("x-reproit-config-contract"), 128),
        };
    }

    // GraphQL selection mapping (parser-produced only). Returns null on an invalid path,
    // matching the Rust constructor.
    public static Dictionary<string, object?>? Selection(
        string schemaPath, string responsePath, string? typeCondition = null)
    {
        if (!ValidPath(schemaPath) || !ValidPath(responsePath)) return null;
        var value = new Dictionary<string, object?>
        {
            ["schemaPath"] = schemaPath,
            ["responsePath"] = responsePath,
        };
        if (typeCondition != null)
        {
            if (!ValidPath(typeCondition) || typeCondition.Contains('.') ||
                typeCondition.Contains("[]"))
            {
                return null;
            }
            value["typeCondition"] = typeCondition;
        }
        return value;
    }

    // Canonical decoded OpenAPI input. Framework adapters must provide decoded values
    // (including lists for repeated query/header parameters), never raw query strings whose
    // serialization style is ambiguous.
    public static Dictionary<string, object?> HttpInput(
        object? body = null,
        IDictionary<string, object?>? path = null,
        IDictionary<string, object?>? query = null,
        IDictionary<string, object?>? headers = null)
    {
        var value = new Dictionary<string, object?>();
        if (body != null) value["body"] = body;
        foreach (var (name, fields) in new[] { ("path", path), ("query", query),
            ("headers", headers) })
        {
            if (fields == null || fields.Count == 0) continue;
            var entries = new Dictionary<string, object?>();
            foreach (var (key, field) in fields)
            {
                entries[name == "headers" ? key.ToLowerInvariant() : key] = field;
            }
            value[name] = entries;
        }
        return value;
    }

    // Recursive structural redaction: secret-named fields are replaced with a `$reproit`
    // metadata stub (type + length), everything else recurses.
    public static object? Redact(object? value)
    {
        switch (value)
        {
            case IDictionary<string, object?> map:
                var redacted = new Dictionary<string, object?>();
                foreach (var (key, field) in map)
                {
                    redacted[key] = SecretField(key) ? Metadata(field) : Redact(field);
                }
                return redacted;
            case string:
                return value;
            case System.Collections.IEnumerable sequence:
                var list = new List<object?>();
                foreach (var item in sequence) list.Add(Redact(item));
                return list;
            default:
                return value;
        }
    }

    // Trimmed, non-empty, at most `maximum` code points; null otherwise.
    internal static string? Bounded(string? value, int maximum)
    {
        if (value == null) return null;
        var trimmed = value.Trim();
        if (trimmed.Length == 0 || CodePoints(trimmed) > maximum) return null;
        return trimmed;
    }

    // Hashed identity for idempotency keys: never ship the raw key.
    internal static string Identity(string value)
    {
        var digest = SHA256.HashData(Encoding.UTF8.GetBytes(value));
        return "sha256:" + Convert.ToHexString(digest, 0, 12).ToLowerInvariant();
    }

    internal static bool SecretField(string name)
    {
        var folded = new string(name.Where(char.IsAsciiLetterOrDigit).ToArray())
            .ToLowerInvariant();
        return SecretParts.Any(folded.Contains);
    }

    internal static int CodePoints(string value)
    {
        var count = 0;
        foreach (var _ in value.EnumerateRunes()) count++;
        return count;
    }

    // At most `maximum` code points, preserving surrogate pairs.
    internal static string TruncateCodePoints(string value, int maximum)
    {
        var count = 0;
        var index = 0;
        while (index < value.Length && count < maximum)
        {
            index += char.IsSurrogatePair(value, index) ? 2 : 1;
            count++;
        }
        return value[..index];
    }

    private static Dictionary<string, object?> Metadata(object? value)
    {
        var kind = "null";
        object? length = null;
        switch (value)
        {
            case bool: kind = "boolean"; break;
            case int or long: kind = "integer"; break;
            case double number:
                kind = number == Math.Floor(number) && !double.IsInfinity(number)
                    ? "integer" : "number";
                break;
            case string text:
                kind = "string";
                length = (long)CodePoints(text);
                break;
            case IDictionary<string, object?>: kind = "object"; break;
            case System.Collections.IEnumerable sequence:
                kind = "array";
                length = (long)sequence.Cast<object?>().Count();
                break;
        }
        return new Dictionary<string, object?>
        {
            ["$reproit"] = new Dictionary<string, object?>
            {
                ["redacted"] = true,
                ["type"] = kind,
                ["length"] = length,
            },
        };
    }

    private static bool ValidPath(string? path)
    {
        if (string.IsNullOrEmpty(path)) return false;
        foreach (var segment in path.Split('.'))
        {
            var name = segment.EndsWith("[]", StringComparison.Ordinal)
                ? segment[..^2] : segment;
            if (name.Length == 0) return false;
            if (!(char.IsAsciiLetter(name[0]) || name[0] == '_')) return false;
            if (name.Skip(1).Any(ch => !(char.IsAsciiLetterOrDigit(ch) || ch == '_')))
            {
                return false;
            }
        }
        return true;
    }
}

// One traced operation: a start event, observed effects, one return.
public sealed class BackendTrace
{
    private static long _sequence;

    private readonly Dictionary<string, object?> _common;
    private readonly List<Dictionary<string, object?>> _events = new();

    public bool Finished { get; private set; }

    private BackendTrace(Dictionary<string, object?> common)
    {
        _common = common;
    }

    public static BackendTrace Begin(
        TraceContext context, string operation, BeginOptions? options = null)
    {
        options ??= new BeginOptions();
        var name = Reproit.Bounded(operation, 256)
            ?? throw new TraceException("InvalidOperation");
        var spanId = Reproit.Bounded(options.SpanId ?? context.TraceId + ":" + name, 128)
            ?? throw new TraceException("InvalidOperation");
        var common = new Dictionary<string, object?>
        {
            ["traceId"] = context.TraceId,
            ["spanId"] = spanId,
            ["actionIndex"] = context.ActionIndex,
            ["operation"] = name,
        };
        if (!string.IsNullOrEmpty(context.Actor)) common["actor"] = context.Actor;
        if (!string.IsNullOrEmpty(context.Build)) common["build"] = context.Build;
        if (!string.IsNullOrEmpty(context.ConfigContract))
        {
            common["configContract"] = context.ConfigContract;
        }
        var tenant = options.Tenant == null ? null : Reproit.Bounded(options.Tenant, 128);
        if (tenant != null) common["tenant"] = tenant;
        if (options.IdempotencyKey != null)
        {
            common["idempotencyKey"] = Reproit.Identity(options.IdempotencyKey);
        }
        if (options.Selections is { Count: > 0 })
        {
            common["selections"] =
                options.Selections.Take(Reproit.MaxEvents).ToList<object?>();
        }
        var trace = new BackendTrace(common);
        trace.Push("start", new Dictionary<string, object?>
        {
            ["input"] = Reproit.Redact(options.Input),
        });
        return trace;
    }

    public void Effect(string kind, EffectOptions? options = null)
    {
        options ??= new EffectOptions();
        if (Finished) throw new TraceException("AlreadyFinished");
        if (!Reproit.EffectKinds.Contains(kind)) throw new TraceException("InvalidOperation");
        var fields = new Dictionary<string, object?> { ["effect"] = kind };
        foreach (var (name, value) in new[]
        {
            ("resource", options.Resource),
            ("key", options.Key),
            ("effectTenant", options.Tenant),
            ("event", options.Event),
        })
        {
            if (value != null) fields[name] = Reproit.TruncateCodePoints(value, 256);
        }
        if (options.Detail != null &&
            Reproit.Redact(options.Detail) is IDictionary<string, object?> detail)
        {
            foreach (var key in new[] { "before", "after", "payload" })
            {
                if (detail.TryGetValue(key, out var value)) fields[key] = value;
            }
        }
        Push("effect", fields);
    }

    public void Finish(object? output, int status, bool success, bool effectsComplete)
    {
        if (Finished) throw new TraceException("AlreadyFinished");
        Push("return", new Dictionary<string, object?>
        {
            ["output"] = Reproit.Redact(output),
            ["status"] = (long)status,
            ["success"] = success,
            ["effectsComplete"] = effectsComplete,
        });
        Finished = true;
    }

    public string Header()
    {
        if (!Finished) throw new TraceException("AlreadyFinished");
        var encoded = Json.Base64Url(Json.CanonicalUtf8(Events()));
        if (encoded.Length > Reproit.MaxHeaderBytes) throw new TraceException("HeaderTooLarge");
        return encoded;
    }

    public IReadOnlyList<Dictionary<string, object?>> Events() => _events;

    private void Push(string kind, Dictionary<string, object?> fields)
    {
        if (_events.Count >= Reproit.MaxEvents) throw new TraceException("TooManyEvents");
        var evt = new Dictionary<string, object?>(_common)
        {
            ["sequence"] = Interlocked.Increment(ref _sequence),
            ["kind"] = kind,
        };
        foreach (var (key, value) in fields) evt[key] = value;
        _events.Add(evt);
    }
}
