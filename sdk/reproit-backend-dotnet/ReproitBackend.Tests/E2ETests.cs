// Functional end-to-end test: a real Kestrel-hosted minimal API app with a planted 500, real
// HTTP requests, and a local stub ingest server. Asserts the finding batch arrives correctly
// tagged with the reproitCapture sequence, and that a scan-time request round-trips the
// x-reproit-events response header. Mirrors sdk/reproit-backend-node/test/e2e.test.js.

using System.Text;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.Logging;
using Xunit;

namespace ReproitBackend.Tests;

public class E2ETests
{
    private sealed record IngestRequest(string? Authorization, object? Batch);

    private static async Task<(WebApplication App, string Url, List<IngestRequest> Received)>
        StartStubIngest()
    {
        var received = new List<IngestRequest>();
        var builder = WebApplication.CreateBuilder();
        builder.Logging.ClearProviders();
        var app = builder.Build();
        app.MapPost("/v1/capture-batches", async context =>
        {
            using var reader = new StreamReader(context.Request.Body, Encoding.UTF8);
            var body = await reader.ReadToEndAsync();
            lock (received)
            {
                received.Add(new IngestRequest(
                    context.Request.Headers.Authorization.FirstOrDefault(),
                    Json.Parse(body)));
            }
            context.Response.ContentType = "application/json";
            await context.Response.WriteAsync("{\"accepted\":true}");
        });
        app.Urls.Add("http://127.0.0.1:0");
        await app.StartAsync();
        return (app, app.Urls.First() + "/v1/capture-batches", received);
    }

    private static async Task<(WebApplication App, string BaseUrl)> StartHostApp(
        Capture capture)
    {
        var builder = WebApplication.CreateBuilder();
        builder.Logging.ClearProviders();
        var app = builder.Build();
        app.UseReproit(new ReproitOptions { Capture = capture });
        app.MapGet("/ok", () => Results.Json(new { ok = true }));
        app.MapPost("/boom", (HttpContext context) =>
        {
            context.ReproitTrace()?.Effect("write", new EffectOptions
            {
                Resource = "orders",
                Key = "1",
            });
            return Results.Json(new { error = "boom" }, statusCode: 500);
        });
        app.Urls.Add("http://127.0.0.1:0");
        await app.StartAsync();
        return (app, app.Urls.First());
    }

    private static void AssertServerErrorBatch(List<IngestRequest> received)
    {
        Assert.Single(received);
        Assert.Equal("Bearer sk_live_test", received[0].Authorization);
        var batch = (Dictionary<string, object?>)received[0].Batch!;
        Assert.Equal(1L, batch["version"]);
        Assert.Equal("app-e2e", batch["projectId"]);
        Assert.Equal("9.9.9",
            ((Dictionary<string, object?>)batch["deployment"]!)["version"]);
        var events = ((List<object?>)batch["events"]!)
            .Select(captured => (Dictionary<string, object?>)
                ((Dictionary<string, object?>)captured!)["event"]!)
            .ToList();
        Assert.Equal(
            new[]
            {
                "operation-start",
                "trigger",
                "state-access",
                "operation-end",
                "observation",
            },
            events.Select(evt => (string)evt["kind"]!).ToArray());
        Assert.Equal("orders", events[2]["subject"]);
        var failure = (Dictionary<string, object?>)events[^1]["failure"]!;
        Assert.Equal("backend-server-error:POST /boom", failure["signature"]);
        // The secret-shaped input field was structurally redacted before upload.
        var triggerValue = (Dictionary<string, object?>)events[1]["value"]!;
        Assert.Equal("replayable", triggerValue["representation"]);
        var input = (Dictionary<string, object?>)triggerValue["value"]!;
        var body = (Dictionary<string, object?>)
            input["body"]!;
        var redacted = (Dictionary<string, object?>)
            ((Dictionary<string, object?>)body["apiKey"]!)["$reproit"]!;
        Assert.Equal(true, redacted["redacted"]);
        Assert.Equal("widget", body["item"]);
    }

    private static async Task AssertScanHeader(HttpClient client, string baseUrl)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, baseUrl + "/ok");
        request.Headers.Add("x-reproit-trace", "trace-e2e");
        request.Headers.Add("x-reproit-actor", "alice");
        using var response = await client.SendAsync(request);
        Assert.Equal(200, (int)response.StatusCode);
        Assert.True(response.Headers.TryGetValues("x-reproit-events", out var values),
            "expected an x-reproit-events response header");
        var header = values!.First();
        var decoded = Encoding.UTF8.GetString(TraceTests.DecodeBase64Url(header));
        var events = ((List<object?>)Json.Parse(decoded)!)
            .Select(evt => (Dictionary<string, object?>)evt!)
            .ToList();
        Assert.Equal("trace-e2e", events[0]["traceId"]);
        Assert.Equal("alice", events[0]["actor"]);
        Assert.Equal("return", events[^1]["kind"]);
        Assert.Equal(200L, events[^1]["status"]);
    }

    [Fact]
    public async Task Planted500ShipsATaggedFindingBatchToTheStubIngest()
    {
        var (ingest, ingestUrl, received) = await StartStubIngest();
        var capture = Capture.Create(new CaptureConfig
        {
            Endpoint = ingestUrl,
            ApiKey = "sk_live_test",
            AppId = "app-e2e",
            Build = "9.9.9",
            FlushIntervalMs = 100,
        });
        Assert.NotNull(capture);
        var (app, baseUrl) = await StartHostApp(capture!);
        using var client = new HttpClient();
        try
        {
            using var boom = await client.PostAsync(baseUrl + "/boom", new StringContent(
                "{\"item\":\"widget\",\"apiKey\":\"sk_live_leak\"}",
                Encoding.UTF8, "application/json"));
            Assert.Equal(500, (int)boom.StatusCode);
            Assert.True(capture!.Flush(5000));
            AssertServerErrorBatch(received);
            await AssertScanHeader(client, baseUrl);
            // The healthy scan-time request must not have been captured.
            Assert.Equal(1, capture.Stats().CapturedOperations);
        }
        finally
        {
            await app.StopAsync();
            await ingest.StopAsync();
        }
    }
}
