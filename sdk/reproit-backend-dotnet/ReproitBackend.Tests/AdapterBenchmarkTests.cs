using System.Diagnostics;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.Logging;
using Xunit;

namespace ReproitBackend.Tests;

public class AdapterBenchmarkTests
{
    private const int Dependencies = 64;

    private static int Configured(string name, int fallback) =>
        int.TryParse(Environment.GetEnvironmentVariable(name), out var value) && value > 0
            ? value : fallback;

    private static double Median(IEnumerable<double> values)
    {
        var sorted = values.Order().ToArray();
        return sorted[sorted.Length / 2];
    }

    private static async Task<double> HttpCost(bool mounted, bool traced, int runs)
    {
        var builder = WebApplication.CreateBuilder();
        builder.Logging.ClearProviders();
        var app = builder.Build();
        if (mounted) app.UseReproit();
        app.MapGet("/account", () => Results.Json(new { account = new { id = 42, ok = true } }));
        app.Urls.Add("http://127.0.0.1:0");
        await app.StartAsync();
        using var client = new HttpClient();
        try
        {
            async Task Fire()
            {
                using var request = new HttpRequestMessage(
                    HttpMethod.Get, app.Urls.First() + "/account?id=42");
                if (traced) request.Headers.Add("x-reproit-trace", "bench-trace");
                using var response = await client.SendAsync(request);
                response.EnsureSuccessStatusCode();
                await response.Content.LoadIntoBufferAsync();
            }
            for (var index = 0; index < Math.Min(500, runs / 4); index++) await Fire();
            var started = Stopwatch.GetTimestamp();
            for (var index = 0; index < runs; index++) await Fire();
            return Stopwatch.GetElapsedTime(started).TotalMicroseconds / runs;
        }
        finally
        {
            await app.StopAsync();
            await app.DisposeAsync();
        }
    }

    private static double DependencyCost(bool captured, int runs)
    {
        var context = new TraceContext { TraceId = "dependency-benchmark", ActionIndex = 1 };
        var exchange = new Dictionary<string, object?>
        {
            ["request"] = new { method = "GET", url = "http://pricing.test/quote?tier=gold" },
            ["response"] = new { status = 200, body = new { price = 42 } },
        };
        var started = Stopwatch.GetTimestamp();
        for (var run = 0; run < runs; run++)
        {
            var trace = BackendTrace.Begin(context, "dependencyBenchmark");
            if (!captured) continue;
            for (var index = 0; index < Dependencies; index++)
            {
                trace.Effect("call", new EffectOptions
                {
                    Resource = "pricing", Key = index.ToString(), Exchange = exchange,
                });
            }
        }
        return Stopwatch.GetElapsedTime(started).TotalMicroseconds / (runs * Dependencies);
    }

    [Fact]
    public async Task RealMiddlewareAndDependencyCaptureStayWithinCeilings()
    {
        var runs = Configured("REPROIT_ADAPTER_BENCH_RUNS", 1000);
        var rounds = Configured("REPROIT_ADAPTER_BENCH_ROUNDS", 5);
        var baseline = new List<double>();
        var inactive = new List<double>();
        var active = new List<double>();
        var control = new List<double>();
        var dependencyBaseline = new List<double>();
        var dependencyCaptured = new List<double>();
        var dependencyControl = new List<double>();
        for (var round = 0; round < rounds; round++)
        {
            baseline.Add(await HttpCost(false, false, runs));
            inactive.Add(await HttpCost(true, false, runs));
            active.Add(await HttpCost(true, true, runs));
            control.Add(await HttpCost(false, false, runs));
            dependencyBaseline.Add(DependencyCost(false, runs));
            dependencyCaptured.Add(DependencyCost(true, runs));
            dependencyControl.Add(DependencyCost(false, runs));
        }
        var baselineMicros = Median(baseline);
        var noise = Math.Abs(Median(control) - baselineMicros);
        var inactiveCost = Median(inactive) - baselineMicros;
        var activeCost = Median(active) - baselineMicros;
        var dependencyBase = Median(dependencyBaseline);
        var dependencyNoise = Math.Abs(Median(dependencyControl) - dependencyBase);
        var dependencyCost = Median(dependencyCaptured) - dependencyBase;
        Assert.True(noise < 500, $"HTTP noise {noise:F2}us");
        Assert.True(inactiveCost < 500, $"inactive cost {inactiveCost:F2}us");
        Assert.True(activeCost < 1500, $"active cost {activeCost:F2}us");
        Assert.True(dependencyNoise < 10, $"dependency noise {dependencyNoise:F2}us");
        Assert.True(dependencyCost < 50, $"dependency cost {dependencyCost:F2}us");
        Console.WriteLine(
            $"{{\"language\":\"dotnet\",\"runs\":{runs},\"rounds\":{rounds},"
            + $"\"noiseFloorMicros\":{noise:F2},\"baselineMicros\":{baselineMicros:F2},"
            + $"\"inactiveCostMicros\":{inactiveCost:F2},\"activeCostMicros\":{activeCost:F2},"
            + $"\"dependencyNoiseFloorMicros\":{dependencyNoise:F2},"
            + $"\"dependencyCaptureCostMicros\":{dependencyCost:F2},"
            + "\"dependencyCeilingMicros\":50}");
    }
}
