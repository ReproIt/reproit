// Capture-mode parity tests against sdk/reproit-backend-rs/src/capture.rs, mirroring
// sdk/reproit-backend-node/test/capture.test.js. The batch round-trip validates through the
// C# port of sdk/test/event_batch_v1.js.

using Xunit;

namespace ReproitBackend.Tests;

public class CaptureTests
{
    // Inert (no upload thread): these tests pin queue and batch semantics deterministically.
    private static Capture NewCapture(string appId = "app", string? build = null) =>
        Capture.CreateInert(new CaptureConfig
        {
            Endpoint = "http://c/v1/events",
            ApiKey = "sk",
            AppId = appId,
            Build = build,
        });

    private static BackendTrace FinishedTrace(int status, bool success)
    {
        var capture = NewCapture();
        var context = new TraceContext
        {
            TraceId = capture.Context().TraceId,
            Build = "1.2.3",
        };
        var trace = BackendTrace.Begin(context, "createOrder", new BeginOptions
        {
            Input = new Dictionary<string, object?>
            {
                ["body"] = new Dictionary<string, object?>
                {
                    ["item"] = "widget",
                    ["qty"] = 2L,
                },
            },
        });
        trace.Effect("read", new EffectOptions { Resource = "inventory", Key = "widget" });
        trace.Finish(new Dictionary<string, object?> { ["error"] = "boom" },
            status, success, true);
        return trace;
    }

    private static Dictionary<string, object?> BatchFor(int status, bool success)
    {
        var capture = NewCapture("app-demo", "1.2.3");
        var trace = FinishedTrace(status, success);
        return capture.BuildBatch(new List<Capture.CapturedOperation>
        {
            new()
            {
                Operation = "createOrder",
                Status = status,
                Events = trace.Events().ToList(),
            },
        });
    }

    private static List<object?> Frames(Dictionary<string, object?> batch) =>
        (List<object?>)batch["frames"]!;

    private static Dictionary<string, object?> FrameEvent(object? frame) =>
        (Dictionary<string, object?>)((Dictionary<string, object?>)frame!)["event"]!;

    [Fact]
    public void ServerErrorBatchIsAValidTaggedEventBatch()
    {
        var batch = BatchFor(500, false);
        EventBatchV1.ValidateEventBatch(batch);
        Assert.Equal(4, Frames(batch).Count);
        var finding = FrameEvent(Frames(batch)[3]);
        Assert.Equal("finding", finding["kind"]);
        var identity = (Dictionary<string, object?>)finding["identity"]!;
        Assert.Equal(Capture.ServerErrorOracle, identity["oracle"]);
        var context = (Dictionary<string, object?>)finding["context"]!;
        var capture = (Dictionary<string, object?>)context["reproitCapture"]!;
        Assert.Equal(Capture.CaptureFormat, capture["format"]);
        Assert.Equal("createOrder", capture["operation"]);
        var events = (List<Dictionary<string, object?>>)capture["events"]!;
        Assert.Equal(3, events.Count);
        // Redaction happened before anything left the process boundary.
        var input = (Dictionary<string, object?>)events[0]["input"]!;
        Assert.Equal("widget", ((Dictionary<string, object?>)input["body"]!)["item"]);
        Assert.Equal("1.2.3",
            ((Dictionary<string, object?>)batch["deployment"]!)["version"]);
    }

    [Fact]
    public void HealthyOperationsShipBackendFramesWithoutAFinding()
    {
        var batch = BatchFor(201, true);
        EventBatchV1.ValidateEventBatch(batch);
        Assert.Equal(3, Frames(batch).Count);
        Assert.All(Frames(batch), frame => Assert.Equal("backend", FrameEvent(frame)["kind"]));
    }

    [Fact]
    public void OversizedCapturesDropTrailingEffectsFirst()
    {
        var events = FinishedTrace(500, false).Events().ToList();
        events.Insert(2, new Dictionary<string, object?>
        {
            ["kind"] = "effect",
            ["effect"] = "write",
            ["resource"] = new string('x', 48 * 1024),
        });
        var batch = NewCapture().BuildBatch(new List<Capture.CapturedOperation>
        {
            new() { Operation = "createOrder", Status = 500, Events = events },
        });
        EventBatchV1.ValidateEventBatch(batch);
        var finding = FrameEvent(Frames(batch)[^1]);
        var context = (Dictionary<string, object?>)finding["context"]!;
        Assert.Equal(1L, context["captureDroppedEffects"]);
        var kept = (List<Dictionary<string, object?>>)
            ((Dictionary<string, object?>)context["reproitCapture"]!)["events"]!;
        Assert.Equal(3, kept.Count);
        Assert.Equal("effect", kept[1]["kind"]);
        Assert.Equal("inventory", kept[1]["resource"]);
    }

    [Fact]
    public void ACaptureThatCannotFitStartPlusReturnIsOmitted()
    {
        var events = new List<Dictionary<string, object?>>
        {
            new()
            {
                ["kind"] = "start",
                ["operation"] = "op",
                ["input"] = new Dictionary<string, object?>
                {
                    ["blob"] = new string('x', 48 * 1024),
                },
            },
            new() { ["kind"] = "return", ["status"] = 500L, ["success"] = false },
        };
        var batch = NewCapture().BuildBatch(new List<Capture.CapturedOperation>
        {
            new() { Operation = "op", Status = 500, Events = events },
        });
        var finding = FrameEvent(Frames(batch)[^1]);
        var context = (Dictionary<string, object?>)finding["context"]!;
        Assert.Equal(true, context["captureOmitted"]);
        Assert.False(context.ContainsKey("reproitCapture"));
    }

    [Fact]
    public void UnusableConfigsDisableCaptureInsteadOfFailing()
    {
        Assert.Null(Capture.Create(new CaptureConfig
        {
            Endpoint = "", ApiKey = "sk", AppId = "app",
        }));
        Assert.Null(Capture.Create(new CaptureConfig
        {
            Endpoint = "http://c", ApiKey = "", AppId = "app",
        }));
        Assert.Null(Capture.Create(new CaptureConfig
        {
            Endpoint = "http://c", ApiKey = "sk", AppId = "bad app",
        }));
        Assert.Null(Capture.Create(new CaptureConfig
        {
            Endpoint = "http://c", ApiKey = "sk", AppId = "app", Build = "bad build",
        }));
    }

    [Fact]
    public void RecordIgnoresUnfinishedTracesAndHealthyTracesWhenSamplingIsOff()
    {
        var capture = NewCapture();
        var open = BackendTrace.Begin(capture.Context(), "op");
        capture.Record(open);
        var healthy = BackendTrace.Begin(capture.Context(), "op");
        healthy.Finish(null, 200, true, true);
        capture.Record(healthy);
        Assert.Equal(0, capture.Stats().CapturedOperations);
        var failed = BackendTrace.Begin(capture.Context(), "op");
        failed.Finish(null, 200, false, true);
        capture.Record(failed);
        Assert.Equal(1, capture.Stats().CapturedOperations);
    }

    [Fact]
    public void QueueOverflowDropsTheOldestOperation()
    {
        var capture = NewCapture();
        for (var index = 0; index < 65; index++)
        {
            var trace = BackendTrace.Begin(capture.Context(), "op-" + index);
            trace.Finish(null, 500, false, true);
            capture.Record(trace);
        }
        var stats = capture.Stats();
        Assert.Equal(65, stats.CapturedOperations);
        Assert.Equal(1, stats.DroppedOperations);
        Assert.Equal("op-1", capture.PeekOldestOperation());
    }
}
