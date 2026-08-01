// Outbound-exchange capture and hermetic replay for reproit-backend-dotnet.
//
// .NET has no monkeypatching, so the boundary is explicit and OPT-IN: build the app's
// HttpClient with `Instrument.Handler()`, wrap the app's ADO.NET connection with `Ado.Wrap`
// (or route statements through `Instrument.Db.RunAsync`). Every dependency exchange (request
// AND response) is then recorded onto the ambient request trace, bounded and redacted at
// source. Anything not routed through the boundary is invisible to capture and unavailable at
// replay. Streaming responses (SSE / chunked) are recorded through a TEE stream as the app
// consumes them, chunk boundaries preserved; an abandoned body records nothing.
//
// With `REPROIT_REPLAY` naming a `reproit-backend-capture` payload the SAME boundary serves
// the recorded exchanges: no socket is opened and no database is contacted. An unmatched call
// emits the structured `REPROIT:DIVERGENCE` line and answers 599 (HTTP) or throws (db).
//
// Determinism sources the SDK can and cannot pin, named:
//   - `Instrument.RandomSource` is the SDK-exposed System.Random: the envelope-seeded
//     stream in replay mode, Random.Shared otherwise. `System.Security.Cryptography.
//     RandomNumberGenerator` is NOT pinnable (it reads the OS CSPRNG directly); apps
//     drawing security randomness replay with fresh entropy, and that is named here
//     rather than faked.
//   - `Instrument.Time` is the SDK-exposed TimeProvider: pinned to the capture's
//     `observedAtMs` in replay mode, TimeProvider.System otherwise. Direct DateTime.Now /
//     DateTime.UtcNow reads CANNOT be intercepted without profiler APIs, so only code
//     reading time through the exposed provider replays the capture's clock. The time
//     ZONE is pinned process-wide on Unix (Replay.PinTimeZone); Windows resolves the zone
//     from the registry and keeps the readable fallback.
//
// The ambient trace is an AsyncLocal, so it flows across awaits automatically; a call made
// outside a scope is simply not recorded, never half-recorded.

using System.Net;
using System.Text;

namespace ReproitBackend;

public static class Instrument
{
    private static readonly AsyncLocal<BackendTrace?> Ambient = new();
    private static readonly object SessionLock = new();
    private static bool _sessionResolved;
    private static Replay? _session;

    // Run `body` with `trace` ambient for the instrumented handler and Db.RunAsync. The
    // ASP.NET Core middleware scopes each request automatically.
    public static async Task<T> ScopeAsync<T>(BackendTrace trace, Func<Task<T>> body)
    {
        var previous = Ambient.Value;
        Ambient.Value = trace;
        try
        {
            return await body().ConfigureAwait(false);
        }
        finally
        {
            Ambient.Value = previous;
        }
    }

    public static async Task ScopeAsync(BackendTrace trace, Func<Task> body)
    {
        await ScopeAsync<object?>(trace, async () =>
        {
            await body().ConfigureAwait(false);
            return null;
        }).ConfigureAwait(false);
    }

    internal static BackendTrace? AmbientTrace()
    {
        var trace = Ambient.Value;
        return trace is { Finished: false } ? trace : null;
    }

    internal static void SetAmbient(BackendTrace? trace) => Ambient.Value = trace;

    // Load the replay session (when `REPROIT_REPLAY` is set). Idempotent; the first
    // instrumented call triggers it lazily, but calling it from Main resolves it up front.
    public static void Init() => Session();

    // True when this process serves a recorded capture instead of live calls.
    public static bool Replaying() => Session() != null;

    // The seeded replay stream, or null outside replay mode.
    public static ReplayRng? ReplayRng() => Session()?.Rng();

    // The capture's time zone, or null outside replay mode. On Unix the session
    // also PINS it at load, so this is the app-readable fallback for Windows,
    // where the local zone comes from the registry and TZ is ignored.
    public static TimeZoneInfo? ReplayTimeZone() => Session()?.PinnedTimeZone();

    internal static Replay? Session()
    {
        lock (SessionLock)
        {
            if (!_sessionResolved)
            {
                _sessionResolved = true;
                var path = Environment.GetEnvironmentVariable("REPROIT_REPLAY");
                if (!string.IsNullOrWhiteSpace(path))
                {
                    _session = Replay.Load(path);
                    // Pin the zone as early as the session resolves, before any
                    // zone-sensitive application code reads the clock.
                    _session?.PinTimeZone();
                }
            }
            return _session;
        }
    }

    // Tests drive the session directly rather than mutating the environment.
    internal static void ResetSessionForTest(Replay? replacement)
    {
        lock (SessionLock)
        {
            _sessionResolved = replacement != null;
            _session = replacement;
            _randomSource = null;
            _time = null;
        }
    }

    private static long _capturedExchanges;
    private static long _truncatedBodies;
    private static long _failedCaptures;
    private static Random? _randomSource;
    private static TimeProvider? _time;

    internal static void CountCapturedExchange() =>
        Interlocked.Increment(ref _capturedExchanges);

    internal static void CountTruncatedBody() => Interlocked.Increment(ref _truncatedBodies);

    internal static void CountFailedCapture() => Interlocked.Increment(ref _failedCaptures);

    // Capture-side health counters, the Node reference's `stats()`. A failed capture is a
    // per-trace drop (the trace finished or hit its event cap), never a host failure.
    public static Dictionary<string, long> Stats() => new()
    {
        ["capturedExchanges"] = Interlocked.Read(ref _capturedExchanges),
        ["truncatedBodies"] = Interlocked.Read(ref _truncatedBodies),
        ["failedCaptures"] = Interlocked.Read(ref _failedCaptures),
    };

    // The SDK-exposed randomness source: the envelope-seeded stream in replay mode,
    // Random.Shared otherwise. RandomNumberGenerator (the OS CSPRNG) is NOT pinnable; the
    // file header names it. Honesty note, same as every SDK: the seed makes REPLAY runs
    // deterministic; it does not reproduce the randomness the app drew in production.
    public static Random RandomSource
    {
        get
        {
            lock (SessionLock)
            {
                if (_randomSource == null)
                {
                    var rng = Session()?.Rng();
                    _randomSource = rng == null ? Random.Shared : new SeededRandom(rng);
                }
                return _randomSource;
            }
        }
    }

    // The SDK-exposed clock: pinned to the capture's observedAtMs in replay mode,
    // TimeProvider.System otherwise. Direct DateTime.Now reads cannot be intercepted
    // without profiler APIs; the file header names it.
    public static TimeProvider Time
    {
        get
        {
            lock (SessionLock)
            {
                if (_time == null)
                {
                    var observed = Session()?.ObservedAtMs();
                    _time = observed == null
                        ? TimeProvider.System
                        : new PinnedTimeProvider(
                            DateTimeOffset.FromUnixTimeMilliseconds(observed.Value)
                                - DateTimeOffset.UtcNow,
                            Session()?.PinnedTimeZone());
                }
                return _time;
            }
        }
    }

    // System.Random over the envelope-seeded xorshift64* stream. Derived Random routes
    // Next/NextDouble through the virtual Sample(), so one override pins them all.
    private sealed class SeededRandom : Random
    {
        private readonly ReplayRng _rng;

        internal SeededRandom(ReplayRng rng)
        {
            _rng = rng;
        }

        protected override double Sample() => _rng.NextDouble();

        public override double NextDouble() => Sample();
    }

    private sealed class PinnedTimeProvider : TimeProvider
    {
        private readonly TimeSpan _offset;
        private readonly TimeZoneInfo? _zone;

        internal PinnedTimeProvider(TimeSpan offset, TimeZoneInfo? zone)
        {
            _offset = offset;
            _zone = zone;
        }

        public override DateTimeOffset GetUtcNow() => base.GetUtcNow() + _offset;

        public override TimeZoneInfo LocalTimeZone => _zone ?? base.LocalTimeZone;
    }

    // The instrumented outbound HTTP boundary. Compose it into an HttpClient:
    //   new HttpClient(Instrument.Handler())
    public static DelegatingHandler Handler(HttpMessageHandler? inner = null) =>
        new ExchangeHandler(inner ?? new HttpClientHandler());

    private sealed class ExchangeHandler : DelegatingHandler
    {
        internal ExchangeHandler(HttpMessageHandler inner) : base(inner) {}

        protected override async Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var method = request.Method.Method;
            var url = request.RequestUri?.ToString() ?? string.Empty;
            byte[] requestBody = request.Content == null
                ? Array.Empty<byte>()
                : await request.Content.ReadAsByteArrayAsync(cancellationToken)
                    .ConfigureAwait(false);
            var requestContentType =
                request.Content?.Headers.ContentType?.ToString() ?? string.Empty;

            var session = Session();
            if (session != null)
            {
                var probe = new Dictionary<string, object?>
                {
                    ["method"] = method,
                    ["url"] = url,
                };
                if (requestBody.Length > 0)
                {
                    probe["body"] = Replay.TryJson(
                        Encoding.UTF8.GetString(requestBody), requestContentType);
                }
                return ServedResponse(session.ServeHttp(probe));
            }

            var live = await base.SendAsync(request, cancellationToken).ConfigureAwait(false);
            var trace = AmbientTrace();
            if (trace == null) return live;
            var contentType = live.Content.Headers.ContentType?.ToString() ?? string.Empty;
            var isEventStream = contentType.Contains("text/event-stream", StringComparison.Ordinal);
            // Streaming (SSE, and chunked responses generally): record through a TEE as the
            // app consumes the body, so the observed chunk boundaries are preserved and the
            // exchange lands at the moment the app sees the end of the body. An abandoned
            // body records nothing, exactly like the Node reference.
            if (isEventStream || live.Content.Headers.ContentLength == null)
            {
                try
                {
                    return await TeeResponse(
                        trace, request, live, requestBody, requestContentType,
                        contentType, isEventStream, cancellationToken).ConfigureAwait(false);
                }
                catch (Exception)
                {
                    CountFailedCapture();
                    return live;
                }
            }
            try
            {
                var responseBody =
                    await live.Content.ReadAsByteArrayAsync(cancellationToken)
                        .ConfigureAwait(false);
                Record(trace, request, live, requestBody, requestContentType,
                    Exchange.BoundedBody(responseBody, contentType), stream: null);
                // The body was consumed to record it; hand the app an equivalent response.
                return Replacement(live, new ByteArrayContent(responseBody));
            }
            catch (Exception)
            {
                // The trace may have finished or overflowed; the host call goes on.
                CountFailedCapture();
                return live;
            }
        }

        // Record one finished exchange onto the trace. Failure is counted, never surfaced.
        private static void Record(
            BackendTrace trace,
            HttpRequestMessage request,
            HttpResponseMessage live,
            byte[] requestBody,
            string requestContentType,
            Dictionary<string, object?> responseBodyFields,
            Dictionary<string, object?>? stream)
        {
            try
            {
                var method = request.Method.Method;
                var url = request.RequestUri?.ToString() ?? string.Empty;
                var responseHeaders = HeaderPairs(live.Headers)
                    .Concat(HeaderPairs(live.Content.Headers))
                    .ToList();
                trace.Effect("call", new EffectOptions
                {
                    Resource = request.RequestUri?.Host,
                    Key = method + " " + Replay.PathAndQuery(url),
                    Exchange = Exchange.Http(
                        method,
                        url,
                        HeaderPairs(request.Headers),
                        requestBody,
                        requestContentType,
                        (int)live.StatusCode,
                        responseHeaders,
                        responseBodyFields,
                        stream),
                });
                CountCapturedExchange();
            }
            catch (Exception)
            {
                CountFailedCapture();
            }
        }

        // Hand the app a live response whose body is a recording TEE: chunks are observed
        // as the app reads them, and the exchange records once at EOF.
        private async Task<HttpResponseMessage> TeeResponse(
            BackendTrace trace,
            HttpRequestMessage request,
            HttpResponseMessage live,
            byte[] requestBody,
            string requestContentType,
            string contentType,
            bool isEventStream,
            CancellationToken cancellationToken)
        {
            var upstream = await live.Content.ReadAsStreamAsync(cancellationToken)
                .ConfigureAwait(false);
            var collector = new BodyCollector();
            var tee = new RecordingStream(upstream, collector, () => Record(
                trace, request, live, requestBody, requestContentType,
                collector.ResultFields(contentType), collector.Stream(isEventStream)));
            return Replacement(live, new StreamContent(tee));
        }

        private static HttpResponseMessage Replacement(
            HttpResponseMessage live, HttpContent content)
        {
            var replacement = new HttpResponseMessage(live.StatusCode)
            {
                Content = content,
                ReasonPhrase = live.ReasonPhrase,
                RequestMessage = live.RequestMessage,
            };
            foreach (var (name, values) in live.Headers)
            {
                replacement.Headers.TryAddWithoutValidation(name, values);
            }
            foreach (var (name, values) in live.Content.Headers)
            {
                replacement.Content.Headers.TryAddWithoutValidation(name, values);
            }
            return replacement;
        }

        private static IEnumerable<KeyValuePair<string, string>> HeaderPairs(
            System.Net.Http.Headers.HttpHeaders headers) =>
            headers.Select(header =>
                new KeyValuePair<string, string>(
                    header.Key, header.Value.FirstOrDefault() ?? string.Empty));

        // Synthesize the HttpResponseMessage for a served (or diverged) probe. A recorded
        // stream shape is re-served chunk for chunk so a consumer reading the stream
        // observes the recorded boundaries.
        private static HttpResponseMessage ServedResponse(Replay.Served served)
        {
            var message = new HttpResponseMessage((HttpStatusCode)served.Status)
            {
                Content = served.Chunks != null
                    ? new StreamContent(new ChunkedReplayStream(served.Chunks))
                    : new ByteArrayContent(Encoding.UTF8.GetBytes(served.BodyText)),
            };
            foreach (var (name, value) in served.Headers)
            {
                if (value == null) continue;
                var lower = name.ToLowerInvariant();
                var text = value.ToString() ?? string.Empty;
                if (!message.Headers.TryAddWithoutValidation(lower, text))
                {
                    message.Content.Headers.TryAddWithoutValidation(lower, text);
                }
            }
            return message;
        }
    }

    // A read-only stream that records every byte the app consumes and fires `onEof` once at
    // end of stream. Abandoning the body (dispose before EOF) records nothing: the exchange
    // is only real once the app observed its end, exactly the Node tee semantics.
    private sealed class RecordingStream : Stream
    {
        private readonly Stream _upstream;
        private readonly BodyCollector _collector;
        private readonly Action _onEof;
        private bool _recorded;

        internal RecordingStream(Stream upstream, BodyCollector collector, Action onEof)
        {
            _upstream = upstream;
            _collector = collector;
            _onEof = onEof;
        }

        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => throw new NotSupportedException();
        public override long Position
        {
            get => throw new NotSupportedException();
            set => throw new NotSupportedException();
        }

        public override int Read(byte[] buffer, int offset, int count)
        {
            var read = _upstream.Read(buffer, offset, count);
            Observe(buffer.AsSpan(offset, read));
            return read;
        }

        public override async ValueTask<int> ReadAsync(
            Memory<byte> buffer, CancellationToken cancellationToken = default)
        {
            var read = await _upstream.ReadAsync(buffer, cancellationToken)
                .ConfigureAwait(false);
            Observe(buffer.Span[..read]);
            return read;
        }

        private void Observe(ReadOnlySpan<byte> chunk)
        {
            if (_recorded) return;
            try
            {
                if (chunk.Length > 0)
                {
                    _collector.Push(chunk);
                    return;
                }
                _recorded = true;
                _onEof();
            }
            catch (Exception)
            {
                CountFailedCapture();
            }
        }

        public override void Flush() {}
        public override long Seek(long offset, SeekOrigin origin) =>
            throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) =>
            throw new NotSupportedException();

        protected override void Dispose(bool disposing)
        {
            if (disposing) _upstream.Dispose();
            base.Dispose(disposing);
        }
    }

    // Serves a recorded stream shape chunk for chunk: each Read returns bytes from at most
    // ONE recorded chunk, so the consumer observes the recorded boundaries.
    private sealed class ChunkedReplayStream : Stream
    {
        private readonly List<byte[]> _chunks;
        private int _chunk;
        private int _offset;

        internal ChunkedReplayStream(List<byte[]> chunks)
        {
            _chunks = chunks;
        }

        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => throw new NotSupportedException();
        public override long Position
        {
            get => throw new NotSupportedException();
            set => throw new NotSupportedException();
        }

        public override int Read(byte[] buffer, int offset, int count)
        {
            while (_chunk < _chunks.Count && _offset >= _chunks[_chunk].Length)
            {
                _chunk += 1;
                _offset = 0;
            }
            if (_chunk >= _chunks.Count) return 0;
            var current = _chunks[_chunk];
            var take = Math.Min(count, current.Length - _offset);
            Array.Copy(current, _offset, buffer, offset, take);
            _offset += take;
            return take;
        }

        public override void Flush() {}
        public override long Seek(long offset, SeekOrigin origin) =>
            throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) =>
            throw new NotSupportedException();
    }

    // Database statements through the exchange boundary. .NET has no driver to monkeypatch,
    // so the app routes each statement through RunAsync; anything else is invisible to
    // capture and unavailable at replay.
    public static class Db
    {
        public sealed record Outcome(string? Command, long RowCount, IReadOnlyList<object?> Rows);

        public sealed class DbException : Exception
        {
            public string? Code { get; }

            public DbException(string message, string? code = null) : base(message)
            {
                Code = code;
            }
        }

        // Run one statement through the boundary: replay mode serves the recorded outcome
        // without calling `live`; capture mode awaits it and records the exchange either way
        // it settles.
        public static async Task<Outcome> RunAsync(
            string text, IReadOnlyList<object?>? values, Func<Task<Outcome>> live)
        {
            var session = Session();
            if (session != null)
            {
                var probe = new Dictionary<string, object?> { ["text"] = text };
                if (values is { Count: > 0 }) probe["values"] = values.ToList();
                var recorded = session.Matched("pg", probe);
                if (recorded == null)
                {
                    throw new DbException("reproit: db call diverged from the capture");
                }
                var response =
                    recorded.GetValueOrDefault("response") as Dictionary<string, object?>
                    ?? new Dictionary<string, object?>();
                if (response.GetValueOrDefault("error") is Dictionary<string, object?> error)
                {
                    throw new DbException(
                        error.GetValueOrDefault("message") as string ?? "recorded db error",
                        error.GetValueOrDefault("code") as string);
                }
                var rowCount = response.GetValueOrDefault("rowCount") switch
                {
                    long value => value,
                    int value => value,
                    double value => (long)value,
                    _ => 0L,
                };
                var rows = response.GetValueOrDefault("rows") as List<object?>
                    ?? new List<object?>();
                return new Outcome(
                    response.GetValueOrDefault("command") as string, rowCount, rows);
            }

            Outcome outcome;
            try
            {
                outcome = await live().ConfigureAwait(false);
            }
            catch (Exception failure)
            {
                Record(text, values, Exchange.DbError(
                    failure.Message, (failure as DbException)?.Code));
                throw;
            }
            Record(text, values,
                Exchange.DbOutcome(outcome.Command, outcome.RowCount, outcome.Rows));
            return outcome;
        }

        private static void Record(
            string text, IReadOnlyList<object?>? values, Dictionary<string, object?> outcome)
        {
            var trace = AmbientTrace();
            if (trace == null) return;
            try
            {
                trace.Effect(Exchange.DbEffectKind(text), new EffectOptions
                {
                    Resource = "pg",
                    Key = text[..Math.Min(text.Length, 256)],
                    Exchange = Exchange.Db(text, values, outcome),
                });
                CountCapturedExchange();
            }
            catch (Exception)
            {
                // The trace may have finished or overflowed; the host call goes on.
                CountFailedCapture();
            }
        }
    }
}
