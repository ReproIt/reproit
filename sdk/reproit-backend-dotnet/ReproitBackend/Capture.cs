// Production capture mode: config-gated self-sampling upload of finished operation traces to
// the Reproit Cloud ingest endpoint (`/v1/events`).
//
// .NET port of sdk/reproit-backend-rs/src/capture.rs. Scan-time tracing stays untouched: this
// class only adds a place to hand a finished BackendTrace when no `x-reproit-trace` header
// exists. Operations that end in a server error (HTTP 5xx) or report `success == false` are
// always captured; healthy operations only under an optional per-mille baseline sample
// (default 0).
//
// Everything is bounded and capture failure is invisible to the host app: a fixed-depth queue
// drops oldest on overflow, batches and retries are capped, uploads run on one background
// thread over a shared HttpClient, and `Record` never blocks or throws.

using System.Net.Http.Headers;
using System.Text;
using System.Text.RegularExpressions;

namespace ReproitBackend;

public sealed class CaptureConfig
{
    public string? Endpoint { get; init; }
    public string? ApiKey { get; init; }
    public string? AppId { get; init; }
    public string? Build { get; init; }
    public int HealthySamplePerMille { get; init; }
    public int FlushIntervalMs { get; init; } = 3000;
    public int RequestTimeoutMs { get; init; } = 5000;
    public int RetryLimit { get; init; } = 2;
}

public sealed class CaptureStats
{
    public long CapturedOperations { get; init; }
    public long DroppedOperations { get; init; }
    public long SentBatches { get; init; }
    public long FailedBatches { get; init; }
}

// Handle to the capture worker. Thread-safe; one queue, one upload thread.
public sealed class Capture
{
    // Payload format identifier of the replayable capture object attached to the finding
    // context (`context.reproitCapture`).
    public const string CaptureFormat = "reproit-backend-capture";
    public const int CaptureVersion = 1;
    // First-class registry oracle id for an operation that returned HTTP 5xx.
    public const string ServerErrorOracle = "backend-server-error";
    public const string SdkName = "reproit-backend-dotnet";

    // Bounds. Queue overflow drops the OLDEST pending operation; an oversized capture payload
    // drops trailing effect events before it drops itself.
    public const int MaxQueueOperations = 64;
    public const int MaxBatchOperations = 16;
    public const int MaxCaptureJsonBytes = 48 * 1024;
    public const int MinFlushIntervalMs = 100;
    public const int MaxRetryLimit = 5;

    // The ingest protocol token charset (`validate_token` in reproit-protocol).
    private static readonly Regex Token =
        new("^[A-Za-z0-9._:-]{1,128}$", RegexOptions.Compiled);

    private static readonly HttpClient Client =
        new() { Timeout = Timeout.InfiniteTimeSpan };

    private readonly string _endpoint;
    private readonly string _apiKey;
    private readonly string _appId;
    private readonly string? _build;
    private readonly int _healthySamplePerMille;
    private readonly int _flushIntervalMs;
    private readonly int _requestTimeoutMs;
    private readonly int _retryLimit;

    private readonly object _signal = new();
    private readonly Queue<CapturedOperation> _queue = new();
    private bool _sending;
    private bool _flushNow;
    private long _traceSequence;
    private long _batchSequence;
    private long _capturedOperations;
    private long _droppedOperations;
    private long _sentBatches;
    private long _failedBatches;

    internal sealed class CapturedOperation
    {
        public required string Operation { get; init; }
        public required long? Status { get; init; }
        public required List<Dictionary<string, object?>> Events { get; init; }
    }

    // Start capture mode. Returns null (capture disabled, host unaffected) when the config is
    // unusable: empty endpoint/key or identifiers the ingest protocol would reject.
    public static Capture? Create(CaptureConfig config)
    {
        if (string.IsNullOrWhiteSpace(config.Endpoint)) return null;
        if (string.IsNullOrWhiteSpace(config.ApiKey)) return null;
        if (config.AppId == null || !Token.IsMatch(config.AppId)) return null;
        if (config.Build != null && !Token.IsMatch(config.Build)) return null;
        return new Capture(config, startWorker: true);
    }

    // Queue and batch semantics without the upload thread; deterministic unit tests only.
    internal static Capture CreateInert(CaptureConfig config) =>
        new(config, startWorker: false);

    private Capture(CaptureConfig config, bool startWorker)
    {
        _endpoint = config.Endpoint!;
        _apiKey = config.ApiKey!;
        _appId = config.AppId!;
        _build = config.Build;
        _healthySamplePerMille = Math.Max(0, config.HealthySamplePerMille);
        _flushIntervalMs = Math.Max(MinFlushIntervalMs, config.FlushIntervalMs);
        _requestTimeoutMs = config.RequestTimeoutMs;
        _retryLimit = Math.Min(MaxRetryLimit, Math.Max(0, config.RetryLimit));
        if (startWorker)
        {
            var worker = new Thread(RunWorker)
            {
                IsBackground = true,
                Name = "reproit-capture",
            };
            worker.Start();
        }
    }

    // Synthesized trace context for capture-mode operations, replacing the scan-time
    // `x-reproit-trace` header requirement.
    public TraceContext Context()
    {
        var sequence = Interlocked.Increment(ref _traceSequence);
        return new TraceContext
        {
            TraceId = "cap-" + NowMs() + "-" + sequence,
            ActionIndex = 0,
            Build = _build,
        };
    }

    // Hand a finished trace to the sampler. Unfinished traces are ignored. Never blocks and
    // never fails visibly; overflow drops the oldest queued operation.
    public void Record(BackendTrace trace)
    {
        try
        {
            var events = trace.Events();
            Dictionary<string, object?>? returned = null;
            for (var index = events.Count - 1; index >= 0; index--)
            {
                if (events[index].TryGetValue("kind", out var kind) && (kind as string) == "return")
                {
                    returned = events[index];
                    break;
                }
            }
            if (returned == null) return;
            var success = !returned.TryGetValue("success", out var rawSuccess) ||
                rawSuccess is not bool flag || flag;
            long? status = returned.TryGetValue("status", out var rawStatus) &&
                rawStatus is long code && code >= 0 && code <= 0xffff ? code : null;
            var error = !success || status >= 500;
            if (!error && !SampleHealthy()) return;
            if (events.Count == 0 ||
                !events[0].TryGetValue("operation", out var rawOperation) ||
                rawOperation is not string operation)
            {
                return;
            }
            var captured = new CapturedOperation
            {
                Operation = operation,
                Status = status,
                Events = events.ToList(),
            };
            lock (_signal)
            {
                _capturedOperations++;
                _queue.Enqueue(captured);
                if (_queue.Count > MaxQueueOperations)
                {
                    _queue.Dequeue();
                    _droppedOperations++;
                }
                Monitor.PulseAll(_signal);
            }
        }
        catch
        {
            // Capture must never surface errors into the host app.
        }
    }

    // Block up to `timeoutMs` until every queued operation has been sent (or dropped).
    // Returns false on timeout. Intended for tests, examples, and graceful shutdown.
    public bool Flush(int timeoutMs)
    {
        var deadline = Environment.TickCount64 + timeoutMs;
        lock (_signal)
        {
            _flushNow = true;
            Monitor.PulseAll(_signal);
            while (_queue.Count > 0 || _sending)
            {
                var remaining = deadline - Environment.TickCount64;
                if (remaining <= 0) return false;
                Monitor.Wait(_signal, (int)remaining);
            }
            return true;
        }
    }

    public CaptureStats Stats()
    {
        lock (_signal)
        {
            return new CaptureStats
            {
                CapturedOperations = _capturedOperations,
                DroppedOperations = _droppedOperations,
                SentBatches = _sentBatches,
                FailedBatches = _failedBatches,
            };
        }
    }

    internal string? PeekOldestOperation()
    {
        lock (_signal)
        {
            return _queue.Count > 0 ? _queue.Peek().Operation : null;
        }
    }

    private bool SampleHealthy()
    {
        if (_healthySamplePerMille <= 0) return false;
        if (_healthySamplePerMille >= 1000) return true;
        return Random.Shared.NextDouble() * 1000 < _healthySamplePerMille;
    }

    private void RunWorker()
    {
        while (true)
        {
            var operations = NextBatch();
            var sent = false;
            try
            {
                sent = Send(BuildBatch(operations));
            }
            catch
            {
                // Fail closed: drop, never crash the host.
            }
            lock (_signal)
            {
                if (sent) _sentBatches++;
                else
                {
                    _failedBatches++;
                    _droppedOperations += operations.Count;
                }
                _sending = false;
                Monitor.PulseAll(_signal);
            }
        }
    }

    // Wait for work, gather up to the batch cap within one flush interval, then drain.
    // `_flushNow` (set by `Flush`) cuts the gather short.
    private List<CapturedOperation> NextBatch()
    {
        lock (_signal)
        {
            while (true)
            {
                if (_queue.Count > 0)
                {
                    var deadline = Environment.TickCount64 + _flushIntervalMs;
                    while (_queue.Count < MaxBatchOperations && !_flushNow)
                    {
                        var remaining = deadline - Environment.TickCount64;
                        if (remaining <= 0) break;
                        if (!Monitor.Wait(_signal, (int)remaining)) break;
                    }
                    _flushNow = false;
                    var take = Math.Min(_queue.Count, MaxBatchOperations);
                    _sending = true;
                    var operations = new List<CapturedOperation>(take);
                    for (var index = 0; index < take; index++)
                    {
                        operations.Add(_queue.Dequeue());
                    }
                    return operations;
                }
                _flushNow = false;
                Monitor.Wait(_signal);
            }
        }
    }

    // Build one event-batch-v1 payload: every captured event ships as a `backend` frame, and
    // each 5xx operation additionally ships a `finding` frame tagged `backend-server-error`
    // whose context carries the full replayable capture object.
    internal Dictionary<string, object?> BuildBatch(List<CapturedOperation> operations)
    {
        var sequence = Interlocked.Increment(ref _batchSequence);
        var batchId = "cap-" + NowMs() + "-" + sequence;
        var frames = new List<object?>();
        void Frame(Dictionary<string, object?> evt) => frames.Add(new Dictionary<string, object?>
        {
            ["runId"] = batchId,
            ["sequence"] = (long)frames.Count + 1,
            ["scope"] = new Dictionary<string, object?> { ["domain"] = "shared" },
            ["event"] = evt,
        });
        foreach (var operation in operations)
        {
            foreach (var evt in operation.Events)
            {
                Frame(new Dictionary<string, object?>
                {
                    ["kind"] = "backend",
                    ["evidence"] = evt,
                });
            }
            if (operation.Status is not >= 500) continue;
            var signature = "backend:" + operation.Operation;
            var message = "backend operation " + operation.Operation +
                " returned HTTP " + operation.Status;
            var context = new Dictionary<string, object?> { ["capture"] = SdkName };
            if (_build != null)
            {
                context["build"] = new Dictionary<string, object?> { ["version"] = _build };
            }
            var (payload, droppedEffects) = CapturePayload(operation);
            if (payload == null) context["captureOmitted"] = true;
            else
            {
                context["reproitCapture"] = payload;
                if (droppedEffects > 0) context["captureDroppedEffects"] = (long)droppedEffects;
            }
            Frame(new Dictionary<string, object?>
            {
                ["kind"] = "finding",
                ["signature"] = signature,
                ["message"] = message,
                ["identity"] = new Dictionary<string, object?>
                {
                    ["oracle"] = ServerErrorOracle,
                    ["invariant"] = "backend:server-error",
                    ["kind"] = "server-error",
                    ["message"] = message,
                    ["frame"] = "",
                    ["trigger"] = signature,
                    ["boundary"] = signature,
                },
                ["path"] = new List<object?>(),
                ["context"] = context,
            });
        }
        var batch = new Dictionary<string, object?>
        {
            ["version"] = 1L,
            ["batchId"] = batchId,
            ["appId"] = _appId,
            ["frames"] = frames,
            ["evidence"] = new List<object?>(),
        };
        if (_build != null)
        {
            batch["deployment"] = new Dictionary<string, object?> { ["version"] = _build };
        }
        return batch;
    }

    // The replayable capture object (`reproit debug replay-capture` input). Trailing effect
    // events are dropped first when the payload exceeds the context budget; a payload that
    // stays oversized with only start/return left is omitted entirely (null).
    internal static (Dictionary<string, object?>?, int) CapturePayload(
        CapturedOperation operation)
    {
        var events = operation.Events.ToList();
        var droppedEffects = 0;
        while (true)
        {
            var value = new Dictionary<string, object?>
            {
                ["format"] = CaptureFormat,
                ["version"] = (long)CaptureVersion,
                ["operation"] = operation.Operation,
                ["oracle"] = ServerErrorOracle,
                ["events"] = events,
            };
            if (Json.CanonicalUtf8(value).Length <= MaxCaptureJsonBytes)
            {
                return (value, droppedEffects);
            }
            var lastEffect = -1;
            for (var index = events.Count - 1; index >= 0; index--)
            {
                if (events[index].TryGetValue("kind", out var kind) &&
                    (kind as string) == "effect")
                {
                    lastEffect = index;
                    break;
                }
            }
            if (lastEffect < 0) return (null, droppedEffects);
            events.RemoveAt(lastEffect);
            droppedEffects++;
        }
    }

    private bool Send(Dictionary<string, object?> batch)
    {
        var body = Json.Canonical(batch);
        for (var attempt = 0; attempt <= _retryLimit; attempt++)
        {
            try
            {
                using var request = new HttpRequestMessage(HttpMethod.Post, _endpoint);
                request.Headers.Authorization =
                    new AuthenticationHeaderValue("Bearer", _apiKey);
                request.Content = new StringContent(body, Encoding.UTF8, "application/json");
                using var timeout = new CancellationTokenSource(_requestTimeoutMs);
                using var response = Client.Send(request, timeout.Token);
                if (response.IsSuccessStatusCode) return true;
                // A definitive client-side rejection cannot improve on retry.
                var status = (int)response.StatusCode;
                if (status >= 400 && status < 500) return false;
            }
            catch
            {
                // Network failure: retry below.
            }
            if (attempt < _retryLimit) Thread.Sleep(200 * attempt + 200);
        }
        return false;
    }

    private static long NowMs() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
}
