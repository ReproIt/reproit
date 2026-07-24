// ASP.NET Core middleware for reproit-backend-dotnet.
//
// Scan-time: inert unless the request carries `x-reproit-trace`; the finished trace is
// returned as the `x-reproit-events` response header. Production: pass a Capture and every
// request is traced and handed to the sampler instead. Handlers record observed effects via
// `httpContext.ReproitTrace()`. Every adapter path fails closed: instrumentation errors never
// reach the host app.
//
// Bodies are buffered up to a fixed cap so the start/return events carry the decoded JSON
// payloads; larger or non-JSON bodies are traced without content. The response is held in
// memory until the handler finishes (or the cap is hit) so the `x-reproit-events` header is
// complete before headers flush. Route values are matched after this middleware runs, so path
// parameters are not part of the canonical input here.

using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.Primitives;

namespace ReproitBackend;

public sealed class ReproitOptions
{
    public Capture? Capture { get; init; }
    public Func<HttpContext, string>? Operation { get; init; }
    public Func<HttpContext, string?>? Tenant { get; init; }
    public bool EffectsComplete { get; init; }
}

public static class ReproitAspNetCore
{
    public const int MaxBodyBytes = 64 * 1024;

    private const string ItemKey = "reproit";

    // app.UseReproit(new ReproitOptions { Capture = capture }); mount before the handlers.
    public static IApplicationBuilder UseReproit(
        this IApplicationBuilder app, ReproitOptions? options = null)
    {
        var resolved = options ?? new ReproitOptions();
        return app.Use(next => context => Invoke(context, next, resolved));
    }

    // The trace for the current request, or null when the adapter is inert.
    public static BackendTrace? ReproitTrace(this HttpContext context) =>
        context.Items.TryGetValue(ItemKey, out var trace) ? trace as BackendTrace : null;

    private static async Task Invoke(
        HttpContext http, RequestDelegate next, ReproitOptions options)
    {
        BackendTrace trace;
        TraceContext? scanContext;
        try
        {
            string? First(string name) =>
                http.Request.Headers.TryGetValue(name, out var value) && value.Count > 0
                    ? value[0] : null;
            scanContext = Reproit.TraceContextFromHeaders(First);
            var context = scanContext ?? options.Capture?.Context();
            if (context == null)
            {
                await next(http);
                return;
            }
            var operation = options.Operation != null
                ? options.Operation(http)
                : http.Request.Method + " " + http.Request.Path;
            trace = BackendTrace.Begin(context, operation, new BeginOptions
            {
                Tenant = options.Tenant?.Invoke(http),
                Input = Reproit.HttpInput(
                    body: await ReadJsonBody(http.Request),
                    query: Collapse(http.Request.Query.Select(
                        pair => (pair.Key, pair.Value))),
                    headers: Collapse(http.Request.Headers.Select(
                        pair => (pair.Key, pair.Value)))),
            });
            http.Items[ItemKey] = trace;
        }
        catch
        {
            // Fail closed: an instrumentation defect must not break the request.
            await next(http);
            return;
        }

        var originalBody = http.Response.Body;
        HoldStream hold = null!;
        hold = new HoldStream(originalBody, () => Finalize(outputKnown: false));
        http.Response.Body = hold;

        void Finalize(bool outputKnown)
        {
            try
            {
                if (trace.Finished) return;
                var status = http.Response.StatusCode;
                object? output = null;
                if (outputKnown && hold.Complete)
                {
                    var contentType = http.Response.ContentType ?? "";
                    output = DecodeJson(hold.Buffered, contentType, complete: true);
                }
                trace.Finish(output, status, status < 500, options.EffectsComplete);
                if (scanContext != null)
                {
                    if (!http.Response.HasStarted)
                    {
                        http.Response.Headers["x-reproit-events"] = trace.Header();
                    }
                }
                else
                {
                    options.Capture!.Record(trace);
                }
            }
            catch
            {
                // Oversized or over-long traces drop their header; the response ships.
            }
        }

        try
        {
            await next(http);
            Finalize(outputKnown: true);
        }
        catch
        {
            // Unhandled handler exception: the server will reset to a 500.
            http.Response.StatusCode = http.Response.HasStarted
                ? http.Response.StatusCode : StatusCodes.Status500InternalServerError;
            Finalize(outputKnown: false);
            throw;
        }
        finally
        {
            http.Response.Body = originalBody;
            await hold.ReleaseAsync();
        }
    }

    // Buffer and decode a JSON request body up to the cap; the buffered stream is rewound so
    // the handler still reads the full body (spooled to disk past the in-memory threshold).
    private static async Task<object?> ReadJsonBody(HttpRequest request)
    {
        var contentType = request.ContentType ?? "";
        request.EnableBuffering();
        var buffer = new MemoryStream();
        var chunk = new byte[8192];
        var complete = true;
        while (true)
        {
            var read = await request.Body.ReadAsync(chunk);
            if (read == 0) break;
            if (buffer.Length + read > MaxBodyBytes) complete = false;
            else buffer.Write(chunk, 0, read);
        }
        request.Body.Position = 0;
        return DecodeJson(buffer.ToArray(), contentType, complete);
    }

    private static object? DecodeJson(byte[] body, string contentType, bool complete)
    {
        if (!complete || body.Length == 0 || !contentType.Contains("application/json"))
        {
            return null;
        }
        try
        {
            return Json.Parse(System.Text.Encoding.UTF8.GetString(body));
        }
        catch
        {
            return null;
        }
    }

    // StringValues to canonical input values: single value as a string, repeated as a list.
    private static Dictionary<string, object?> Collapse(
        IEnumerable<(string Key, StringValues Values)> pairs)
    {
        var collapsed = new Dictionary<string, object?>();
        foreach (var (key, values) in pairs)
        {
            collapsed[key] = values.Count == 1
                ? values[0]
                : values.Select(value => (object?)value).ToList();
        }
        return collapsed;
    }

    // Holds response bytes in memory so the trace can finish (and the header attach) before
    // anything flushes. Past the cap the finalize callback runs with unknown output and the
    // stream switches to passthrough, bounding memory.
    private sealed class HoldStream : Stream
    {
        private readonly Stream _inner;
        private readonly Action _finalizeEarly;
        private readonly MemoryStream _buffer = new();
        private bool _passthrough;

        public bool Complete { get; private set; } = true;

        public byte[] Buffered => _buffer.ToArray();

        public HoldStream(Stream inner, Action finalizeEarly)
        {
            _inner = inner;
            _finalizeEarly = finalizeEarly;
        }

        // Flush buffered bytes to the real response body. Called once, after finalize.
        public async Task ReleaseAsync()
        {
            if (_passthrough) return;
            _passthrough = true;
            if (_buffer.Length > 0)
            {
                _buffer.Position = 0;
                await _buffer.CopyToAsync(_inner);
            }
        }

        public override void Write(byte[] buffer, int offset, int count)
        {
            if (_passthrough)
            {
                _inner.Write(buffer, offset, count);
                return;
            }
            if (_buffer.Length + count > MaxBodyBytes)
            {
                Complete = false;
                _finalizeEarly();
                var release = ReleaseAsync();
                release.GetAwaiter().GetResult();
                _inner.Write(buffer, offset, count);
                return;
            }
            _buffer.Write(buffer, offset, count);
        }

        public override async Task WriteAsync(
            byte[] buffer, int offset, int count, CancellationToken token)
        {
            if (_passthrough)
            {
                await _inner.WriteAsync(buffer.AsMemory(offset, count), token);
                return;
            }
            if (_buffer.Length + count > MaxBodyBytes)
            {
                Complete = false;
                _finalizeEarly();
                await ReleaseAsync();
                await _inner.WriteAsync(buffer.AsMemory(offset, count), token);
                return;
            }
            _buffer.Write(buffer, offset, count);
        }

        public override async ValueTask WriteAsync(
            ReadOnlyMemory<byte> buffer, CancellationToken token = default)
        {
            if (_passthrough)
            {
                await _inner.WriteAsync(buffer, token);
                return;
            }
            if (_buffer.Length + buffer.Length > MaxBodyBytes)
            {
                Complete = false;
                _finalizeEarly();
                await ReleaseAsync();
                await _inner.WriteAsync(buffer, token);
                return;
            }
            _buffer.Write(buffer.Span);
        }

        // While holding, a flush must not start the response; after release it passes through.
        public override void Flush()
        {
            if (_passthrough) _inner.Flush();
        }

        public override Task FlushAsync(CancellationToken token) =>
            _passthrough ? _inner.FlushAsync(token) : Task.CompletedTask;

        public override bool CanRead => false;
        public override bool CanSeek => false;
        public override bool CanWrite => true;
        public override long Length => throw new NotSupportedException();

        public override long Position
        {
            get => throw new NotSupportedException();
            set => throw new NotSupportedException();
        }

        public override int Read(byte[] buffer, int offset, int count) =>
            throw new NotSupportedException();

        public override long Seek(long offset, SeekOrigin origin) =>
            throw new NotSupportedException();

        public override void SetLength(long value) => throw new NotSupportedException();
    }
}
