// CI capture mode (Ci.cs): a failing test spools a test-trigger capsule, a replay run
// re-executes only the named test and reports the structured result marker, and the spool
// cap drops loudly. Node scenarios run the ci wrapper in child processes because
// instrument.install() rewires process-wide clients; the .NET boundary is explicit, so these
// scenarios drive the same seams (ReadEnvironment, Stderr) in process and stay
// deterministic. The dotnet-flaky-ci-e2e gate covers the real child-process path through
// `dotnet test` and `reproit check`.

using System.Text;
using Xunit;

namespace ReproitBackend.Tests;

// The seams are static, so Ci scenarios must not interleave with each other.
[Collection("ci-mode")]
public sealed class CiTests : IDisposable
{
    private readonly Dictionary<string, string?> _env = new();
    private readonly StringWriter _stderr = new();
    private readonly Func<string, string?> _priorEnv;
    private readonly TextWriter _priorStderr;
    private readonly string _work;

    public CiTests()
    {
        _priorEnv = Ci.ReadEnvironment;
        _priorStderr = Ci.Stderr;
        Ci.ReadEnvironment = name => _env.GetValueOrDefault(name);
        Ci.Stderr = _stderr;
        _work = Path.Combine(Path.GetTempPath(), "reproit-ci-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_work);
    }

    public void Dispose()
    {
        Ci.ReadEnvironment = _priorEnv;
        Ci.Stderr = _priorStderr;
        Directory.Delete(_work, recursive: true);
    }

    private Dictionary<string, object?> SpooledCapsule(string spool)
    {
        var files = Directory.GetFiles(spool, "capsule-*.json");
        Assert.Single(files);
        return (Dictionary<string, object?>)Json.Parse(File.ReadAllText(files[0]))!;
    }

    [Fact]
    public async Task FailingTestSpoolsATestTriggerCapsuleWithTheExchange()
    {
        var spool = Path.Combine(_work, "spool");
        _env["REPROIT_CI_CAPTURE"] = "1";
        _env["REPROIT_CI_SPOOL"] = spool;
        await Assert.ThrowsAsync<InvalidOperationException>(() =>
            Ci.TestAsync("unit", "asserts the upstream answer", async () =>
            {
                // A dependency exchange through the explicit boundary, no socket needed.
                var outcome = await Instrument.Db.RunAsync(
                    "SELECT n FROM answers", null,
                    () => Task.FromResult(new Instrument.Db.Outcome("SELECT", 1, new object?[]
                    {
                        new Dictionary<string, object?> { ["n"] = 7L },
                    })));
                if (outcome.RowCount == 1)
                {
                    throw new InvalidOperationException("7 !== 8");
                }
            }));
        Assert.Contains(Ci.SpoolMarker, _stderr.ToString());
        var capsule = SpooledCapsule(spool);
        Assert.Equal("reproit-backend-capture", capsule["format"]);
        Assert.Equal(2L, capsule["version"]);
        Assert.Equal("test:unit#asserts the upstream answer", capsule["operation"]);
        Assert.Equal(Ci.TestFailureOracle, capsule["oracle"]);
        var envelope = (Dictionary<string, object?>)capsule["envelope"]!;
        Assert.IsType<string>(envelope["replaySeed"]);
        var events = ((List<object?>)capsule["events"]!)
            .Cast<Dictionary<string, object?>>().ToList();
        Assert.Single(events, evt => evt.ContainsKey("exchange"));
        var returned = events[^1];
        Assert.Equal("return", returned["kind"]);
        Assert.Equal(false, returned["success"]);
        // The trigger is a test, not a request: no HTTP status key, like the Node capsule.
        Assert.False(returned.ContainsKey("status"));
        var output = (Dictionary<string, object?>)returned["output"]!;
        Assert.Equal("7 !== 8", output["error"]);
    }

    [Fact]
    public async Task ReplayRunsOnlyTheNamedTestAndReportsTheResultMarker()
    {
        var capsule = Path.Combine(_work, "capsule.json");
        File.WriteAllText(capsule, Json.Canonical(new Dictionary<string, object?>
        {
            ["format"] = Capture.CaptureFormat,
            ["version"] = 2L,
            ["operation"] = "test:unit#target",
            ["oracle"] = Ci.TestFailureOracle,
            ["envelope"] = new Dictionary<string, object?>(),
            ["events"] = new List<object?>(),
        }));
        _env["REPROIT_REPLAY"] = capsule;

        // Not the capsule's named test: the body does not run and nothing is reported.
        var ran = false;
        await Ci.TestAsync("unit", "other", () =>
        {
            ran = true;
            return Task.CompletedTask;
        });
        Assert.False(ran);
        Assert.Equal(string.Empty, _stderr.ToString());

        await Assert.ThrowsAsync<InvalidOperationException>(() =>
            Ci.TestAsync("unit", "target",
                () => throw new InvalidOperationException("7 !== 8")));
        var failedLine = _stderr.ToString().Split('\n')
            .Single(line => line.StartsWith(Ci.ResultMarker, StringComparison.Ordinal));
        var failed = (Dictionary<string, object?>)
            Json.Parse(failedLine[Ci.ResultMarker.Length..])!;
        Assert.Equal("test:unit#target", failed["operation"]);
        Assert.Equal("failed", failed["status"]);
        Assert.Equal("7 !== 8", failed["failure"]);

        await Ci.TestAsync("unit", "target", () => Task.CompletedTask);
        Assert.Contains("\"status\":\"passed\"", _stderr.ToString());
    }

    [Fact]
    public async Task ACapsuleWithoutATestTriggerIdentityIsRejected()
    {
        var capsule = Path.Combine(_work, "http.json");
        File.WriteAllText(capsule, "{\"operation\":\"GET /quote\"}");
        _env["REPROIT_REPLAY"] = capsule;
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() =>
            Ci.TestAsync("unit", "target", () => Task.CompletedTask));
        Assert.Contains("test trigger identity", error.Message);
    }

    [Fact]
    public void AFullSpoolDropsTheCapsuleAndCountsTheDrop()
    {
        var spool = Path.Combine(_work, "full");
        Directory.CreateDirectory(spool);
        // Pre-fill the spool to the floor cap so the next capsule cannot fit.
        File.WriteAllText(Path.Combine(spool, "existing.json"), new string('x', 4 * 1024));
        var body = Encoding.UTF8.GetBytes("{\"capsule\":true}");
        Assert.Null(Ci.Spool(body, "test:s#t", spool, 4 * 1024));
        Assert.Empty(Directory.GetFiles(spool, "capsule-*.json"));
        Assert.Equal("1", File.ReadAllText(Path.Combine(spool, "dropped.count")).Trim());
        // Drops accumulate; the counter is a running total, not a flag.
        Assert.Null(Ci.Spool(body, "test:s#t", spool, 4 * 1024));
        Assert.Equal("2", File.ReadAllText(Path.Combine(spool, "dropped.count")).Trim());
    }

    [Fact]
    public void SpoolBoundsClampToTheFloorAndCeiling()
    {
        Assert.Equal(Ci.DefaultSpoolMaxBytes, Ci.SpoolMaxBytes());
        _env["REPROIT_CI_SPOOL_MAX"] = "10";
        Assert.Equal(4L * 1024, Ci.SpoolMaxBytes());
        _env["REPROIT_CI_SPOOL_MAX"] = "999999999999";
        Assert.Equal(64L * 1024 * 1024, Ci.SpoolMaxBytes());
        _env["REPROIT_CI_SPOOL_MAX"] = "not-a-number";
        Assert.Equal(Ci.DefaultSpoolMaxBytes, Ci.SpoolMaxBytes());
        Assert.Equal(Ci.DefaultSpoolDir, Ci.SpoolDir());
        _env["REPROIT_CI_SPOOL"] = "/somewhere/else";
        Assert.Equal("/somewhere/else", Ci.SpoolDir());
    }

    [Fact]
    public async Task WithoutCaptureOrReplayEnvTheWrapperIsInert()
    {
        var ran = false;
        await Ci.TestAsync("unit", "plain", () =>
        {
            ran = true;
            return Task.CompletedTask;
        });
        Assert.True(ran);
        await Assert.ThrowsAsync<InvalidOperationException>(() =>
            Ci.TestAsync("unit", "plain", () => throw new InvalidOperationException("boom")));
        Assert.Equal(string.Empty, _stderr.ToString());
    }

    [Fact]
    public void TestIdentityIsBoundedAndPrefixed()
    {
        Assert.Equal("test:suite#name", Ci.OperationFor(" suite ", " name "));
        var oversized = new string('s', 200);
        var operation = Ci.OperationFor(oversized, "t");
        Assert.Equal("test:" + new string('s', 120) + "#t", operation);
    }
}
