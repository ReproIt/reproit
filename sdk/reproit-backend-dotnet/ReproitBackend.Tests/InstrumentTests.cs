// Exchange capture and hermetic replay: bounds, redaction inside exchange bodies, the
// ambient-trace boundary, strict matching, and the structured divergence marker.

using System.Net;
using System.Text;
using ReproitBackend;
using Xunit;

namespace ReproitBackend.Tests;

// One collection with CapsuleParityTests: both mutate the process-global replay session and
// redirect Console.Error, which parallel classes would race on.
[Collection("replay-session")]
public class InstrumentTests : IDisposable
{
    private static readonly TraceContext CaptureContext = new()
    {
        TraceId = "cap-1",
        CaptureEnvelope = true,
    };

    public void Dispose() => Instrument.ResetSessionForTest(null);

    private static BackendTrace Trace() => BackendTrace.Begin(
        CaptureContext,
        "GET /quote",
        new BeginOptions
        {
            Input = new Dictionary<string, object?>
            {
                ["query"] = new Dictionary<string, object?> { ["symbol"] = "ACME" },
            },
        });

    private static Dictionary<string, object?>? ExchangeOf(BackendTrace trace) =>
        trace.Events()
            .Select(evt => evt.GetValueOrDefault("exchange") as Dictionary<string, object?>)
            .FirstOrDefault(exchange => exchange != null);

    // A canned inner handler so the tests need no live socket.
    private sealed class StubHandler : HttpMessageHandler
    {
        private readonly HttpStatusCode _status;
        private readonly string _body;

        internal StubHandler(HttpStatusCode status, string body)
        {
            _status = status;
            _body = body;
        }

        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken) =>
            Task.FromResult(new HttpResponseMessage(_status)
            {
                Content = new StringContent(_body, Encoding.UTF8, "application/json"),
            });
    }

    [Fact]
    public async Task HttpExchangesRecordRequestAndResponseOnTheAmbientTrace()
    {
        var client = new HttpClient(Instrument.Handler(
            new StubHandler(HttpStatusCode.OK, "{\"prices\":null,\"apiKey\":\"sk-live-secret\"}")));
        var trace = Trace();
        await Instrument.ScopeAsync(trace, async () =>
            await client.GetAsync("http://pricing.internal/prices?tier=gold"));
        var exchange = ExchangeOf(trace);
        Assert.NotNull(exchange);
        Assert.Equal("http", exchange!["protocol"]);
        var request = (Dictionary<string, object?>)exchange["request"]!;
        Assert.Equal("GET", request["method"]);
        var response = (Dictionary<string, object?>)exchange["response"]!;
        Assert.Equal(200L, response["status"]);
        var body = (Dictionary<string, object?>)response["body"]!;
        Assert.Null(body["prices"]);
        // Structural redaction applies INSIDE captured exchange bodies.
        var redacted = (Dictionary<string, object?>)
            ((Dictionary<string, object?>)body["apiKey"]!)["$reproit"]!;
        Assert.Equal(true, redacted["redacted"]);
    }

    [Fact]
    public async Task AnUnscopedCallIsNotRecordedRatherThanHalfRecorded()
    {
        var client = new HttpClient(Instrument.Handler(
            new StubHandler(HttpStatusCode.NoContent, "{}")));
        var trace = Trace();
        var response = await client.GetAsync("http://pricing.internal/ping");
        Assert.Equal(HttpStatusCode.NoContent, response.StatusCode);
        Assert.Null(ExchangeOf(trace));
    }

    [Fact]
    public void OversizedBodiesKeepProvableIdentityOnly()
    {
        var big = Encoding.UTF8.GetBytes(new string('x', Exchange.MaxExchangeBodyBytes + 1));
        var bounded = Exchange.BoundedBody(big, "text/plain");
        Assert.Equal(true, bounded["truncated"]);
        Assert.Equal((long)big.Length, bounded["bodyBytes"]);
        Assert.Equal(64, ((string)bounded["bodySha256"]!).Length);
        Assert.False(bounded.ContainsKey("body"));
    }

    [Fact]
    public void HeadersAreCappedAndLowercased()
    {
        var headers = Enumerable.Range(0, Exchange.MaxExchangeHeaders + 5)
            .Select(index => new KeyValuePair<string, string>(
                "X-Header-" + index, "value-" + index));
        var bounded = (Dictionary<string, object?>)
            Exchange.BoundedHeaders(headers)["headers"]!;
        Assert.Equal(Exchange.MaxExchangeHeaders, bounded.Count);
        Assert.True(bounded.ContainsKey("x-header-0"));
    }

    [Fact]
    public async Task DbStatementsRecordRowsAndReadsStayReads()
    {
        var trace = Trace();
        await Instrument.ScopeAsync(trace, async () => await Instrument.Db.RunAsync(
            "SELECT id FROM issuers WHERE symbol = $1",
            new object?[] { "ACME" },
            () => Task.FromResult(new Instrument.Db.Outcome(
                "SELECT", 1, new object?[]
                {
                    new Dictionary<string, object?> { ["id"] = 7L },
                }))));
        var exchange = ExchangeOf(trace);
        Assert.NotNull(exchange);
        Assert.Equal("pg", exchange!["protocol"]);
        var response = (Dictionary<string, object?>)exchange["response"]!;
        Assert.Equal(1L, response["rowCount"]);
        var recorded = trace.Events().First(evt => evt.ContainsKey("exchange"));
        Assert.Equal("read", recorded["effect"]);
    }

    [Fact]
    public async Task ReplayServesRecordedExchangesWithoutTouchingTheNetwork()
    {
        Instrument.ResetSessionForTest(Replay.Load(WriteCapture()));
        Assert.True(Instrument.Replaying());
        // The inner handler throws: any real send would fail the test.
        var client = new HttpClient(Instrument.Handler(new ThrowingHandler()));
        var response = await client.GetAsync("http://pricing.internal/prices?tier=gold");
        Assert.Equal(HttpStatusCode.OK, response.StatusCode);
        var body = await response.Content.ReadAsStringAsync();
        Assert.Contains("\"prices\"", body);
        var outcome = await Instrument.Db.RunAsync(
            "SELECT id FROM issuers WHERE symbol = $1",
            new object?[] { "ACME" },
            () => throw new InvalidOperationException("replay must never reach the statement"));
        Assert.Equal(1, outcome.RowCount);
    }

    private sealed class ThrowingHandler : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken) =>
            throw new InvalidOperationException("replay must never open a socket");
    }

    [Fact]
    public async Task AnUnmatchedCallDivergesWithTheStructuredMarker()
    {
        Instrument.ResetSessionForTest(Replay.Load(WriteCapture()));
        var original = Console.Error;
        var captured = new StringWriter();
        Console.SetError(captured);
        HttpResponseMessage response;
        try
        {
            var client = new HttpClient(Instrument.Handler(new ThrowingHandler()));
            response = await client.GetAsync("http://pricing.internal/unknown");
        }
        finally
        {
            Console.SetError(original);
        }
        Assert.Equal(599, (int)response.StatusCode);
        var marker = captured.ToString();
        Assert.StartsWith(Replay.DivergenceMarker, marker);
        var report = (Dictionary<string, object?>)Json.Parse(
            marker[Replay.DivergenceMarker.Length..].Trim())!;
        Assert.Equal("http", report["protocol"]);
        Assert.Equal(2L, report["total"]);
    }

    [Fact]
    public async Task ADivergedDbCallThrowsRatherThanGuessing()
    {
        Instrument.ResetSessionForTest(Replay.Load(WriteCapture()));
        var original = Console.Error;
        Console.SetError(new StringWriter());
        try
        {
            await Assert.ThrowsAsync<Instrument.Db.DbException>(() => Instrument.Db.RunAsync(
                "SELECT something else",
                null,
                () => Task.FromResult(new Instrument.Db.Outcome(null, 0, Array.Empty<object?>()))));
        }
        finally
        {
            Console.SetError(original);
        }
    }

    [Fact]
    public void RedactionPlaceholdersMatchAnyLiveValue()
    {
        var recorded = new Dictionary<string, object?>
        {
            ["password"] = new Dictionary<string, object?>
            {
                ["$reproit"] = new Dictionary<string, object?> { ["redacted"] = true },
            },
        };
        Assert.True(Replay.Matches(recorded, new Dictionary<string, object?>
        {
            ["password"] = "anything-at-all",
        }));
        Assert.False(Replay.Matches(
            new Dictionary<string, object?> { ["item"] = "widget" },
            new Dictionary<string, object?> { ["item"] = "gadget" }));
    }

    [Fact]
    public void TheEnvelopeSeedYieldsAStableStream()
    {
        var session = Replay.Load(WriteCapture());
        Assert.NotNull(session);
        var first = session!.Rng();
        var second = session.Rng();
        Assert.NotNull(first);
        var left = first!.NextDouble();
        var right = second!.NextDouble();
        Assert.Equal(left, right);
        Assert.InRange(left, 0, 1);
    }

    // A version-2 capture carrying one pg and one http exchange.
    [Fact]
    public void ReplayPinsTheProcessTimeZoneOnUnix()
    {
        // The zone is not merely exposed: on Unix .NET resolves
        // TimeZoneInfo.Local from TZ, so the session pins it and replayed code
        // reading DateTime.Now sees what production saw. Windows resolves the
        // zone from the registry and ignores TZ, so it keeps the fallback.
        var originalTz = Environment.GetEnvironmentVariable("TZ");
        try
        {
            var path = WriteCapture();
            var replay = Replay.Load(path);
            Assert.NotNull(replay);
            var pinned = replay!.PinTimeZone();
            if (OperatingSystem.IsWindows())
            {
                Assert.False(pinned);
            }
            else
            {
                Assert.True(pinned);
                Assert.Equal("Europe/Berlin", TimeZoneInfo.Local.Id);
                Assert.Equal(TimeSpan.FromHours(2), DateTimeOffset.Now.Offset);
            }
            File.Delete(path);
        }
        finally
        {
            Environment.SetEnvironmentVariable("TZ", originalTz);
            TimeZoneInfo.ClearCachedData();
        }
    }

    [Fact]
    public void ThePinnedZoneStaysReadableForAppsThatNeedItExplicitly()
    {
        // The exposed-zone contract is what Windows falls back to; pin it with
        // a test so it cannot silently regress.
        var path = WriteCapture();
        var replay = Replay.Load(path);
        Assert.NotNull(replay);
        Assert.Equal("Europe/Berlin", replay!.PinnedTimeZone()?.Id);
        File.Delete(path);
    }

    private static string WriteCapture()
    {
        const string payload = """
            {
              "format": "reproit-backend-capture",
              "version": 2,
              "operation": "GET /quote",
              "oracle": "backend-server-error",
              "envelope": {
                "observedAtMs": 1753747200000,
                "tz": "Europe/Berlin",
                "replaySeed": "00ff00ff00ff00ff"
              },
              "events": [
                {"traceId":"cap-r-1","spanId":"s","actionIndex":0,"operation":"GET /quote",
                 "sequence":1,"kind":"start","input":{}},
                {"traceId":"cap-r-1","spanId":"s","actionIndex":0,"operation":"GET /quote",
                 "sequence":2,"kind":"effect","effect":"read","resource":"pg",
                 "exchange":{"protocol":"pg",
                   "request":{"text":"SELECT id FROM issuers WHERE symbol = $1",
                              "values":["ACME"]},
                   "response":{"command":"SELECT","rowCount":1,"rows":[{"id":7}]}}},
                {"traceId":"cap-r-1","spanId":"s","actionIndex":0,"operation":"GET /quote",
                 "sequence":3,"kind":"effect","effect":"call","resource":"pricing",
                 "exchange":{"protocol":"http",
                   "request":{"method":"GET","url":"http://pricing.internal/prices?tier=gold"},
                   "response":{"status":200,"headers":{"content-type":"application/json"},
                               "body":{"prices":[1,2]}}}},
                {"traceId":"cap-r-1","spanId":"s","actionIndex":0,"operation":"GET /quote",
                 "sequence":4,"kind":"return","output":{},"status":500,"success":false,
                 "effectsComplete":true}
              ]
            }
            """;
        var path = Path.Combine(Path.GetTempPath(), "reproit-dotnet-replay-" +
            Guid.NewGuid().ToString("n") + ".json");
        File.WriteAllText(path, payload);
        return path;
    }
}
