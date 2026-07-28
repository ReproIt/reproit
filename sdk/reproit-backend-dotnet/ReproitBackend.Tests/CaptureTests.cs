// Universal source-neutral capture tests, mirroring the Node recorder contract.

using Xunit;

namespace ReproitBackend.Tests;

public class CaptureTests
{
    // Inert (no upload thread): these tests pin queue and batch semantics deterministically.
    private static Capture NewCapture(string appId = "app", string? build = null) =>
        Capture.CreateInert(new CaptureConfig
        {
            Endpoint = "http://c/v1/capture-batches",
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

    private static List<Dictionary<string, object?>> Events(
        Dictionary<string, object?> batch) =>
        (List<Dictionary<string, object?>>)batch["events"]!;

    private static Dictionary<string, object?> Event(
        Dictionary<string, object?> captured) =>
        (Dictionary<string, object?>)captured["event"]!;

    [Fact]
    public void ServerErrorBatchIsASourceNeutralCausalCapture()
    {
        var batch = BatchFor(500, false);
        Assert.Equal(1L, batch["version"]);
        Assert.Equal("app-demo", batch["projectId"]);
        var events = Events(batch);
        Assert.Equal("operation-start", Event(events[0])["kind"]);
        Assert.Equal("trigger", Event(events[1])["kind"]);
        var observation = Event(events[^1]);
        Assert.Equal("observation", observation["kind"]);
        var failure = (Dictionary<string, object?>)observation["failure"]!;
        Assert.Equal("backend:createOrder", failure["signature"]);
        Assert.Equal("1.2.3",
            ((Dictionary<string, object?>)batch["deployment"]!)["version"]);
    }

    [Fact]
    public void HealthyOperationsShipFactsWithoutAFailureObservation()
    {
        var batch = BatchFor(201, true);
        Assert.DoesNotContain(Events(batch), captured =>
            Event(captured)["kind"] as string == "observation");
    }

    [Fact]
    public void UnrelatedOperationsCannotShareAnOccurrenceBatch()
    {
        var operation = new Capture.CapturedOperation
        {
            Operation = "createOrder",
            Status = 500,
            Events = FinishedTrace(500, false).Events().ToList(),
        };
        Assert.Throws<ArgumentException>(() =>
            NewCapture().BuildBatch(new List<Capture.CapturedOperation>
            {
                operation,
                operation,
            }));
    }

    [Fact]
    public void UniversalRecorderReportsBoundedOverflow()
    {
        var recorder = new UniversalRecorder(new UniversalRecorderOptions
        {
            ProjectId = "app",
            EmitterId = "test",
            Component = "worker",
            MaxEvents = 3,
        });
        recorder.OperationStart("one");
        recorder.OperationStart("two");
        recorder.OperationEnd("two", "succeeded");
        recorder.Effect("write", "result");
        var batch = recorder.Finish()!;
        Assert.Equal(3, Events(batch).Count);
        Assert.Equal("defect", Event(Events(batch)[^1])["kind"]);
    }

    [Fact]
    public void UniversalRecorderNormalizesExternalCorrelationIds()
    {
        var recorder = new UniversalRecorder(new UniversalRecorderOptions
        {
            ProjectId = "app",
            EmitterId = "test",
            Component = "worker",
            SessionId = "session/with spaces",
        });
        recorder.OperationStart("POST /orders", new UniversalEventContext
        {
            Actor = "Ada Lovelace",
            TraceId = "trace/with spaces",
            SpanId = "trace/with spaces:POST /orders",
        });
        var batch = recorder.Finish()!;
        var captured = Events(batch)[0];
        Assert.StartsWith("sessionid:", batch["sessionId"] as string);
        Assert.StartsWith("actor:", captured["actor"] as string);
        Assert.StartsWith("traceid:", captured["traceId"] as string);
        Assert.StartsWith("spanid:", captured["spanId"] as string);
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
