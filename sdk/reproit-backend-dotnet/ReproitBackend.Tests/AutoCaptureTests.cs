// The automatic DiagnosticSource capture path: a plain HttpClient call records its outbound
// exchange onto the ambient trace with no Instrument.Handler() wiring. Uses a real Kestrel
// server so the "System.Net.Http" DiagnosticListener actually fires, mirroring E2ETests.

using System.Net;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.Logging;
using Xunit;

namespace ReproitBackend.Tests;

public class AutoCaptureTests
{
    private static readonly TraceContext CaptureContext = new()
    {
        TraceId = "auto-1",
        CaptureEnvelope = true,
    };

    private static async Task<(WebApplication App, string BaseUrl)> StartServer()
    {
        var builder = WebApplication.CreateBuilder();
        builder.Logging.ClearProviders();
        var app = builder.Build();
        app.MapGet("/quote", () => Results.Json(new { symbol = "ACME", price = 42 }));
        app.Urls.Add("http://127.0.0.1:0");
        await app.StartAsync();
        return (app, app.Urls.First());
    }

    [Fact]
    public async Task AutoCaptureRecordsAPlainHttpClientCallOnTheAmbientTrace()
    {
        Instrument.InstallAutoCapture();
        var (app, baseUrl) = await StartServer();
        var trace = BackendTrace.Begin(CaptureContext, "GET /quote");
        try
        {
            var body = string.Empty;
            await Instrument.ScopeAsync(trace, async () =>
            {
                using var client = new HttpClient();
                using var response = await client.GetAsync(baseUrl + "/quote?tier=gold");
                Assert.Equal(HttpStatusCode.OK, response.StatusCode);
                body = await response.Content.ReadAsStringAsync();
            });
            // The app still reads the real response body: the observer never consumed it.
            Assert.Contains("ACME", body);
            var exchange = trace.Events()
                .Select(evt => evt.GetValueOrDefault("exchange") as Dictionary<string, object?>)
                .FirstOrDefault(recorded => recorded != null);
            Assert.NotNull(exchange);
            Assert.Equal("http", exchange!["protocol"]);
            var request = (Dictionary<string, object?>)exchange["request"]!;
            Assert.Equal("GET", request["method"]);
            Assert.Equal("/quote?tier=gold", Replay.PathAndQuery(request["url"]));
            var response = (Dictionary<string, object?>)exchange["response"]!;
            Assert.Equal(200L, response["status"]);
            // The automatic path records status and headers, never the response body: the file
            // header names the boundary, and the explicit Handler() path records the body.
            Assert.False(response.ContainsKey("body"));
            Assert.True(((Dictionary<string, object?>)response["headers"]!)
                .ContainsKey("content-type"));
        }
        finally
        {
            await app.StopAsync();
        }
    }
}
