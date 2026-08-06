// Hermetic replay for reproit-backend-dotnet.
//
// When `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same boundary that
// records exchanges at capture time SERVES them instead, so the application re-executes
// against exactly what production saw with no live dependency at all.
//
// Determinism is a contract here, not a similarity score. Matching is strict per-operation
// ordinals: within one operation (method plus path for HTTP, statement text for pg) exchanges
// are consumed in recorded order, so pooled db clients and LLM tool-call loops that interleave
// operations still match exactly. Recorded `$reproit` redaction placeholders match any value
// at their position, and a body truncated at capture fails closed. The first unmatched call is
// a DIVERGENCE, reported as a structured `REPROIT:DIVERGENCE` stderr line, byte-identical to
// the Node SDK's (insertion-order compact JSON, Json.Compact), with a `bodyDelta` naming
// WHERE the bodies differ: chat-shaped bodies name the first differing message index, unknown
// shapes fall back to the byte offset of the first differing byte.
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

    // The capture's time zone, resolved from the envelope.
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

    // Pin the process time zone to the capture's, so replayed code reading
    // DateTime.Now or TimeZoneInfo.Local sees what production saw.
    //
    // On Unix .NET resolves TimeZoneInfo.Local from the TZ environment
    // variable, and ClearCachedData drops the cached Local so the next read
    // re-resolves it. Windows resolves the local zone from the registry and
    // ignores TZ, so there the zone cannot be pinned and PinnedTimeZone stays
    // the app-readable fallback. Returns true only when the zone was really
    // pinned, so no caller can mistake the fallback for a pin.
    public bool PinTimeZone()
    {
        if (_envelope.GetValueOrDefault("tz") is not string tz || tz.Length == 0) return false;
        if (OperatingSystem.IsWindows()) return false;
        try
        {
            Environment.SetEnvironmentVariable("TZ", tz);
            TimeZoneInfo.ClearCachedData();
            return TimeZoneInfo.Local.Id == tz;
        }
        catch (Exception)
        {
            return false;
        }
    }

    // The capture moment from the envelope, or null when the capture carries none.
    public long? ObservedAtMs() => _envelope.GetValueOrDefault("observedAtMs") switch
    {
        long value => value,
        int value => value,
        double value => (long)value,
        _ => null,
    };

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

    // Strict per-operation ordinal match. Within one operation (the key below) the next
    // unconsumed exchange is the ONLY candidate; skipping it silently would be a fuzzy match.
    // Other operations' exchanges may interleave (db pooling, tool-call loops), which is why
    // the key filters first. Null is a divergence, reported.
    internal Dictionary<string, object?>? Matched(
        string protocol, Dictionary<string, object?> probe)
    {
        var key = OperationKey(protocol, probe);
        lock (_lock)
        {
            foreach (var entry in _exchanges)
            {
                if (entry.Consumed) continue;
                if (entry.Exchange.GetValueOrDefault("protocol") as string != protocol) continue;
                var recorded =
                    entry.Exchange.GetValueOrDefault("request") as Dictionary<string, object?>
                    ?? new Dictionary<string, object?>();
                if (OperationKey(protocol, recorded) != key) continue;
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

    // One operation's identity for ordinal matching: HTTP is method plus path and query, pg
    // is the exact statement text.
    internal static string OperationKey(string protocol, Dictionary<string, object?> request) =>
        protocol == "http"
            ? (request.GetValueOrDefault("method") as string ?? string.Empty) + " " +
                PathAndQuery(request.GetValueOrDefault("url"))
            : request.GetValueOrDefault("text") as string ?? string.Empty;

    // Report a divergence on stderr in the shared structured shape. Field order mirrors the
    // Node reference so the marker line is byte-comparable across SDKs; compact insertion-
    // order separators for the same reason.
    internal void Diverge(string protocol, Dictionary<string, object?> probe)
    {
        var key = OperationKey(protocol, probe);
        Dictionary<string, object?>? expected = null;
        Dictionary<string, object?>? firstCandidate = null;
        long consumed = 0;
        long total;
        lock (_lock)
        {
            foreach (var entry in _exchanges)
            {
                if (entry.Consumed)
                {
                    consumed += 1;
                    continue;
                }
                if (entry.Exchange.GetValueOrDefault("protocol") as string != protocol)
                {
                    continue;
                }
                var request =
                    entry.Exchange.GetValueOrDefault("request") as Dictionary<string, object?>
                    ?? new Dictionary<string, object?>();
                firstCandidate ??= request;
                if (expected == null && OperationKey(protocol, request) == key)
                {
                    expected = request;
                }
            }
            total = _exchanges.Count;
        }
        expected ??= firstCandidate;
        var report = new Dictionary<string, object?>
        {
            ["protocol"] = protocol,
            ["got"] = probe,
            ["expected"] = expected,
            ["consumed"] = consumed,
            ["total"] = total,
        };
        // Prompt drift: when the recorded and live bodies both exist and differ, name WHERE.
        var delta = expected == null
            ? null
            : BodyDelta(
                expected.TryGetValue("body", out var recordedBody) ? recordedBody : Absent,
                probe.TryGetValue("body", out var liveBody) ? liveBody : Absent);
        if (delta != null) report["bodyDelta"] = delta;
        Console.Error.WriteLine(DivergenceMarker + Json.Compact(report));
    }

    // Distinct from null: an ABSENT body is "the request had no body key", which the delta
    // must not confuse with an explicit JSON null.
    internal static readonly object Absent = new();

    // The messages array of an OpenAI/Anthropic-shaped chat body, else null.
    private static List<object?>? ChatMessages(object? body) =>
        body is Dictionary<string, object?> map &&
        map.GetValueOrDefault("messages") is List<object?> messages ? messages : null;

    private static byte[] DeltaBytes(object? value) =>
        System.Text.Encoding.UTF8.GetBytes(value is string text ? text : Json.Compact(value));

    // Locate the first difference between a recorded request body and a live one, modulo
    // redaction placeholders. Null when there is nothing to report (either body missing, or
    // no difference the matcher would object to).
    internal static Dictionary<string, object?>? BodyDelta(object? recorded, object? live)
    {
        if (ReferenceEquals(recorded, Absent) || ReferenceEquals(live, Absent)) return null;
        if (Matches(recorded, live)) return null;
        var recordedMessages = ChatMessages(recorded);
        var liveMessages = ChatMessages(live);
        if (recordedMessages != null && liveMessages != null)
        {
            var bound = Math.Min(recordedMessages.Count, liveMessages.Count);
            int? index = null;
            for (var i = 0; i < bound; i++)
            {
                if (!Matches(recordedMessages[i], liveMessages[i]))
                {
                    index = i;
                    break;
                }
            }
            // All shared indexes match: the drift is a longer or shorter conversation, and
            // the first differing message is the first unshared one. If lengths also agree
            // the drift is outside `messages`; fall through to bytes.
            if (index == null && recordedMessages.Count != liveMessages.Count) index = bound;
            if (index != null)
            {
                return new Dictionary<string, object?>
                {
                    ["kind"] = "message",
                    ["firstDifferingMessage"] = (long)index.Value,
                    ["recordedMessages"] = (long)recordedMessages.Count,
                    ["liveMessages"] = (long)liveMessages.Count,
                };
            }
        }
        var recordedBytes = DeltaBytes(recorded);
        var liveBytes = DeltaBytes(live);
        var length = Math.Min(recordedBytes.Length, liveBytes.Length);
        long offset = length;
        for (var i = 0; i < length; i++)
        {
            if (recordedBytes[i] != liveBytes[i])
            {
                offset = i;
                break;
            }
        }
        return new Dictionary<string, object?> { ["kind"] = "byte", ["offset"] = offset };
    }

    // The served shape of one HTTP probe: what the instrumented handler synthesizes a
    // response from. `Chunks` is present only for a recorded stream shape.
    public sealed class Served
    {
        public required int Status { get; init; }
        public required Dictionary<string, object?> Headers { get; init; }
        public required string BodyText { get; init; }
        public List<byte[]>? Chunks { get; init; }
    }

    // Resolve a live HTTP probe against the session, entirely in process (no sockets). A
    // divergence, a body truncated at capture, and truncated stream boundaries all serve a
    // hard 599 so the application observes an attributable failure instead of a guess.
    public Served ServeHttp(Dictionary<string, object?> probe)
    {
        var recorded = Matched("http", probe);
        if (recorded == null) return Diverged599("diverged");
        var response = recorded.GetValueOrDefault("response") as Dictionary<string, object?>
            ?? new Dictionary<string, object?>();
        if (response.GetValueOrDefault("truncated") as bool? == true)
        {
            // The capture kept identity but not bytes; serving a guessed body would be a
            // silent lie. Fail closed with the named reason.
            var diverged = new Dictionary<string, object?>(probe) { ["truncated"] = true };
            Diverge("http", diverged);
            return Diverged599("truncated-exchange-body");
        }
        var headers = new Dictionary<string, object?>();
        if (response.GetValueOrDefault("headers") is Dictionary<string, object?> recordedHeaders)
        {
            foreach (var (name, value) in recordedHeaders)
            {
                var lower = name.ToLowerInvariant();
                if (lower is "content-length" or "transfer-encoding" or "content-encoding")
                {
                    continue;
                }
                headers[name] = value;
            }
        }
        var body = response.GetValueOrDefault("body");
        var bodyText = body switch
        {
            null => string.Empty,
            string text => text,
            // Insertion-order compact: byte-identical to the Node reference's
            // JSON.stringify of the same recorded body.
            _ => Json.Compact(body),
        };
        var status = response.GetValueOrDefault("status") switch
        {
            long value => (int)value,
            int value => value,
            double value => (int)value,
            _ => 200,
        };
        List<byte[]>? chunks = null;
        if (response.GetValueOrDefault("stream") is Dictionary<string, object?> stream &&
            stream.GetValueOrDefault("chunks") is List<object?> boundaries)
        {
            if (stream.GetValueOrDefault("truncated") as bool? == true)
            {
                // The capture kept the body but not every chunk boundary; serving a guessed
                // stream shape would be a silent lie. Fail closed, named.
                var diverged = new Dictionary<string, object?>(probe)
                {
                    ["streamBoundariesTruncated"] = true,
                };
                Diverge("http", diverged);
                return Diverged599("truncated-stream-boundaries");
            }
            chunks = SplitChunks(bodyText, boundaries);
        }
        return new Served
        {
            Status = status,
            Headers = headers,
            BodyText = bodyText,
            Chunks = chunks,
        };
    }

    // Split a replayed body at the recorded chunk boundaries (byte lengths). Redaction can
    // change body byte counts, so lengths are clamped and the last chunk absorbs any
    // remainder: the CHUNK COUNT (the stream shape the app observed) is preserved exactly,
    // the recorded content never padded.
    internal static List<byte[]> SplitChunks(string bodyText, List<object?> lengths)
    {
        var raw = System.Text.Encoding.UTF8.GetBytes(bodyText);
        var chunks = new List<byte[]>(lengths.Count);
        var offset = 0;
        for (var index = 0; index < lengths.Count; index++)
        {
            var last = index == lengths.Count - 1;
            var size = lengths[index] switch
            {
                long value when value > 0 => (int)value,
                int value when value > 0 => value,
                double value when value > 0 => (int)value,
                _ => 0,
            };
            var end = last ? raw.Length : Math.Min(offset + size, raw.Length);
            chunks.Add(raw[offset..end]);
            offset = end;
        }
        return chunks;
    }

    private static Served Diverged599(string reason) => new()
    {
        Status = 599,
        Headers = new Dictionary<string, object?> { ["content-type"] = "application/json" },
        BodyText = Json.Compact(new Dictionary<string, object?> { ["reproit"] = reason }),
    };

    // Parse a request body the way the capture recorded it: declared JSON parses (so the
    // matcher compares structure), everything else stays text.
    public static object? TryJson(string text, string? contentType)
    {
        if (contentType != null &&
            contentType.Contains("application/json", StringComparison.Ordinal))
        {
            try
            {
                return Json.Parse(text);
            }
            catch (Exception)
            {
                return text;
            }
        }
        return text;
    }

    // Method, path and query of the original URL, and body modulo placeholders. Recorded
    // headers are deliberately not matched: they carry per-run noise that would turn every
    // replay into a divergence.
    internal static bool HttpMatches(
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
    internal static bool DbMatches(
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
    public double NextDouble() => (NextUInt64() >> 11) / (double)(1UL << 53);

    // The next raw 64-bit word of the stream. NextDouble scales one of these, so the two
    // draws share one sequence; crypto-random byte draws pull whole words from it.
    internal ulong NextUInt64()
    {
        _state ^= _state << 13;
        _state ^= _state >> 7;
        _state ^= _state << 17;
        return unchecked(_state * 0x2545f4914f6cdd1dUL);
    }
}
