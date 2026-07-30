// Hermetic replay for reproit-backend-dotnet.
//
// When `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same boundary that
// records exchanges at capture time SERVES them instead, so the application re-executes
// against exactly what production saw with no live dependency at all.
//
// Determinism is a contract here, not a similarity score. Matching is strict: the first
// unconsumed exchange of the protocol is the only candidate, recorded `$reproit` redaction
// placeholders match any value at their position, and a body truncated at capture fails
// closed. The first unmatched call is a DIVERGENCE, reported as a structured
// `REPROIT:DIVERGENCE` stderr line, byte-identical to the Node SDK's.
//
// The envelope pins the replay: the time zone comes from the capture and Rng() yields the
// seeded stream. Honesty note: the seed makes REPLAY runs deterministic; it does not
// reproduce the randomness the app drew in production.

using System.Globalization;

namespace ReproitBackend;

public sealed class Replay
{
    // The structured divergence marker, byte-identical to the Node SDK's.
    public const string DivergenceMarker = "REPROIT:DIVERGENCE ";

    private sealed class Entry
    {
        public required Dictionary<string, object?> Exchange { get; init; }
        public bool Consumed { get; set; }
    }

    private readonly Dictionary<string, object?> _envelope;
    private readonly List<Entry> _exchanges = new();
    private readonly object _lock = new();

    private Replay(Dictionary<string, object?> envelope, List<Dictionary<string, object?>> found)
    {
        _envelope = envelope;
        foreach (var exchange in found) _exchanges.Add(new Entry { Exchange = exchange });
    }

    // Load the capture named by `path`, or null when it is not a supported
    // `reproit-backend-capture` payload. Never throws: replay mode simply does not engage.
    public static Replay? Load(string path)
    {
        try
        {
            if (Json.Parse(File.ReadAllText(path)) is not Dictionary<string, object?> payload)
            {
                return null;
            }
            if (payload.GetValueOrDefault("format") as string != "reproit-backend-capture")
            {
                return null;
            }
            var version = payload.GetValueOrDefault("version") switch
            {
                long value => value,
                int value => value,
                double value => (long)value,
                _ => 0L,
            };
            if (version is < 1 or > 2) return null;
            var found = new List<Dictionary<string, object?>>();
            if (payload.GetValueOrDefault("events") is List<object?> events)
            {
                foreach (var item in events)
                {
                    if (item is not Dictionary<string, object?> evt) continue;
                    if (evt.GetValueOrDefault("kind") as string != "effect") continue;
                    if (evt.GetValueOrDefault("exchange") is Dictionary<string, object?> exchange)
                    {
                        found.Add(exchange);
                    }
                }
            }
            var envelope = payload.GetValueOrDefault("envelope") as Dictionary<string, object?>
                ?? new Dictionary<string, object?>();
            return new Replay(envelope, found);
        }
        catch (Exception)
        {
            return null;
        }
    }

    // Pin process determinism from the capture envelope. .NET has no settable process time
    // zone, so the pinned zone is exposed for the app to use rather than silently ignored.
    public TimeZoneInfo? PinnedTimeZone()
    {
        if (_envelope.GetValueOrDefault("tz") is not string tz || tz.Length == 0) return null;
        try
        {
            return TimeZoneInfo.FindSystemTimeZoneById(tz);
        }
        catch (Exception)
        {
            return null;
        }
    }

    // Deterministic xorshift64* stream from the capture's `replaySeed`, or null when the
    // capture carries no envelope seed.
    public ReplayRng? Rng()
    {
        if (_envelope.GetValueOrDefault("replaySeed") is not string seed || seed.Length == 0)
        {
            return null;
        }
        var hex = seed.Length > 16 ? seed[..16] : seed;
        return ulong.TryParse(hex, NumberStyles.HexNumber, CultureInfo.InvariantCulture,
            out var state) ? new ReplayRng(state | 1UL) : null;
    }

    // Strict next-unconsumed match. The first unconsumed exchange of the protocol is the ONLY
    // candidate; skipping it silently would be a fuzzy match. Null is a divergence, reported.
    internal Dictionary<string, object?>? Matched(
        string protocol, Dictionary<string, object?> probe)
    {
        lock (_lock)
        {
            foreach (var entry in _exchanges)
            {
                if (entry.Consumed) continue;
                if (entry.Exchange.GetValueOrDefault("protocol") as string != protocol) continue;
                var recorded =
                    entry.Exchange.GetValueOrDefault("request") as Dictionary<string, object?>
                    ?? new Dictionary<string, object?>();
                var hit = protocol == "http"
                    ? HttpMatches(recorded, probe)
                    : DbMatches(recorded, probe);
                if (hit)
                {
                    entry.Consumed = true;
                    return entry.Exchange;
                }
                break;
            }
        }
        Diverge(protocol, probe);
        return null;
    }

    // Report a divergence on stderr in the shared structured shape.
    internal void Diverge(string protocol, Dictionary<string, object?> probe)
    {
        object? expected = null;
        long consumed = 0;
        long total;
        lock (_lock)
        {
            foreach (var entry in _exchanges)
            {
                if (entry.Consumed)
                {
                    consumed += 1;
                }
                else if (expected == null &&
                    entry.Exchange.GetValueOrDefault("protocol") as string == protocol)
                {
                    expected = entry.Exchange.GetValueOrDefault("request");
                }
            }
            total = _exchanges.Count;
        }
        var report = new Dictionary<string, object?>
        {
            ["protocol"] = protocol,
            ["got"] = probe,
            ["expected"] = expected,
            ["consumed"] = consumed,
            ["total"] = total,
        };
        Console.Error.WriteLine(DivergenceMarker + Json.Canonical(report));
    }

    // Method, path and query of the original URL, and body modulo placeholders. Recorded
    // headers are deliberately not matched: they carry per-run noise that would turn every
    // replay into a divergence.
    private static bool HttpMatches(
        Dictionary<string, object?> recorded, Dictionary<string, object?> probe)
    {
        if (!Equals(recorded.GetValueOrDefault("method"), probe.GetValueOrDefault("method")))
        {
            return false;
        }
        if (PathAndQuery(recorded.GetValueOrDefault("url")) !=
            PathAndQuery(probe.GetValueOrDefault("url")))
        {
            return false;
        }
        return !recorded.ContainsKey("body")
            || Matches(recorded.GetValueOrDefault("body"), probe.GetValueOrDefault("body"));
    }

    // Exact statement text, values modulo placeholders.
    private static bool DbMatches(
        Dictionary<string, object?> recorded, Dictionary<string, object?> probe)
    {
        if (!Equals(recorded.GetValueOrDefault("text"), probe.GetValueOrDefault("text")))
        {
            return false;
        }
        return !recorded.ContainsKey("values")
            || Matches(recorded.GetValueOrDefault("values"), probe.GetValueOrDefault("values"));
    }

    internal static string PathAndQuery(object? url)
    {
        if (url is not string text) return string.Empty;
        return Uri.TryCreate(text, UriKind.Absolute, out var parsed)
            ? parsed.PathAndQuery : text;
    }

    // A recorded value matches a live one when equal, or when the recorded side is a
    // `$reproit` redaction placeholder (any value stood here at capture). Objects compare per
    // key; a recorded null matches anything.
    internal static bool Matches(object? recorded, object? live)
    {
        switch (recorded)
        {
            case null:
                return true;
            case Dictionary<string, object?> map:
                if (map.ContainsKey("$reproit")) return true;
                if (live is not Dictionary<string, object?> liveMap) return false;
                return map.All(entry =>
                    Matches(entry.Value, liveMap.GetValueOrDefault(entry.Key)));
            case List<object?> list:
                if (live is not List<object?> liveList || liveList.Count != list.Count)
                {
                    return false;
                }
                return !list.Where((item, index) => !Matches(item, liveList[index])).Any();
            default:
                if (recorded is IConvertible && live is IConvertible
                    && recorded is not string && live is not string)
                {
                    return Convert.ToDouble(recorded, CultureInfo.InvariantCulture)
                        .Equals(Convert.ToDouble(live, CultureInfo.InvariantCulture));
                }
                return recorded.Equals(live);
        }
    }
}

// The seeded replay stream; matches the Node SDK's draw shape.
public sealed class ReplayRng
{
    private ulong _state;

    internal ReplayRng(ulong state)
    {
        _state = state;
    }

    // The next draw in [0, 1).
    public double NextDouble()
    {
        _state ^= _state << 13;
        _state ^= _state >> 7;
        _state ^= _state << 17;
        var mixed = unchecked(_state * 0x2545f4914f6cdd1dUL);
        return (mixed >> 11) / (double)(1UL << 53);
    }
}
