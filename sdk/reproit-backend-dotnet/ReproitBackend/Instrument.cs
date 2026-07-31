// Outbound-exchange capture and hermetic replay for reproit-backend-dotnet.
//
// .NET has no monkeypatching, so the boundary is explicit and OPT-IN: build the app's
// HttpClient with `Instrument.Handler()` and route database statements through
// `Instrument.Db.RunAsync`. Every dependency exchange (request AND response) is then recorded
// onto the ambient request trace, bounded and redacted at source. Anything not routed through
// the boundary is invisible to capture and unavailable at replay.
//
// With `REPROIT_REPLAY` naming a `reproit-backend-capture` payload the SAME boundary serves
// the recorded exchanges: no socket is opened and no database is contacted. An unmatched call
// emits the structured `REPROIT:DIVERGENCE` line and answers 599 (HTTP) or throws (db).
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
        }
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
                foreach (var (key, value) in Exchange.BoundedBody(requestBody, requestContentType))
                {
                    probe[key] = value;
                }
                var recorded = session.Matched("http", probe);
                if (recorded == null) return Diverged599("diverged");
                var response =
                    recorded.GetValueOrDefault("response") as Dictionary<string, object?>
                    ?? new Dictionary<string, object?>();
                if (response.GetValueOrDefault("truncated") as bool? == true)
                {
                    // The capture kept identity but not bytes; serving a guessed body would
                    // be a silent lie.
                    session.Diverge("http", probe);
                    return Diverged599("truncated-exchange-body");
                }
                return Served(response);
            }

            var live = await base.SendAsync(request, cancellationToken).ConfigureAwait(false);
            var trace = AmbientTrace();
            if (trace == null) return live;
            try
            {
                var responseBody =
                    await live.Content.ReadAsByteArrayAsync(cancellationToken)
                        .ConfigureAwait(false);
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
                        responseBody,
                        live.Content.Headers.ContentType?.ToString() ?? string.Empty),
                });
                // The body was consumed to record it; hand the app an equivalent response.
                var replacement = new HttpResponseMessage(live.StatusCode)
                {
                    Content = new ByteArrayContent(responseBody),
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
            catch (Exception)
            {
                // The trace may have finished or overflowed; the host call goes on.
                return live;
            }
        }

        private static IEnumerable<KeyValuePair<string, string>> HeaderPairs(
            System.Net.Http.Headers.HttpHeaders headers) =>
            headers.Select(header =>
                new KeyValuePair<string, string>(
                    header.Key, header.Value.FirstOrDefault() ?? string.Empty));

        private static HttpResponseMessage Served(Dictionary<string, object?> response)
        {
            var status = response.GetValueOrDefault("status") switch
            {
                long value => (int)value,
                int value => value,
                double value => (int)value,
                _ => 200,
            };
            var body = response.GetValueOrDefault("body") switch
            {
                null => Array.Empty<byte>(),
                string text => Encoding.UTF8.GetBytes(text),
                var other => Encoding.UTF8.GetBytes(Json.Canonical(other)),
            };
            var message = new HttpResponseMessage((HttpStatusCode)status)
            {
                Content = new ByteArrayContent(body),
            };
            if (response.GetValueOrDefault("headers") is Dictionary<string, object?> headers)
            {
                foreach (var (name, value) in headers)
                {
                    var lower = name.ToLowerInvariant();
                    if (lower is "content-length" or "transfer-encoding" or "content-encoding")
                    {
                        continue;
                    }
                    if (value == null) continue;
                    var text = value.ToString() ?? string.Empty;
                    if (!message.Headers.TryAddWithoutValidation(lower, text))
                    {
                        message.Content.Headers.TryAddWithoutValidation(lower, text);
                    }
                }
            }
            return message;
        }

        private static HttpResponseMessage Diverged599(string reason)
        {
            var body = Json.Canonical(new Dictionary<string, object?> { ["reproit"] = reason });
            var message = new HttpResponseMessage((HttpStatusCode)599)
            {
                Content = new StringContent(body, Encoding.UTF8, "application/json"),
            };
            return message;
        }
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
            }
            catch (Exception)
            {
                // The trace may have finished or overflowed; the host call goes on.
            }
        }
    }
}
