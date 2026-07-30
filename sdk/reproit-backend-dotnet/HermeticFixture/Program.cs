// Money-test fixture: an ASP.NET Core minimal API whose /quote operation 500s because an
// upstream pricing service returns {"prices": null} and the handler indexes into it.
//
// MODE=capture: boots the upstream and the app, fires the failing request, and writes a
// version-2 reproit-backend-capture (exchanges + envelope) to CAPTURE_OUT. Default (server)
// mode boots ONLY the app on $PORT; with REPROIT_REPLAY set the SDK serves the recorded
// exchanges, so no upstream and no database exist. FIXED=1 applies the fix.

using System.Text;
using ReproitBackend;

const int UpstreamPort = 19983;
var upstreamUrl = $"http://127.0.0.1:{UpstreamPort}/prices?tier=gold";
var capturing = Environment.GetEnvironmentVariable("MODE") == "capture";
var fixedBuild = Environment.GetEnvironmentVariable("FIXED") == "1";
Instrument.Init();

// The upstream dependency exists only while capturing; replay serves it from the recording.
WebApplication? upstream = null;
if (capturing)
{
    var upstreamBuilder = WebApplication.CreateBuilder();
    upstreamBuilder.WebHost.UseUrls($"http://127.0.0.1:{UpstreamPort}");
    upstreamBuilder.Logging.ClearProviders();
    upstream = upstreamBuilder.Build();
    upstream.MapGet("/prices", () => Results.Json(new Dictionary<string, object?>
    {
        ["prices"] = null,
    }));
    await upstream.StartAsync();
}

var traces = new List<BackendTrace>();
var client = new HttpClient(Instrument.Handler());

var builder = WebApplication.CreateBuilder(args);
var port = capturing
    ? 19982
    : int.Parse(Environment.GetEnvironmentVariable("PORT") ?? "19982");
builder.WebHost.UseUrls($"http://127.0.0.1:{port}");
builder.Logging.ClearProviders();
var app = builder.Build();

// The trace boundary, hand-rolled here so the fixture needs no cloud endpoint: it scopes the
// handler exactly as the shipped ASP.NET Core middleware does.
app.Use(async (context, next) =>
{
    var trace = BackendTrace.Begin(
        new TraceContext
        {
            TraceId = "cap-dotnet-1",
            Build = "money-fixture",
            CaptureEnvelope = true,
        },
        context.Request.Method + " " + context.Request.Path,
        new BeginOptions
        {
            Input = Reproit.HttpInput(
                null,
                null,
                context.Request.Query.ToDictionary(
                    entry => entry.Key, entry => (object?)entry.Value.ToString()),
                null),
        });
    traces.Add(trace);
    await Instrument.ScopeAsync(trace, async () => await next(context));
    if (!trace.Finished)
    {
        trace.Finish(null, context.Response.StatusCode, context.Response.StatusCode < 500, true);
    }
});

app.MapGet("/quote", async (HttpContext context) =>
{
    try
    {
        await Instrument.Db.RunAsync(
            "SELECT id, symbol FROM issuers WHERE symbol = $1",
            new object?[] { context.Request.Query["symbol"].ToString() },
            () =>
            {
                if (!capturing)
                {
                    throw new InvalidOperationException(
                        "live database reached during hermetic replay");
                }
                return Task.FromResult(new Instrument.Db.Outcome("SELECT", 1, new object?[]
                {
                    new Dictionary<string, object?> { ["id"] = 7L, ["symbol"] = "ACME" },
                }));
            });
        var response = await client.GetAsync(upstreamUrl);
        var body = Json.Parse(await response.Content.ReadAsStringAsync())
            as Dictionary<string, object?> ?? new Dictionary<string, object?>();
        var prices = body.GetValueOrDefault("prices");
        if (fixedBuild && prices is not List<object?>)
        {
            return Results.Json(new Dictionary<string, object?>
            {
                ["first"] = null,
                ["note"] = "no prices",
            });
        }
        var first = ((List<object?>)prices!)[0];
        return Results.Json(new Dictionary<string, object?> { ["first"] = first });
    }
    catch (Exception)
    {
        return Results.Json(
            new Dictionary<string, object?> { ["error"] = "internal" }, statusCode: 500);
    }
});

await app.StartAsync();

if (!capturing)
{
    await app.WaitForShutdownAsync();
    return;
}

using var driver = new HttpClient();
var failing = await driver.GetAsync($"http://127.0.0.1:{port}/quote?symbol=ACME");
Console.WriteLine("capture fixture status " + (int)failing.StatusCode);
var recorded = traces[^1];
if (!recorded.Finished) recorded.Finish(null, (int)failing.StatusCode, false, true);
WriteCapture(recorded);
await app.StopAsync();
if (upstream != null) await upstream.StopAsync();

static void WriteCapture(BackendTrace trace)
{
    var envelope = new Dictionary<string, object?>
    {
        ["observedAtMs"] = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
        ["tz"] = TimeZoneInfo.Local.Id,
        ["runtime"] = "dotnet " + Environment.Version,
        ["os"] = Environment.OSVersion.Platform.ToString(),
        ["arch"] = System.Runtime.InteropServices.RuntimeInformation
            .ProcessArchitecture.ToString().ToLowerInvariant(),
        ["replaySeed"] = "c0ffee00c0ffee00",
    };
    var payload = new Dictionary<string, object?>
    {
        ["format"] = Capture.CaptureFormat,
        ["version"] = 2L,
        ["operation"] = trace.Events()[0].GetValueOrDefault("operation"),
        ["oracle"] = Capture.ServerErrorOracle,
        ["envelope"] = envelope,
        ["events"] = trace.Events().Cast<object?>().ToList(),
    };
    // No BOM: a capture is consumed as plain JSON by the CLI and every other SDK, and
    // Encoding.UTF8 would prepend one.
    File.WriteAllText(
        Environment.GetEnvironmentVariable("CAPTURE_OUT")!,
        Json.Canonical(payload),
        new UTF8Encoding(false));
}
